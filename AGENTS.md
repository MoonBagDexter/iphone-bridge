# iphone-bridge — local workflow

Rust app that streams PC audio to an iPhone and the iPhone mic back to Windows (see README.md).

## Standing instructions (from the user)

Whenever we work on this project (pull updates or change code), ALWAYS finish by:

1. `git pull` (if working from upstream) then rebuild the release binary:
   ```powershell
   cargo build --release
   ```
2. Re-create the shortcuts so Desktop and auto-startup always launch the latest build:
   ```powershell
   powershell -ExecutionPolicy Bypass -File .\install-shortcuts.ps1
   ```

Build artifacts go to `S:\cargo-builds\iphone-bridge\` (set in `.cargo/config.toml`) because C: is nearly full — do NOT move them back to C:. Shortcuts point at `S:\cargo-builds\iphone-bridge\release\iphone-bridge.exe`, so a rebuild alone refreshes them, but re-run the script anyway in case the path or icon changed.

- Desktop shortcut: `%USERPROFILE%\Desktop\iPhone Bridge.lnk`
- Auto-startup shortcut: `%APPDATA%\Microsoft\Windows\Start Menu\Programs\Startup\iPhone Bridge.lnk`

## Notes

- Requires VB-CABLE for the virtual mic direction.
- Serves HTTPS on port 8443 with a self-signed cert; open the printed URL in Safari on the iPhone.
