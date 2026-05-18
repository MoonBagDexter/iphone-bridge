# iphone-bridge

Turn your iPhone (and any headphones connected to it — AirPods, wired, whatever) into a Windows headset, over your local network.

Windows app captures your PC's audio output and serves it to a tiny web page in Safari on the iPhone. The same page captures the iPhone's mic and sends it back to a virtual mic on Windows. Both directions run independently — audio out and mic in are separate toggles, both can be on at the same time.

## Why

If you want to use your AirPods (or any iPhone audio gear) as a real Windows headset for calls, games, music, dictation, etc., without paying for proprietary "phone-as-mic" apps and without dealing with Bluetooth pairing on the PC. The audio path goes PC → Wi-Fi → iPhone → AirPods, and the mic path goes AirPods → iPhone → Wi-Fi → PC.

## How it works

- **Rust server (Windows)** — captures system audio via WASAPI loopback, serves an HTTPS page + WebSocket on port 8443. Receives mic audio back over WebSocket and writes it into the VB-CABLE virtual input device, which any Windows app can then select as its microphone.
- **Web client (iPhone Safari)** — plays the streamed audio through Web Audio, captures the mic via `getUserMedia`, sends it back over the same WebSocket.
- **TLS** — required because `getUserMedia` doesn't work on non-HTTPS origins. The app auto-generates a self-signed cert; if Tailscale is installed, it'll use the Tailscale-issued Let's Encrypt cert for your machine's `*.ts.net` hostname instead so Safari trusts it without warnings.

## Requirements

- Windows 10/11
- [VB-CABLE](https://vb-audio.com/Cable/) installed (free virtual audio driver). The bridge writes the mic stream into the VB-CABLE Input device; any Windows app picks it up by selecting "CABLE Output" as its microphone.
- Rust toolchain to build (`cargo build --release`)
- iPhone on the same Wi-Fi network, or both devices on Tailscale

## Build & run

```
cargo build --release
.\target\release\iphone-bridge.exe
```

On launch the console prints a URL like `https://<your-machine>:8443`. Open it in Safari on the iPhone, accept the self-signed cert if prompted, then hit the audio and/or mic toggles.

For best results: add Safari to the home screen as a web app (Share → Add to Home Screen). It keeps the screen alive and runs without the URL bar.

## Status

Personal project, "works on my machine" tier. Built for a specific setup (Windows + iPhone + AirPods + Tailscale + VB-CABLE). Feel free to fork and adapt.

## License

[AGPL-3.0-or-later](LICENSE). You're free to use, modify, and redistribute this — including running it as a network service — as long as anyone you give it to (or who uses it over a network) gets the same freedoms with your changes.
