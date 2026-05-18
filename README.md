# iphone-bridge

Use your iPhone (and any headphones connected to it — AirPods, wired, etc.) as a Windows headset over your local network.

## What it does

- Streams your PC's audio output to a web page on the iPhone (you hear it through whatever's plugged into the phone).
- Streams the iPhone's microphone back to Windows as a virtual mic that any app (Discord, Zoom, games, etc.) can select.
- Both directions are independent — you can run just audio, just mic, or both at once.

## Requirements

- Windows 10 or 11
- [VB-CABLE](https://vb-audio.com/Cable/) installed (free virtual audio driver — used as the destination for the mic stream)
- Rust toolchain ([rustup](https://rustup.rs/))
- An iPhone on the same Wi-Fi as the PC

## Run it

```
cargo build --release
.\target\release\iphone-bridge.exe
```

The console prints a URL like `https://<your-pc>:8443`. Open it in Safari on the iPhone, accept the self-signed certificate, and tap the audio / mic buttons.

In Windows sound settings, select **CABLE Output** as the microphone for whatever app you want to use the iPhone mic in.

## License

[AGPL-3.0-or-later](LICENSE).
