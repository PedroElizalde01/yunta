#!/bin/sh
# Builds dist/yunta_<version>_amd64.deb, and dist/yunta.exe when the mingw-w64 cross compiler is installed.
set -eu
cd "$(dirname "$0")"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n1)
rm -rf dist
mkdir -p dist

cargo build --release
root=$(mktemp -d)
trap 'rm -rf "$root"' EXIT
chmod 755 "$root"
install -Dm755 target/release/yunta "$root/usr/bin/yunta"
install -Dm644 assets/yunta.svg "$root/usr/share/icons/hicolor/scalable/apps/yunta.svg"
install -Dm644 assets/yunta.png "$root/usr/share/icons/hicolor/256x256/apps/yunta.png"
install -Dm644 /dev/stdin "$root/usr/share/applications/yunta.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=Yunta
Comment=One keyboard and mouse for two computers
Exec=yunta
Icon=yunta
Terminal=false
Categories=Utility;
EOF
# The settings window loads OpenGL and the X11 libraries winit opens at run time.
install -Dm644 /dev/stdin "$root/DEBIAN/control" <<EOF
Package: yunta
Version: $version
Architecture: amd64
Maintainer: Pedro Elizalde
Depends: libc6, libgl1, libegl1, libx11-6, libx11-xcb1, libxcursor1, libxi6, libxrandr2, libxkbcommon-x11-0
Description: One keyboard and mouse for two computers on the same network
 Push the pointer through the edge of the screen, or double-tap Right Ctrl, and
 input carries on onto the other machine, with the clipboard. X11 only.
EOF
dpkg-deb --root-owner-group --build "$root" "dist/yunta_${version}_amd64.deb"

if command -v x86_64-w64-mingw32-gcc >/dev/null; then
    cargo build --release --target x86_64-pc-windows-gnu
    cp target/x86_64-pc-windows-gnu/release/yunta.exe dist/
else
    echo "Skipping yunta.exe: install mingw-w64 to cross-compile it." >&2
fi

# Signed checksums, which the app's one-click update insists on. The key stays on this machine.
key=${YUNTA_SIGNING_KEY:-$HOME/.config/yunta-release/signing.pem}
if [ -f "$key" ]; then
    (cd dist && sha256sum yunta_*.deb $(ls yunta.exe 2>/dev/null) > SHA256SUMS)
    openssl pkeyutl -sign -rawin -inkey "$key" -in dist/SHA256SUMS -out dist/SHA256SUMS.sig
else
    echo "Skipping SHA256SUMS.sig: no signing key at $key, so this build cannot be offered as an update." >&2
fi
ls -l dist
