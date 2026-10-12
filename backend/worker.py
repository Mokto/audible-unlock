import contextlib
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
DOWNLOADS = pathlib.Path.home() / "Downloads"
COVERS = pathlib.Path.home() / "Library/Caches/audible-unlock/covers"
KEYCHAIN = ("audible-unlock", "audible-device")
LIBRARY_GROUPS = "contributors,customer_rights,media,product_attrs"
DOWNLOAD_AGENT = "Audible/671 CFNetwork/1240.0.4 Darwin/20.6.0"
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


def save_auth(auth):
    import keyring

    keyring.set_password(*KEYCHAIN, json.dumps(auth.to_dict()))


def load_auth():
    import audible
    import keyring

    raw = keyring.get_password(*KEYCHAIN)
    if not raw:
        raise ValueError("Log out and log in with Amazon again to browse your library. "
                         "Pasted activation bytes cannot download books.")
    return audible.Authenticator.from_dict(json.loads(raw))


def has_auth():
    import keyring

    try:
        return bool(keyring.get_password(*KEYCHAIN))
    except Exception:
        return False


def forget_auth():
    import keyring
    import keyring.errors

    try:
        keyring.delete_password(*KEYCHAIN)
    except keyring.errors.PasswordDeleteError:
        pass


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
    if path.suffix.lower() not in (".aax", ".aaxc") or not path.is_file():
        raise ValueError("Choose an .aax or .aaxc audiobook.")
    return path


def decryption(path):
    if path.suffix.lower() == ".aax":
        return ["-activation_bytes", load_bytes()]
    try:
        voucher = json.loads(path.with_suffix(".voucher").read_text())
        key, iv = voucher["key"], voucher["iv"]
    except (OSError, ValueError, KeyError, TypeError):
        raise ValueError("AAXC files need the .voucher file saved next to them when downloaded.") from None
    if not all(isinstance(value, str) and re.fullmatch(r"[0-9a-fA-F]{32}", value) for value in (key, iv)):
        raise ValueError("The .voucher file next to this AAXC file is invalid.")
    return ["-audible_key", key, "-audible_iv", iv]


def probe(path, key):
    raw = run(
        [tool("ffprobe"), "-v", "error", *key, "-show_format",
         "-show_chapters", "-show_streams", "-of", "json", str(path)],
        "Unable to read this audiobook. Check that it belongs to the connected account.",
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


def publish(source, destination, message="The output folder already exists. Nothing was overwritten."):
    # macOS renamex_np refuses existing destinations, including an empty directory.
    library = ctypes.CDLL(None, use_errno=True)
    rename = library.renamex_np
    rename.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint]
    rename.restype = ctypes.c_int
    if rename(os.fsencode(source), os.fsencode(destination), 0x4) != 0:
        code = ctypes.get_errno()
        if code in (errno.EEXIST, errno.ENOTEMPTY):
            raise ValueError(message)
        raise OSError(code, os.strerror(code))


def convert(value):
    path = source_path(value)
    key = decryption(path)
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
                run([ffmpeg, "-nostdin", "-v", "error", *key,
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
            args = [ffmpeg, "-nostdin", "-v", "error", *key,
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
    try:
        save_auth(auth)
    except Exception:
        with contextlib.suppress(Exception):
            auth.deregister_device()
        emit("warning", message="Logged in, but the Keychain refused the Audible sign-in. "
                                "Converting works; the library needs another login.")
    emit("connected", library=has_auth())


def logout():
    check_cancel()
    try:
        auth = load_auth()
    except Exception:
        auth = None
    if auth:
        try:
            auth.deregister_device()
        except Exception:
            emit("warning", message="Amazon could not remove this app's device registration. "
                                    "Remove it under Amazon's Content and Devices settings.")
    BYTES_FILE.unlink(missing_ok=True)
    forget_auth()
    emit("logged_out")


def best_codec(item):
    codecs = [c for c in item.get("available_codecs") or [] if re.fullmatch(r"aax_\d+_\d+", c.get("name", ""))]
    best = max(codecs, key=lambda c: tuple(map(int, c["name"][4:].split("_"))), default=None)
    return best and best.get("enhanced_codec")


def library_entry(item):
    # ponytail: MultiPartBook/podcasts skipped; listing their parts needs per-parent child requests.
    if item.get("content_delivery_type") != "SinglePartBook":
        return None
    if not (item.get("customer_rights") or {}).get("is_consumable_offline"):
        return None
    if not re.fullmatch(r"[A-Z0-9]{10}", item.get("asin") or ""):
        return None
    title = item.get("title") or item["asin"]
    if item.get("subtitle"):
        title += ": " + item["subtitle"]
    cover = COVERS / f"{item['asin']}.jpg"
    return {
        "asin": item["asin"], "title": title, "codec": best_codec(item),
        "cover": str(cover) if cover.is_file() else None,
        "authors": ", ".join(a["name"] for a in item.get("authors") or [] if a.get("name")),
        "minutes": item.get("runtime_length_min") or 0,
    }


def library():
    import audible

    auth = load_auth()
    books, urls, page = [], {}, 1
    with audible.Client(auth, timeout=30) as client:
        while True:
            check_cancel()
            items = client.get("library", num_results=1000, page=page, response_groups=LIBRARY_GROUPS,
                               image_sizes="252", sort_by="-PurchaseDate")["items"]
            for item in items:
                entry = library_entry(item)
                if entry:
                    books.append(entry)
                    images = item.get("product_images") or {}
                    url = images.get("252") or next(iter(images.values()), None)
                    if not entry["cover"] and url:
                        urls[entry["asin"]] = url
            if len(items) < 1000:
                break
            page += 1
    emit("library", books=books)
    fetch_covers(urls)


def fetch_covers(urls):
    import concurrent.futures
    import httpx

    COVERS.mkdir(parents=True, exist_ok=True)

    def fetch(asin, url):
        if urllib.parse.urlsplit(url).scheme != "https":
            return None
        response = client.get(url)
        if (response.status_code != 200 or not response.headers.get("content-type", "").startswith("image/")
                or not 0 < len(response.content) <= 4 << 20):
            return None
        with tempfile.NamedTemporaryFile(dir=COVERS, delete=False) as file:
            file.write(response.content)
        path = COVERS / f"{asin}.jpg"
        os.replace(file.name, path)
        return path

    with httpx.Client(timeout=20, follow_redirects=True) as client, \
            concurrent.futures.ThreadPoolExecutor(8) as pool:
        jobs = {pool.submit(fetch, asin, url): asin for asin, url in urls.items()}
        try:
            for job in concurrent.futures.as_completed(jobs):
                check_cancel()
                with contextlib.suppress(Exception):
                    if path := job.result():
                        emit("cover", asin=jobs[job], path=str(path))
        finally:
            for job in jobs:
                job.cancel()


def https_url(value):
    if not isinstance(value, str) or urllib.parse.urlsplit(value).scheme != "https":
        raise ValueError("Audible returned an invalid download link.")
    return value


def aax_url(client, auth, asin, codec):
    response = client.session.head(
        "https://cde-ta-g7g.amazon.com/FionaCDEServiceEngine/FSDownloadContent",
        params={"type": "AUDI", "currentTransportMethod": "WIFI", "key": asin, "codec": codec},
        follow_redirects=False,
    )
    link = response.headers.get("location")
    if not link:
        return None
    return https_url(link.replace("cds.audible.com", f"cds.audible.{auth.locale.domain}"))


def aaxc_license(client, auth, asin):
    from audible.aescipher import decrypt_voucher_from_licenserequest

    response = client.post(f"content/{asin}/licenserequest", body={
        "supported_drm_types": ["Adrm"], "quality": "High", "consumption_type": "Download",
        "response_groups": "content_reference",
    })
    content = response["content_license"]
    if content.get("status_code") != "Granted":
        raise ValueError("Audible refused a download license for this title.")
    voucher = decrypt_voucher_from_licenserequest(auth, response)
    return https_url(content["content_metadata"]["content_url"]["offline_url"]), {
        "key": voucher["key"], "iv": voucher["iv"]}


def download(url, target):
    import httpx

    with tempfile.TemporaryDirectory(prefix=".audible-unlock-", dir=target.parent) as temp:
        part = pathlib.Path(temp) / target.name
        received, shown = 0, -1
        with httpx.stream("GET", url, headers={"User-Agent": DOWNLOAD_AGENT},
                          follow_redirects=True, timeout=60) as response:
            response.raise_for_status()
            total = int(response.headers.get("content-length") or 0)
            with part.open("wb") as file:
                for chunk in response.iter_bytes(1 << 20):
                    check_cancel()
                    file.write(chunk)
                    received += len(chunk)
                    percent = received * 100 // total if total else -1
                    if percent != shown:
                        shown = percent
                        emit("download", received=received, total=total)
        if not received or (total and received != total):
            raise ValueError("The download was incomplete. Nothing was saved.")
        check_cancel()
        publish(part, target, "A file with this name already exists. Nothing was overwritten.")


def fetch(asin):
    import audible

    if not re.fullmatch(r"[A-Z0-9]{10}", asin or ""):
        raise ValueError("Invalid Audible title identifier.")
    auth = load_auth()
    with audible.Client(auth, timeout=30) as client:
        entry = library_entry(client.get(f"library/{asin}", response_groups=LIBRARY_GROUPS)["item"])
        if not entry:
            raise ValueError("Audible does not allow downloading this title.")
        DOWNLOADS.mkdir(exist_ok=True)
        stem = safe_name(entry["title"])
        existing = [p for p in (DOWNLOADS / f"{stem}.aax", DOWNLOADS / f"{stem}.aaxc") if p.is_file()]
        if existing:
            target = existing[0]
            emit("warning", message=f"Using the existing download {target.name}.")
        else:
            emit("download", received=0, total=0)
            url = entry["codec"] and aax_url(client, auth, asin, entry["codec"])
            target = DOWNLOADS / f"{stem}.aax"
            if not url:
                url, voucher = aaxc_license(client, auth, asin)
                target = DOWNLOADS / f"{stem}.aaxc"
                write_voucher(target.with_suffix(".voucher"), voucher)
            try:
                download(url, target)
            except BaseException:
                if target.suffix == ".aaxc":
                    target.with_suffix(".voucher").unlink(missing_ok=True)
                raise
    emit("downloaded", path=str(target))
    convert(str(target))


def write_voucher(path, voucher):
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    except FileExistsError:
        raise ValueError(f"{path.name} already exists. Nothing was overwritten.") from None
    with os.fdopen(descriptor, "w") as file:
        json.dump(voucher, file)


def status():
    ready = False
    try:
        load_bytes()
        ready = True
    except (ValueError, OSError):
        pass
    emit("status", connected=ready, library=ready and has_auth(), ffmpeg=bool(shutil.which("ffmpeg")),
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
        elif action == "library":
            library()
        elif action == "fetch":
            fetch(sys.argv[2])
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
    except Exception:
        emit("error", message="The Audible request failed. Check your connection and try again.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
