#!/bin/sh
# Builds target/2fcast_<version>_amd64.deb.  Installing it with
#   sudo apt install ./target/2fcast_*.deb
# pulls in every runtime dependency (GStreamer plugins, GTK, pactl).
set -e
cd "$(dirname "$0")/.."
cargo build --release
version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/app/Cargo.toml | head -1)
root=target/deb/2fcast_${version}_amd64
rm -rf "$root"
install -Dm755 target/release/2fcast "$root/usr/bin/2fcast"
install -Dm644 packaging/2fcast.desktop "$root/usr/share/applications/2fcast.desktop"
install -Dm644 README.md "$root/usr/share/doc/2fcast/README.md"
mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<CONTROL
Package: 2fcast
Version: $version
Architecture: amd64
Maintainer: Artem Ostretsov <ostretsovaa@gmail.com>
Section: video
Priority: optional
Depends: libgtk-4-1 (>= 4.6), libgstreamer1.0-0 (>= 1.20), libgstreamer-plugins-base1.0-0 (>= 1.20),
 gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad,
 gstreamer1.0-plugins-ugly, gstreamer1.0-libav, gstreamer1.0-x, gstreamer1.0-pulseaudio,
 pulseaudio-utils, liborc-0.4-0
Description: Two-person LAN videocasting: shared screen, drawing, voice, recording
 Both participants start the app on the same local network; it finds the other
 one by itself.  Share a window, draw or write over it, talk with low latency
 and record the composed picture (shared space + both webcams) to MKV.
CONTROL
dpkg-deb --root-owner-group --build "$root" "target/2fcast_${version}_amd64.deb"
echo "Built target/2fcast_${version}_amd64.deb"
