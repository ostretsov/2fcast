#!/bin/bash
# Builds a self-contained package: target/2fcast_<version>_amd64.deb
#
# Like Discord or an AppImage, it carries its own GTK 4, GStreamer (only the
# plugins 2fcast uses), their libraries and pactl in /opt/2fcast.  From the
# system it needs only libraries every Ubuntu desktop has with a stable ABI
# (glibc, OpenGL/Mesa, X11, fonts, ...; see EXCLUDE).  Build it on the oldest
# Ubuntu you want to support (glibc is forward compatible only).
#
#   sudo apt install ./target/2fcast_*_amd64.deb
set -euo pipefail
cd "$(dirname "$0")/.."

ARCH_LIB=/usr/lib/x86_64-linux-gnu
GST=$ARCH_LIB/gstreamer-1.0
PREFIX=/opt/2fcast

# GStreamer plugins 2fcast uses (H.264 decoding via the small OpenH264
# instead of FFmpeg, which would add ~200 MB).
PLUGINS="coreelements app audioconvert audiomixer audioresample audiotestsrc cairo
  compositor inter jpeg level matroska opus openh264 playback pulseaudio rtp
  rtpmanager typefindfunctions udp video4linux2 videoconvert videoparsersbad
  videorate videoscale videotestsrc volume x264 ximagesrc"
PIXBUF_LOADERS="libpixbufloader-svg.so"

# Libraries taken from the system: same name and ABI on every Ubuntu desktop,
# or tied to the user's hardware/session (graphics drivers, X server).
EXCLUDE='^(ld-linux|libc|libm|libdl|libpthread|librt|libresolv|libutil|libgcc_s|libstdc\+\+|'\
'libGL|libGLX|libGLdispatch|libEGL|libGLESv2|libOpenGL|libgbm|libdrm|libvulkan|'\
'libX11|libX11-xcb|libxcb|libxcb-.*|libXau|libXdmcp|libXext|libXrender|libXi|libXfixes|libXdamage|'\
'libXrandr|libXcursor|libXinerama|libXcomposite|libxkbcommon|libwayland-.*|'\
'libfontconfig|libfreetype|libexpat|libz|libudev|libsystemd|libdbus-1|libselinux|libmount|libblkid|'\
'libuuid|libgcrypt|libgpg-error|liblzma|libzstd|libbz2|libcap|liblz4)\.so'

version=$(sed -n 's/^version = "\(.*\)"/\1/p' crates/app/Cargo.toml | head -1)
cargo build --release

root=target/bundle/2fcast_${version}_amd64
opt=$root$PREFIX
rm -rf "$root"
mkdir -p "$opt/bin" "$opt/libexec" "$opt/lib/gstreamer-1.0" "$opt/lib/gdk-pixbuf-2.0/loaders" "$opt/lib/gio/modules"

install -m755 target/release/2fcast "$opt/libexec/2fcast"
install -m755 "$ARCH_LIB/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner" "$opt/libexec/"
install -m755 /usr/bin/pactl "$opt/bin/pactl"
binaries=("$opt/libexec/2fcast" "$opt/libexec/gst-plugin-scanner" "$opt/bin/pactl")
for p in $PLUGINS; do
  install -m644 "$GST/libgst$p.so" "$opt/lib/gstreamer-1.0/"
  binaries+=("$opt/lib/gstreamer-1.0/libgst$p.so")
done
for l in $PIXBUF_LOADERS; do
  install -m644 "$ARCH_LIB/gdk-pixbuf-2.0/2.10.0/loaders/$l" "$opt/lib/gdk-pixbuf-2.0/loaders/"
  binaries+=("$opt/lib/gdk-pixbuf-2.0/loaders/$l")
done

# Every library these need (recursively; ldd resolves the whole tree).
missing=$(ldd "${binaries[@]}" 2>/dev/null | grep 'not found' || true)
if [ -n "$missing" ]; then echo "unresolved libraries:"; echo "$missing"; exit 1; fi
: > "$root.syslibs"
ldd "${binaries[@]}" 2>/dev/null | awk '/=> \//{print $3}' | sort -u | while read -r lib; do
  name=$(basename "$lib")
  if [[ $name =~ $EXCLUDE ]]; then
    echo "$lib" >> "$root.syslibs"
  else
    cp -L "$lib" "$opt/lib/$name"
  fi
done

# The pixbuf loader cache must name the final install path.
GDK_PIXBUF_MODULEDIR="$opt/lib/gdk-pixbuf-2.0/loaders" \
  "$ARCH_LIB/gdk-pixbuf-2.0/gdk-pixbuf-query-loaders" "$opt"/lib/gdk-pixbuf-2.0/loaders/*.so \
  | sed "s|$(realpath "$opt")|$PREFIX|g" > "$opt/lib/gdk-pixbuf-2.0/loaders.cache"
# Every module path in the cache must point into the final install location.
if grep '^"/' "$opt/lib/gdk-pixbuf-2.0/loaders.cache" | grep -v "^\"$PREFIX/"; then
  echo "loaders.cache names paths outside $PREFIX (above)"
  exit 1
fi

cat > "$opt/bin/2fcast" <<'LAUNCHER'
#!/bin/sh
# Runs 2fcast with the libraries and GStreamer plugins bundled in /opt/2fcast.
HERE=/opt/2fcast
export LD_LIBRARY_PATH="$HERE/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export PATH="$HERE/bin:$PATH"
# Only the bundled plugins; own registry so the system's GStreamer is untouched.
export GST_PLUGIN_SYSTEM_PATH_1_0="$HERE/lib/gstreamer-1.0"
export GST_PLUGIN_PATH_1_0=""
export GST_PLUGIN_SCANNER_1_0="$HERE/libexec/gst-plugin-scanner"
export GST_REGISTRY_1_0="${XDG_CACHE_HOME:-$HOME/.cache}/2fcast/gstreamer-registry.bin"
# Bundled GLib/GTK must not load the system's (newer) modules.
export GIO_MODULE_DIR="$HERE/lib/gio/modules"
export GDK_PIXBUF_MODULE_FILE="$HERE/lib/gdk-pixbuf-2.0/loaders.cache"
mkdir -p "$(dirname "$GST_REGISTRY_1_0")"
exec "$HERE/libexec/2fcast" "$@"
LAUNCHER
chmod 755 "$opt/bin/2fcast"

mkdir -p "$root/usr/bin"
ln -s "$PREFIX/bin/2fcast" "$root/usr/bin/2fcast"
install -Dm644 packaging/2fcast.desktop "$root/usr/share/applications/2fcast.desktop"
install -Dm644 README.md "$root/usr/share/doc/2fcast/README.md"

# Runtime dependencies: the system packages that own the libraries we did not
# bundle (found with dpkg, so nothing is forgotten).
# libGL/libEGL are loaded at runtime (dlopen by libepoxy), invisible to ldd.
depends="libc6 (>= $(dpkg-query -W -f='${Version}' libc6 | cut -d- -f1)), libgl1, libegl1"
owner() { # packages register either /lib or /usr/lib paths (merged /usr)
  local real; real=$(realpath "$1")
  for path in "$1" "$real" "/usr$1" "${real#/usr}"; do
    dpkg -S "$path" 2>/dev/null < /dev/null && return
  done
  echo "no package owns $1" >&2
  exit 1
}
for pkg in $(while read -r lib; do owner "$lib"; done < "$root.syslibs" | cut -d: -f1 | sort -u); do
  [ "$pkg" = libc6 ] || depends="$depends, $pkg"
done
rm -f "$root.syslibs"
echo "Depends: $depends"

size=$(du -sk "$root" | cut -f1)
mkdir -p "$root/DEBIAN"
cat > "$root/DEBIAN/control" <<CONTROL
Package: 2fcast
Version: $version
Architecture: amd64
Maintainer: Artem Ostretsov <ostretsovaa@gmail.com>
Section: video
Priority: optional
Installed-Size: $size
Depends: $depends
Description: Two-person LAN videocasting: shared screen, drawing, voice, recording
 Both participants start the app on the same local network; it finds the other
 one by itself.  Share a window, draw or write over it, talk with low latency
 and record the composed picture (shared space + both webcams) to MKV.
 .
 Self-contained: GTK 4, GStreamer and their libraries are bundled in /opt/2fcast.
CONTROL

dpkg-deb --root-owner-group -Zxz --build "$root" "target/2fcast_${version}_amd64.deb"
echo "Built target/2fcast_${version}_amd64.deb ($(du -h "target/2fcast_${version}_amd64.deb" | cut -f1), $(ls "$opt/lib" | wc -l) bundled libraries)"
