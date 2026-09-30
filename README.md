# Yunta

One keyboard and mouse for two computers on the same local network. Push the pointer through the
edge of the screen, or double-tap Right Ctrl, and it carries on onto the other machine.

Text and images on the clipboard go along at every switch. It runs in the background with a tray
icon: pause crossing, start at login, quit.

## Setup

1. Install: `sudo apt install ./yunta_<version>_amd64.deb` on Linux; on Windows, `yunta.exe` is
   the whole app, so put it anywhere.
2. Start it on both machines: `yunta` in a terminal on Linux, double-click on Windows. The first
   time, it opens pairing mode: each machine shows a six-digit code and lists the other. On one
   of them, type the other's number and the code it shows, like `1 482913`.
3. Set `edge` in each config to the side the other machine is on: with the PC to the right of
   the Linux machine, `edge = right` on Linux and `edge = left` on the PC. The config is
   `~/.config/yunta/yunta.conf` on Linux and `%APPDATA%\yunta\yunta.conf` on Windows.
4. Restart it on both, and tick Start at login in the tray menu.

`yunta pair` pairs again, with the same or another machine.

On the PC, allow Yunta through Windows Firewall on private networks when asked. It uses TCP 24830
for input and TCP and UDP 24831 while pairing.

## Build

`./package.sh` builds `dist/yunta_<version>_amd64.deb`, and `dist/yunta.exe` as well once the
Windows cross compiler is installed:

```
sudo apt install mingw-w64
rustup target add x86_64-pc-windows-gnu
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
