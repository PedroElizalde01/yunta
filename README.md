# Yunta

One keyboard and mouse for two computers on the same local network. Push the pointer through the
edge of the screen, or double-tap Right Ctrl, and it carries on onto the other machine.

Text and images on the clipboard go along at every switch. Pairing and a tray icon come next.

## Setup

Until pairing lands, the two machines are introduced by hand:

1. On each machine, run `yunta init`. It writes a config holding a fresh key and prints the
   machine's public key.
2. In each config, set `peer_key` to the other machine's public key.
3. In the Linux config, set `peer` to the PC's address. The PC listens on TCP 24830: when Windows
   Firewall asks, allow it on private networks only.
4. Set `edge` on each to the side the other machine is on: with the PC to the right of the Linux
   machine, `edge = right` on Linux and `edge = left` on the PC.
5. Run `yunta` on both.

## Build

`cargo build --release`. For the PC, either build there after installing Rust from rustup.rs, or
cross-compile from Linux:

```
sudo apt install mingw-w64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

## Security

The link is Noise_KK (X25519, ChaCha20-Poly1305, BLAKE2s). Each machine accepts only the one
public key it was given, every connection gets fresh keys, and replayed or altered traffic is
rejected. Only private, link-local and loopback addresses may connect, and a handshake has two
seconds to finish. Nothing leaves the local network.

## Known limits

- Files do not cross, only text up to 256KB and images up to 8MB as PNG.
- X11 only on Linux. Wayland has no way for an app to watch or take over global input.
- Windows ignores input sent into administrator windows (Task Manager, installers, UAC) unless
  yunta itself runs as administrator.
