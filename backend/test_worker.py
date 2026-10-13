import contextlib
import http.server
import io
import json
import pathlib
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import MagicMock, patch

import keyring
import keyring.backend
import keyring.errors

import worker


class MemoryKeyring(keyring.backend.KeyringBackend):
    priority = 1

    def __init__(self):
        super().__init__()
        self.values = {}

    def get_password(self, service, username):
        return self.values.get((service, username))

    def set_password(self, service, username, password):
        self.values[service, username] = password

    def delete_password(self, service, username):
        if self.values.pop((service, username), None) is None:
            raise keyring.errors.PasswordDeleteError()


class WorkerTests(unittest.TestCase):
    def setUp(self):
        worker.CANCEL.clear()
        self.temp = tempfile.TemporaryDirectory()
        self.root = pathlib.Path(self.temp.name)
        self.config = patch.multiple(worker, CONFIG=self.root / "config", BYTES_FILE=self.root / "config/key",
                                     DOWNLOADS=self.root, COVERS=self.root / "covers",
                                     SUPPORT=self.root / "support", HISTORY=self.root / "support/history.json")
        self.config.start()
        self.previous_keyring = keyring.get_keyring()
        self.keyring = MemoryKeyring()
        keyring.set_keyring(self.keyring)

    def tearDown(self):
        worker.CANCEL.clear()
        self.config.stop()
        keyring.set_keyring(self.previous_keyring)
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

    def test_logout_deregisters_device_and_forgets_keychain_even_if_amazon_fails(self):
        for failure in [None, OSError("offline")]:
            worker.save_bytes("abcd1234")
            self.keyring.set_password(*worker.KEYCHAIN, "{}")
            auth = MagicMock()
            auth.deregister_device.side_effect = failure
            events = io.StringIO()
            with patch.object(worker, "load_auth", return_value=auth), contextlib.redirect_stdout(events):
                worker.logout()
            auth.deregister_device.assert_called_once()
            self.assertEqual(self.keyring.values, {})
            self.assertFalse(worker.BYTES_FILE.exists())
            kinds = [json.loads(line)["kind"] for line in events.getvalue().splitlines()]
            self.assertEqual(kinds, ["warning", "logged_out"] if failure else ["logged_out"])

    def test_logout_never_tests_the_authenticator_for_truthiness(self):
        class Authenticator:  # mirrors audible.Authenticator, whose __len__ raises RecursionError
            deregistered = False

            def __len__(self):
                raise RecursionError

            def deregister_device(self):
                self.deregistered = True

        auth = Authenticator()
        worker.save_bytes("abcd1234")
        self.keyring.set_password(*worker.KEYCHAIN, "{}")
        with patch.object(worker, "load_auth", return_value=auth), contextlib.redirect_stdout(io.StringIO()):
            worker.logout()
        self.assertTrue(auth.deregistered)
        self.assertEqual(self.keyring.values, {})

    def test_auth_round_trips_through_keychain(self):
        auth = MagicMock()
        auth.to_dict.return_value = {"adp_token": "synthetic"}
        worker.save_auth(auth)
        with patch("audible.Authenticator.from_dict", side_effect=lambda data: data):
            self.assertEqual(worker.load_auth(), {"adp_token": "synthetic"})
        self.assertTrue(worker.has_auth())
        worker.forget_auth()
        worker.forget_auth()
        with self.assertRaises(ValueError):
            worker.load_auth()

    def test_legacy_audible_unlock_keychain_entry_migrates(self):
        self.keyring.set_password(*worker.LEGACY_KEYCHAIN, '{"adp_token": "legacy"}')
        self.assertTrue(worker.has_auth())
        self.assertEqual(self.keyring.values, {worker.KEYCHAIN: '{"adp_token": "legacy"}'})
        self.keyring.set_password(*worker.LEGACY_KEYCHAIN, "stale")
        worker.forget_auth()
        self.assertEqual(self.keyring.values, {})

    def test_library_lists_only_downloadable_single_books_with_best_aax_codec(self):
        book = {"asin": "B000000001", "title": "Sourcery", "subtitle": "Discworld 5",
                "content_delivery_type": "SinglePartBook", "runtime_length_min": 400,
                "customer_rights": {"is_consumable_offline": True},
                "authors": [{"name": "Terry Pratchett"}],
                "available_codecs": [{"name": "aax_22_32", "enhanced_codec": "LC_32"},
                                     {"name": "aax_44_128", "enhanced_codec": "LC_128"},
                                     {"name": "mp4_44_128", "enhanced_codec": "MP4"},
                                     {"name": "aax_44_64", "enhanced_codec": "LC_64"}]}
        entry = worker.library_entry(book)
        self.assertEqual(entry, {"asin": "B000000001", "title": "Sourcery: Discworld 5", "codec": "LC_128",
                                 "cover": None, "authors": "Terry Pratchett", "author": "Terry Pratchett",
                                 "narrators": "", "series": "", "sequence": "", "purchased": "",
                                 "minutes": 400, "kbps": 128, "bytes": 400 * 60 * 128 * 1000 // 8})
        detailed = worker.library_entry({**book, "narrators": [{"name": "Nigel Planer"}],
                                         "series": [{"title": "Discworld", "sequence": "5"}],
                                         "purchase_date": "2025-03-04T10:00:00Z"})
        self.assertEqual((detailed["narrators"], detailed["series"], detailed["sequence"], detailed["purchased"]),
                         ("Nigel Planer", "Discworld", "5", "2025-03-04"))
        worker.COVERS.mkdir()
        (worker.COVERS / "B000000001.jpg").write_bytes(b"jpeg")
        self.assertEqual(worker.library_entry(book)["cover"], str(worker.COVERS / "B000000001.jpg"))
        self.assertIsNone(worker.library_entry({**book, "asin": "../../evil"}))
        self.assertIsNone(worker.library_entry({**book, "available_codecs": None})["codec"])
        self.assertIsNone(worker.library_entry({**book, "content_delivery_type": "PodcastParent"}))
        self.assertIsNone(worker.library_entry({**book, "customer_rights": {"is_consumable_offline": False}}))

    def test_download_publishes_complete_file_without_overwriting(self):
        payload = b"x" * (3 << 20)

        class Handler(http.server.BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                with contextlib.suppress(BrokenPipeError, ConnectionResetError):
                    self.wfile.write(payload)

            def log_message(self, *args):
                pass

        server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        url = f"http://127.0.0.1:{server.server_port}/book"
        target = self.root / "Book.aax"
        try:
            events = io.StringIO()
            with contextlib.redirect_stdout(events):
                worker.download(url, target)
            self.assertEqual(target.read_bytes(), payload)
            progress = [json.loads(line) for line in events.getvalue().splitlines()]
            self.assertEqual(progress[-1], {"kind": "download", "received": len(payload), "total": len(payload)})
            target.write_bytes(b"existing")
            with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(ValueError):
                worker.download(url, target)
            self.assertEqual(target.read_bytes(), b"existing")
            worker.CANCEL.set()
            with contextlib.redirect_stdout(io.StringIO()), self.assertRaises(worker.Cancelled):
                worker.download(url, self.root / "Other.aax")
            self.assertFalse((self.root / "Other.aax").exists())
            self.assertEqual(list(self.root.glob(".earshelf-*")), [])
        finally:
            server.shutdown()
            server.server_close()

    def test_covers_cache_only_https_images(self):
        responses = {
            "https://m.media-amazon.com/good.jpg": (200, "image/jpeg", b"jpeg"),
            "https://m.media-amazon.com/html": (200, "text/html", b"<html>"),
            "https://m.media-amazon.com/missing": (404, "image/jpeg", b""),
        }

        def fake_get(self, url):
            status, kind, body = responses[url]
            return MagicMock(status_code=status, headers={"content-type": kind}, content=body)

        events = io.StringIO()
        with patch("httpx.Client.get", fake_get), contextlib.redirect_stdout(events):
            worker.fetch_covers({"B000000001": "https://m.media-amazon.com/good.jpg",
                                 "B000000002": "https://m.media-amazon.com/html",
                                 "B000000003": "https://m.media-amazon.com/missing",
                                 "B000000004": "http://m.media-amazon.com/good.jpg"})
        path = worker.COVERS / "B000000001.jpg"
        self.assertEqual([json.loads(line) for line in events.getvalue().splitlines()],
                         [{"kind": "cover", "asin": "B000000001", "path": str(path)}])
        self.assertEqual(path.read_bytes(), b"jpeg")
        self.assertEqual(sorted(p.name for p in worker.COVERS.iterdir()), ["B000000001.jpg"])

    def test_aaxc_uses_private_voucher_and_rejects_bad_keys(self):
        book = self.root / "Book.aaxc"
        book.write_bytes(b"audio")
        with self.assertRaises(ValueError):
            worker.decryption(book)
        key, iv = "0" * 32, "f" * 32
        worker.write_voucher(book.with_suffix(".voucher"), {"key": key, "iv": iv})
        self.assertEqual(book.with_suffix(".voucher").stat().st_mode & 0o777, 0o600)
        self.assertEqual(worker.decryption(book), ["-audible_key", key, "-audible_iv", iv])
        with self.assertRaises(ValueError):
            worker.write_voucher(book.with_suffix(".voucher"), {"key": key, "iv": iv})
        book.with_suffix(".voucher").write_text(json.dumps({"key": "--version", "iv": iv}))
        with self.assertRaises(ValueError):
            worker.decryption(book)

    def test_fetch_rejects_invalid_identifiers_before_network(self):
        for value in ["", "../../etc", "b000000001", "B00000000", None]:
            with self.assertRaises(ValueError):
                worker.fetch(value)

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
        self.assertEqual(list(self.root.glob(".earshelf-*")), [])
        self.assertEqual(json.loads(events.getvalue().splitlines()[-1])["kind"], "done")

    def test_m4b_keeps_chapters_in_one_lossless_file_with_layout_and_history(self):
        source = self.make_book()
        worker.save_bytes("1234abcd")
        target = self.root / "Audiobooks"
        opts = worker.options(json.dumps({"format": "m4b", "layout": "author", "output_dir": str(target)}))
        meta = {"asin": "B000000001", "author": "Terry Pratchett", "series": "Discworld", "sequence": "13.0"}
        events = io.StringIO()
        with contextlib.redirect_stdout(events):
            worker.convert(str(source), opts, meta)
        output = target / "Terry Pratchett" / "Discworld" / "Book 13 - Demo_ Book.m4b"
        data = json.loads(subprocess.check_output([
            worker.tool("ffprobe"), "-v", "error", "-show_format", "-show_streams", "-show_chapters",
            "-of", "json", str(output)]))
        self.assertEqual(len(data["chapters"]), 2)
        self.assertEqual(data["format"]["tags"]["title"], "Demo: Book")
        self.assertIn("aac", [s["codec_name"] for s in data["streams"]])
        self.assertTrue(source.exists())
        self.assertEqual(list(target.glob(".earshelf-*")), [])
        done = json.loads(events.getvalue().splitlines()[-1])
        self.assertEqual((done["kind"], done["output"], done["source"]), ("done", str(output), str(source.resolve())))
        self.assertEqual(worker.read_history(), {"B000000001": str(output)})
        with self.assertRaises(ValueError), contextlib.redirect_stdout(io.StringIO()):
            worker.convert(str(source), opts, meta)

    def test_options_and_destinations(self):
        self.assertEqual(worker.options()["format"], "chapters")
        for bad in ('{"format": "mp3"}', '{"layout": "../x"}', '{"output_dir": ""}', "[]"):
            with self.assertRaises(ValueError):
                worker.options(bad)
        root = pathlib.Path("/lib")
        self.assertEqual(worker.destination_for(root, "flat", "T", "A", "S", "1", ".m4b"), root / "T.m4b")
        self.assertEqual(worker.destination_for(root, "author", "T", "A", "", "", ""), root / "A" / "T")
        self.assertEqual(worker.destination_for(root, "author", "T", "A", "S", "2.5", ""), root / "A" / "S" / "Book 2.5 - T")
        self.assertEqual(worker.destination_for(root, "author", "T", "../..", "S", "nan", "").parent, root / "_" / "S")

    def test_library_remembers_converted_and_downloaded_books(self):
        entry = {"asin": "B000000001", "title": "Demo: Book"}
        self.assertEqual((worker.annotate(entry)["output"], worker.annotate(entry)["source"]), (None, None))
        (self.root / "Demo_ Book.aax").write_bytes(b"x")
        (self.root / "Demo_ Book").mkdir()
        found = worker.annotate(entry)
        self.assertEqual((found["output"], found["source"]), (str(self.root / "Demo_ Book"), str(self.root / "Demo_ Book.aax")))
        moved = self.root / "Elsewhere.m4b"
        moved.write_bytes(b"x")
        worker.record("B000000001", moved)
        self.assertEqual(worker.annotate(entry)["output"], str(moved))
        moved.unlink()
        self.assertEqual(worker.annotate(entry)["output"], str(self.root / "Demo_ Book"))

    def test_bundled_minimal_ffmpeg_converts(self):
        bundled = pathlib.Path(__file__).parents[1] / "target/ffmpeg-9.0.2-universal/bin"
        if not (bundled / "ffmpeg").is_file():
            self.skipTest("run scripts/build-ffmpeg.sh first")
        source = self.make_book()
        worker.save_bytes("1234abcd")
        with patch.object(worker, "tool", lambda name: str(bundled / name)), \
                contextlib.redirect_stdout(io.StringIO()):
            worker.convert(str(source))
        chapters = sorted((self.root / "Demo_ Book").glob("*.m4a"))
        self.assertEqual(len(chapters), 2)
        streams = json.loads(subprocess.check_output([
            str(bundled / "ffprobe"), "-v", "error", "-show_streams", "-of", "json", str(chapters[0])]))["streams"]
        self.assertEqual(sorted(s["codec_name"] for s in streams), ["aac", "mjpeg"])

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
        self.assertEqual(list(self.root.glob(".earshelf-*")), [])
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
