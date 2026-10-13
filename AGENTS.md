# Earshelf — agent notes

Formerly "Audible Unlock" (v0.1.x). Keep the name free of "Audible": it is Amazon's trademark.

Personal macOS app: GPUI (Rust) frontend, Python JSON-lines backend, native Swift/WebKit login helper. See README.md for user-facing behavior.

## Layout

- `src/main.rs` — GPUI app: `Earshelf` state, `Operation`, `start_worker()` (spawns `uv run … backend/worker.py <action>`), event polling, queue/library logic, keyboard handling.
- `src/views.rs` — all rendering (sidebar, virtualized library list/grid with `uniform_list`, queue, settings, login/onboarding, context menu). Fixed row heights are required by `uniform_list`; keep `LIST_ROW`/`GRID_ROW` in sync with the row content.
- `src/settings.rs` — persisted settings (`~/Library/Application Support/Earshelf/settings.json`, invalid values fall back to defaults), disk space, Trash helper call, notifications (AppleScript; UserNotifications refuses child processes of ad-hoc signed apps), Dock badge, update check.
- `src/theme.rs` — light/dark palettes; never hard-code colors in views.
- `src/login_browser.rs` — lifecycle of the native login helper plus its offline WebKit tests.
- `native/Login.swift` — WKWebView sign-in window; captures the `/ap/maplanding` redirect.
- `native/OnePassword.swift` — 1Password CLI autofill (password + TOTP), item picker, self-tests.
- `backend/worker.py` — actions: `status`, `login`, `save`, `logout`, `library`, `fetch`, `convert` (the last two take a JSON options argument: format, layout, output_dir). One process per action; events are JSON lines on stdout; `{"kind":"cancel"}` on stdin cancels.
- `backend/test_worker.py` — Python tests (in-memory keyring, local HTTP server, real FFmpeg).
- `build.rs` compiles the Swift helper for arm64 and x86_64 with `xcrun swiftc` and merges them with `lipo`; `scripts/bundle.sh [debug|release]` builds and ad-hoc signs `dist/Earshelf.app` (release: universal app + `dist/Earshelf-macos-universal.zip` and `.sha256`).
- `scripts/fetch-uv.sh` downloads the checksum-pinned uv release for both architectures into `target/uv-<version>`; the app finds it first on PATH.
- `.github/workflows/ci.yml`: checks on every push/PR; on `v*` tags, a clean rebuild, build-provenance attestation, and GitHub release. Actions are pinned to commit SHAs; keep them pinned.
- `scripts/build-ffmpeg.sh` builds the bundled static FFmpeg/ffprobe into `target/ffmpeg-<version>`. The app puts its own `Contents/MacOS` first on the worker's PATH. Keep the build LGPL-only (never `--enable-gpl`/`--enable-nonfree`); if the worker starts using a new FFmpeg feature, enable that component there.

## Checks (run all before committing)

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
uv run --locked python -W error::ResourceWarning -m unittest discover -s backend -v
bash scripts/bundle.sh release   # verifies the signature itself
```

The WebKit tests open a real window and need a desktop session. No test hits Amazon; say so when a change touches live login, library, or download paths.

## Releases

1. Bump `Cargo.toml` (then `cargo update -p earshelf --offline`) and `resources/Info.plist` (`CFBundleShortVersionString`, increment `CFBundleVersion`); commit to `main`.
2. `git tag -a vX.Y.Z` with the release notes as the tag message (CI publishes them via `--notes-from-tag`), then `git push origin vX.Y.Z`.
3. CI fails the release if the tag doesn't match `CFBundleShortVersionString`.

Never upload release assets by hand: they would lack the provenance attestation. The asset name `Earshelf-macos-universal.zip` must stay stable (README install command). The app is ad-hoc signed only; there is no paid Developer ID or notarization.

## Invariants

- Never overwrite or delete user files. Book output and downloads are staged in a hidden `.earshelf-*` directory next to the target and published with `publish()` (exclusive `renamex_np`). Originals are never modified.
- Never log or emit secrets: passwords, OTPs, redirect URLs/authorization codes, activation bytes, voucher keys, Keychain contents, raw 1Password CLI output.
- Sign-in is embedded-only (WKWebView). No external-browser or copy/paste-redirect fallback.
- 1Password: fill only empty fields on the original HTTPS Amazon `/ap/` page; never submit, never fill SMS/email codes or CAPTCHA; only non-secret item IDs are persisted.
- Device credentials live in the macOS Keychain (`earshelf` / `audible-device`; the legacy `audible-unlock` entry is migrated on first read); activation bytes in `~/.audible/activation_bytes` (mode 0600, shared with other tools). Logout deregisters the device, then removes both.
- Python tests must never touch the real Keychain or `~/Downloads`; patch `DOWNLOADS`, `CONFIG`, `BYTES_FILE`, `COVERS`, `SUPPORT`, `HISTORY` and use the in-memory keyring.
- The Trash option runs only after a successful conversion, through the Swift helper (`--trash`), which accepts only `.aax`/`.aaxc`/`.voucher` regular files so `Put Back` works. Never `rm` a user's download.
- Per-book errors belong on the book's row (and the Queue); the bottom status line is for app-wide state only.
- Settings/`history.json` are read defensively: bad values fall back to defaults, never crash.
- Backend error messages are user-facing and fixed; unexpected exceptions map to a generic message.

## Conventions

- Pinned deps: GPUI `=0.2.2` (`font-kit` required for text), `audible==0.9.1`, `keyring==25.7.0`. Use `--locked` everywhere.
- Keep changes minimal; no new dependencies without a reason.
- Restarting the running app cancels an in-progress sign-in; say so before restarting.
