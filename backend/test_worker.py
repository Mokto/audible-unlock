import contextlib
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

import worker


class WorkerTests(unittest.TestCase):
    def setUp(self):
        worker.CANCEL.clear()
        self.temp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp.name)
        self.config = patch.multiple(worker, CONFIG=self.root / "config", BYTES_FILE=self.root / "config/key")
        self.config.start()

    def tearDown(self):
        worker.CANCEL.clear()
        self.config.stop()
        self.temp.cleanup()

    def test_key_validation_and_permissions(self):
        worker.save_bytes(" ABCD1234\n")
        self.assertEqual(worker.load_bytes(), "abcd1234")
        self.assertEqual(worker.BYTES_FILE.stat().st_mode & 0o777, 0o600)
        for value in ["", "abcdefg1", "123456789", "../12345", None]:
            with self.assertRaises(ValueError):
                worker.save_bytes(value)
        self.assertEqual(worker.load_bytes(), "abcd1234")

    def test_logout_forgets_key_and_preserves_other_files(self):
        worker.save_bytes("abcd1234")
        other_auth = worker.CONFIG / "other-auth.json"
        other_auth.write_text("unrelated authentication")
        book = self.root / "book.aax"
        book.write_bytes(b"original audio")
        with contextlib.redirect_stdout(io.StringIO()):
            worker.logout()
            worker.logout()
        self.assertFalse(worker.BYTES_FILE.exists())
        self.assertEqual(other_auth.read_text(), "unrelated authentication")
        self.assertEqual(book.read_bytes(), b"original audio")
        events = io.StringIO()
        with contextlib.redirect_stdout(events):
            worker.status()
        self.assertFalse(json.loads(events.getvalue())["connected"])

    def test_failed_logout_preserves_key(self):
        worker.save_bytes("abcd1234")
        with patch.object(pathlib.Path, "unlink", side_effect=PermissionError("denied")):
            with self.assertRaises(PermissionError):
                worker.logout()
        self.assertEqual(worker.load_bytes(), "abcd1234")

    def test_redirect_validation(self):
        login_url = "https://www.amazon.co.uk/ap/signin?openid.return_to=https%3A%2F%2Fwww.amazon.co.uk%2Fap%2Fmaplanding"
        redirect = "https://www.amazon.co.uk/ap/maplanding?openid.oa2.authorization_code=example"
        self.assertEqual(worker.validate_redirect(redirect, login_url), redirect)
        for url in [redirect.replace("https", "http"), redirect.replace(".co.uk", ".co.uk.evil.com"),
                    redirect.replace("/maplanding", "/signin"), redirect.split("?")[0],
                    redirect + "&openid.oa2.authorization_code=second", None]:
            with self.assertRaises(ValueError):
                worker.validate_redirect(url, login_url)

    def test_names_cannot_escape_output_directory(self):
        for value in ["../", "", "\x00", "\\", " . ", "/tmp/book", "x" * 200, "📚" * 120]:
            name = worker.safe_name(value)
            self.assertNotIn("/", name)
            self.assertNotIn("\\", name)
            self.assertNotIn(name, ["", ".", ".."])
            self.assertLessEqual(len(name.encode("utf-8")), 120)

    def test_publish_never_replaces_existing_directory(self):
        stage = self.root / "stage"
        stage.mkdir()
        (stage / "chapter").write_text("audio")
        output = self.root / "output"
        output.mkdir()
        with self.assertRaises(ValueError):
            worker.publish(stage, output)
        self.assertEqual(list(output.iterdir()), [])
        self.assertTrue((stage / "chapter").exists())
        output.rmdir()
        worker.publish(stage, output)
        self.assertEqual((output / "chapter").read_text(), "audio")

    def test_cancel_terminates_child(self):
        timer = threading.Timer(0.15, worker.CANCEL.set)
        timer.start()
        started = time.monotonic()
        try:
            with self.assertRaises(worker.Cancelled):
                worker.run([sys.executable, "-c", "import time; time.sleep(30)"], "failed")
            self.assertLess(time.monotonic() - started, 3)
        finally:
            timer.join()

    def make_book(self):
        metadata = self.root / "chapters.txt"
        metadata.write_text(
            ";FFMETADATA1\ntitle=Demo: Book\nartist=Test Author\n"
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=600\ntitle=First\n"
            "[CHAPTER]\nTIMEBASE=1/1000\nSTART=600\nEND=1200\ntitle=Second\n"
        )
        cover = self.root / "cover.jpg"
        subprocess.run([worker.tool("ffmpeg"), "-v", "error", "-f", "lavfi", "-i",
                        "color=c=red:s=32x32", "-frames:v", "1", str(cover)], check=True)
        source = self.root / "input.aax"
        subprocess.run([worker.tool("ffmpeg"), "-v", "error", "-f", "lavfi", "-i",
                        "sine=frequency=440:duration=1.2", "-i", str(metadata), "-i", str(cover),
                        "-map", "0:a", "-map", "2:v", "-map_metadata", "1", "-map_chapters", "1",
                        "-c:a", "aac", "-c:v", "copy", "-disposition:v", "attached_pic",
                        "-f", "mp4", str(source)], check=True)
        return source

    def test_real_ffmpeg_conversion_and_no_overwrite(self):
        source = self.make_book()
        original = source.read_bytes()
        worker.save_bytes("1234abcd")
        events = io.StringIO()
        with contextlib.redirect_stdout(events):
            worker.convert(str(source))
        output = self.root / "Demo_ Book"
        chapters = sorted(output.glob("*.m4a"))
        self.assertEqual(len(chapters), 2)
        for index, chapter in enumerate(chapters, 1):
            data = json.loads(subprocess.check_output([
                worker.tool("ffprobe"), "-v", "error", "-show_format", "-show_streams",
                "-show_chapters", "-of", "json", str(chapter),
            ]))
            self.assertEqual(data["format"]["tags"]["track"], f"{index}/2")
            self.assertEqual(data["format"]["tags"]["album"], "Demo: Book")
            self.assertEqual(data["chapters"], [])
            self.assertTrue(any(s.get("disposition", {}).get("attached_pic") for s in data["streams"]))
            self.assertTrue(0.5 < float(data["format"]["duration"]) < 0.8)
        saved = [chapter.read_bytes() for chapter in chapters]
        with self.assertRaises(ValueError):
            worker.convert(str(source))
        self.assertEqual([chapter.read_bytes() for chapter in chapters], saved)
        self.assertEqual(source.read_bytes(), original)
        self.assertEqual(list(self.root.glob(".audible-unlock-*")), [])
        self.assertEqual(json.loads(events.getvalue().splitlines()[-1])["kind"], "done")

    def test_failed_conversion_cleans_staging(self):
        source = self.make_book()
        worker.save_bytes("1234abcd")
        original_run = worker.run

        def fail_chapter(args, message):
            if "-c:a" in args:
                raise ValueError("simulated chapter failure")
            return original_run(args, message)

        with patch.object(worker, "run", side_effect=fail_chapter), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaises(ValueError):
                worker.convert(str(source))
        self.assertFalse((self.root / "Demo_ Book").exists())
        self.assertEqual(list(self.root.glob(".audible-unlock-*")), [])
        self.assertTrue(source.exists())

    def test_external_login_generates_browser_url_without_network(self):
        import audible

        def callback(url):
            self.assertTrue(url.startswith("https://www.amazon.co.uk/ap/signin?"))
            self.assertIn("openid.oa2.code_challenge=", url)
            raise worker.Cancelled()

        with self.assertRaises(worker.Cancelled):
            audible.Authenticator.from_login_external(locale="uk", login_url_callback=callback)


if __name__ == "__main__":
    unittest.main()
