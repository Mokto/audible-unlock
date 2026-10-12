# Audible Unlock — agent notes

Personal macOS app: GPUI (Rust) frontend, Python JSON-lines backend, native Swift/WebKit login helper. See README.md for user-facing behavior.

## Layout

- `src/main.rs` — GPUI app: `Unlock` state, `Operation`, `start_worker()` (spawns `uv run … backend/worker.py <action>`), queue, library, render.
- `src/login_browser.rs` — lifecycle of the native login helper plus its offline WebKit tests.
- `native/Login.swift` — WKWebView sign-in window; captures the `/ap/maplanding` redirect.
- `native/OnePassword.swift` — 1Password CLI autofill (password + TOTP), item picker, self-tests.
- `backend/worker.py` — actions: `status`, `login`, `save`, `logout`, `library`, `fetch`, `convert`. One process per action; events are JSON lines on stdout; `{"kind":"cancel"}` on stdin cancels.
- `backend/test_worker.py` — Python tests (in-memory keyring, local HTTP server, real FFmpeg).
- `build.rs` compiles the Swift helper with `xcrun swiftc`; `scripts/bundle.sh` builds and ad-hoc signs `dist/Audible Unlock.app`.
- `scripts/build-ffmpeg.sh` builds the bundled static FFmpeg/ffprobe into `target/ffmpeg-<version>`. The app puts its own `Contents/MacOS` first on the worker's PATH. Keep the build LGPL-only (never `--enable-gpl`/`--enable-nonfree`); if the worker starts using a new FFmpeg feature, enable that component there.

## Checks (run all before committing)

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
uv run --locked python -W error::ResourceWarning -m unittest discover -s backend -v
bash scripts/bundle.sh && codesign --verify --deep --strict "dist/Audible Unlock.app"
```

The WebKit tests open a real window and need a desktop session. No test hits Amazon; say so when a change touches live login, library, or download paths.

## Releases

Bump `Cargo.toml` (then `cargo update -p audible-unlock --offline`) and `resources/Info.plist` (`CFBundleShortVersionString`, increment `CFBundleVersion`). Build with `bash scripts/bundle.sh release`, zip with `ditto -c -k --norsrc --noextattr --noacl --keepParent`, and upload the asset as `Audible-Unlock-macos-arm64.zip`. The README install command depends on that stable name. The app is ad-hoc signed only; there is no paid Developer ID or notarization.

## Invariants

- Never overwrite or delete user files. Book output and downloads are staged in a hidden `.audible-unlock-*` directory next to the target and published with `publish()` (exclusive `renamex_np`). Originals are never modified.
- Never log or emit secrets: passwords, OTPs, redirect URLs/authorization codes, activation bytes, voucher keys, Keychain contents, raw 1Password CLI output.
- Sign-in is embedded-only (WKWebView). No external-browser or copy/paste-redirect fallback.
- 1Password: fill only empty fields on the original HTTPS Amazon `/ap/` page; never submit, never fill SMS/email codes or CAPTCHA; only non-secret item IDs are persisted.
- Device credentials live in the macOS Keychain (`audible-unlock` / `audible-device`); activation bytes in `~/.audible/activation_bytes` (mode 0600, shared with other tools). Logout deregisters the device, then removes both.
- Python tests must never touch the real Keychain or `~/Downloads`; patch `DOWNLOADS`, `CONFIG`, `BYTES_FILE` and use the in-memory keyring.
- Backend error messages are user-facing and fixed; unexpected exceptions map to a generic message.

## Conventions

- Pinned deps: GPUI `=0.2.2` (`font-kit` required for text), `audible==0.9.1`, `keyring==25.7.0`. Use `--locked` everywhere.
- Keep changes minimal; no new dependencies without a reason.
- Restarting the running app cancels an in-progress sign-in; say so before restarting.
