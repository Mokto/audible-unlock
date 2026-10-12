import ctypes
import errno
import json
import math
import os
import pathlib
import queue
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import urllib.parse

CONFIG = pathlib.Path.home() / ".audible"
BYTES_FILE = CONFIG / "activation_bytes"
CANCEL = threading.Event()
INPUTS = queue.Queue()


class Cancelled(Exception):
    pass


def emit(kind, **data):
    print(json.dumps({"kind": kind, **data}), flush=True)


def read_inputs():
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except (ValueError, TypeError):
            continue
        if not isinstance(message, dict):
            continue
        if message.get("kind") == "cancel":
            CANCEL.set()
        else:
            INPUTS.put(message)
    CANCEL.set()


def check_cancel():
    if CANCEL.is_set():
        raise Cancelled()


def activation_bytes(value):
    if not isinstance(value, str):
        raise ValueError("Activation bytes must be text.")
    value = value.strip()
    if not re.fullmatch(r"[0-9a-fA-F]{8}", value):
        raise ValueError("Activation bytes must be exactly eight hexadecimal characters.")
    return value.lower()


def save_bytes(value):
    value = activation_bytes(value)
    CONFIG.mkdir(mode=0o700, parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(mode="w", dir=CONFIG, delete=False) as file:
        temp = pathlib.Path(file.name)
        file.write(value + "\n")
    try:
        temp.chmod(0o600)
        temp.replace(BYTES_FILE)
    finally:
        temp.unlink(missing_ok=True)


def load_bytes():
    if not BYTES_FILE.is_file():
        raise ValueError("Connect to Audible or paste activation bytes before converting.")
    return activation_bytes(BYTES_FILE.read_text())


def tool(name):
    found = shutil.which(name)
    if not found:
        raise ValueError(f"{name} is missing. Install it with: brew install ffmpeg")
    return found


def run(args, message):
    check_cancel()
    with tempfile.TemporaryFile() as errors:
        process = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=errors)
        try:
            while True:
                try:
                    stdout, _ = process.communicate(timeout=0.2)
                    break
                except subprocess.TimeoutExpired:
                    check_cancel()
            check_cancel()
            if process.returncode:
                raise ValueError(message)
            return stdout
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            process.stdout.close()


def safe_name(value):
    value = re.sub(r'[\x00-\x1f\x7f/\\:*?"<>|]', "_", str(value))
    value = re.sub(r"\s+", " ", value).strip(" .")
    return value.encode("utf-8")[:120].decode("utf-8", errors="ignore").strip(" .") or "Untitled"


def source_path(value):
    path = pathlib.Path(value).expanduser().resolve(strict=True)
    if path.suffix.lower() != ".aax" or not path.is_file():
        raise ValueError("Choose an .aax file. AAXC needs different keys and is not supported.")
    return path


def probe(path, key):
    raw = run(
        [tool("ffprobe"), "-v", "error", "-activation_bytes", key, "-show_format",
         "-show_chapters", "-show_streams", "-of", "json", str(path)],
        "Unable to read this AAX file. Check that it belongs to the connected account.",
    )
    data = json.loads(raw)
    chapters = data.get("chapters", [])
    if not chapters:
        raise ValueError("The file contains no chapters.")
    duration = float(data["format"]["duration"])
    for chapter in chapters:
        start, end = float(chapter["start_time"]), float(chapter["end_time"])
        if not all(map(math.isfinite, [start, end, duration])) or not 0 <= start < end <= duration + 0.1:
            raise ValueError("The file contains invalid chapter timestamps.")
    if not any(stream.get("codec_type") == "audio" for stream in data.get("streams", [])):
        raise ValueError("The file contains no audio.")
    tags = data["format"].get("tags", {})
    title = tags.get("title") or path.stem
    return data, title, tags


def publish(source, destination):
    # macOS renamex_np refuses existing destinations, including an empty directory.
    library = ctypes.CDLL(None, use_errno=True)
    rename = library.renamex_np
    rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    if rename(os.fsencode(source), os.fsencode(destination), 0x4) != 0:
        code = ctypes.get_errno()
        if code in (errno.EEXIST, errno.ENOTEMPTY):
            raise ValueError("The output folder already exists. Nothing was overwritten.")
        raise OSError(code, os.strerror(code))


def convert(value):
    path = source_path(value)
    key = load_bytes()
    data, title, tags = probe(path, key)
    chapters = data["chapters"]
    destination = path.parent / safe_name(title)
    if destination.exists() or destination.is_symlink():
        raise ValueError("The output folder already exists. Nothing was overwritten.")
    emit("book", title=title, total=len(chapters))
    with tempfile.TemporaryDirectory(prefix=".audible-unlock-", dir=path.parent) as temp:
        stage = pathlib.Path(temp) / "book"
        stage.mkdir()
        cover = pathlib.Path(temp) / "cover.jpg"
        has_cover = any(s.get("codec_type") == "video" for s in data.get("streams", []))
        ffmpeg = tool("ffmpeg")
        if has_cover:
            try:
                run([ffmpeg, "-nostdin", "-v", "error", "-activation_bytes", key,
                     "-i", str(path), "-map", "0:v:0", "-frames:v", "1", str(cover)],
                    "Unable to extract cover art.")
            except ValueError:
                has_cover = False
                emit("warning", message="Cover art could not be extracted; audio will still be converted.")
        digits = len(str(len(chapters)))
        for index, chapter in enumerate(chapters, 1):
            check_cancel()
            chapter_title = chapter.get("tags", {}).get("title") or f"Chapter {index}"
            start = float(chapter["start_time"])
            duration = float(chapter["end_time"]) - start
            output = stage / f"{index:0{digits}d} - {safe_name(chapter_title)}.m4a"
            args = [ffmpeg, "-nostdin", "-v", "error", "-activation_bytes", key,
                    "-ss", str(start), "-i", str(path)]
            if has_cover:
                args += ["-i", str(cover)]
            args += ["-t", str(duration), "-map", "0:a:0", "-c:a", "copy",
                     "-map_metadata", "-1", "-map_chapters", "-1"]
            if has_cover:
                args += ["-map", "1:v:0", "-c:v", "copy", "-disposition:v:0", "attached_pic"]
            for name, value in {
                "title": chapter_title, "artist": tags.get("artist") or tags.get("author", ""),
                "album": title, "track": f"{index}/{len(chapters)}",
                "genre": tags.get("genre", "Audiobook"), "date": tags.get("date", ""),
            }.items():
                args += ["-metadata", f"{name}={value}"]
            run(args + [str(output)], f"Chapter {index} failed. No completed output was published.")
            if not output.is_file() or output.stat().st_size == 0:
                raise ValueError(f"Chapter {index} produced no output.")
            emit("progress", completed=index, total=len(chapters), chapter=chapter_title)
        check_cancel()
        publish(stage, destination)
    emit("done", output=str(destination), title=title)


def validate_redirect(value, login_url):
    if not isinstance(value, str):
        raise ValueError("The redirect URL must be text.")
    if len(value) > 32768:
        raise ValueError("The redirect URL is too long.")
    target = urllib.parse.urlsplit(value.strip())
    expected = urllib.parse.urlsplit(
        urllib.parse.parse_qs(urllib.parse.urlsplit(login_url).query)["openid.return_to"][0]
    )
    codes = urllib.parse.parse_qs(target.query).get("openid.oa2.authorization_code", [])
    if (target.scheme != "https" or target.netloc != expected.netloc
            or target.path != expected.path or len(codes) != 1 or not codes[0]):
        raise ValueError("The captured Amazon sign-in redirect is invalid.")
    return value.strip()


def login(locale):
    import audible

    def callback(url):
        emit("login_url", url=url)
        while True:
            check_cancel()
            try:
                message = INPUTS.get(timeout=0.2)
            except queue.Empty:
                continue
            if message.get("kind") != "redirect":
                continue
            try:
                return validate_redirect(message.get("value", ""), url)
            except (ValueError, KeyError):
                emit("warning", message="The sign-in result was invalid. Reopen the sign-in window and try again.")

    try:
        auth = audible.Authenticator.from_login_external(locale=locale, login_url_callback=callback)
        check_cancel()
        key = auth.get_activation_bytes()
        check_cancel()
        save_bytes(key)
    except Cancelled:
        raise
    except Exception:
        raise ValueError("Audible login failed. Check the marketplace and try connecting again.") from None
    emit("connected")


def logout():
    check_cancel()
    BYTES_FILE.unlink(missing_ok=True)
    emit("logged_out")


def status():
    ready = False
    try:
        load_bytes()
        ready = True
    except (ValueError, OSError):
        pass
    emit("status", connected=ready, ffmpeg=bool(shutil.which("ffmpeg")),
         ffprobe=bool(shutil.which("ffprobe")))


def main():
    threading.Thread(target=read_inputs, daemon=True).start()
    try:
        action = sys.argv[1]
        if action == "status":
            status()
        elif action == "logout":
            logout()
        elif action == "convert":
            convert(sys.argv[2])
        elif action == "login":
            login(sys.argv[2])
        elif action == "save":
            while True:
                check_cancel()
                try:
                    value = INPUTS.get(timeout=0.2)
                    break
                except queue.Empty:
                    pass
            save_bytes(value.get("value", ""))
            emit("connected")
        else:
            raise ValueError("Unknown operation.")
    except Cancelled:
        emit("cancelled")
        return 2
    except (ValueError, OSError, KeyError, TypeError) as error:
        emit("error", message=str(error))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
