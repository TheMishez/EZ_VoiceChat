# EZ VoiceChat

Low-latency voice chat for Windows by EZGGco. One app that joins both **Mumble** and **TeamSpeak 3** servers.

- **Low-latency audio mode**: asks Windows for its smallest audio buffers (about 3 ms instead of 10 ms) without locking other apps out of your device.
- **Adaptive jitter buffer**: plays after ~15 ms on a clean connection and only grows when the network is actually jittery.
- **Live voice-delay readout** next to the ping, with a per-stage breakdown on hover.
- **Opus voice** at 10 ms frames (20 ms optional), 64 kbps by default, up to 128 kbps.
- **RNNoise noise suppression**, voice activation with a live level meter, or push-to-talk.
- **Push-to-talk through Raw Input**: no keyboard hooks, no added input lag, and the key still reaches your game. Mouse 3–5 work.
- **Light on your PC**: native Rust audio engine, one small WebView window, and it lives in the tray when closed.
- Channel tree with talking indicators, per-user volume, text chat, bookmarks.

## Servers
| | Mumble | TeamSpeak 3 |
|---|---|---|
| Voice | UDP (encrypted, OCB2-AES128) with automatic TCP fallback | TS3 protocol via [tsclientlib](https://github.com/ReSpeak/tsclientlib) |
| Default port | 64738 | 9987 |
| Notes | Certificates are trusted on first use; you're warned if one changes | Unofficial open-source implementation; TeamSpeak 5/6-only servers are not supported |

Want your own server? See [`server/`](server/) for the **EZ server**: a tuned Mumble server with a one-command Linode setup.

## Build
Installers are built by GitHub Actions on every push (see the **Actions** tab → latest run → *Artifacts*).

Locally on Windows (Rust stable, Node 22, CMake):
```bash
npm install
npx tauri build
```

## Layout
```
src-tauri/src/
  audio.rs      capture, RNNoise, VAD/PTT gate, playback mixer
  wasapi.rs     low-latency Windows audio (IAudioClient3), cpal fallback
  ptt.rs        Raw Input push-to-talk
  session.rs    shared server model (channels, users, chat) for both protocols
  mumble/       Mumble: TLS control channel, UDP voice, OCB2 crypto, jitter buffer
  ts.rs         TeamSpeak 3 via tsclientlib
ui/             the app window
server/         EZ server (Docker + Linode setup)
crypt-test/     known-answer tests for the Mumble voice crypto
```
