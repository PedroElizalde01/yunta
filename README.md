# Yunta

One keyboard and mouse for two computers on the same local network. Push the pointer through the
edge of the screen, or double-tap Right Ctrl, and it carries on onto the other machine.

Text and images on the clipboard go along at every switch. A tray icon and packages come next.

## Setup

1. Run `yunta pair` on both machines. Each shows a six-digit code and lists the other.
2. On one of them, type the other's number and the code it shows, like `1 482913`.
3. Set `edge` in each config (the path is printed by `yunta init`) to the side the other machine
   is on: with the PC to the right of the Linux machine, `edge = right` on Linux and
   `edge = left` on the PC.
4. Run `yunta` on both.

On the PC, allow Yunta through Windows Firewall on private networks when asked. It uses TCP 24830
for input and TCP and UDP 24831 while pairing.

## Build

`cargo build --release`. For the PC, either build there after installing Rust from rustup.rs, or
cross-compile from Linux:

```
sudo apt install mingw-w64
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
```

## Security

Pairing runs SPAKE2 with the code as the password, so the code never crosses the network and
cannot be guessed offline from what does. A wrong code costs a live attempt: three end pairing
mode, which also switches itself off after two minutes. Outside pairing mode nothing listens on
UDP.

The link is Noise_KK (X25519, ChaCha20-Poly1305, BLAKE2s). Each machine accepts only the one
public key it was given, every connection gets fresh keys, and replayed or altered traffic is
rejected. Only private, link-local and loopback addresses may connect, and a handshake has two
seconds to finish. Nothing leaves the local network.

## Known limits

- Files do not cross, only text up to 256KB and images up to 8MB as PNG.
- X11 only on Linux. Wayland has no way for an app to watch or take over global input.
- Windows ignores input sent into administrator windows (Task Manager, installers, UAC) unless
  yunta itself runs as administrator.
