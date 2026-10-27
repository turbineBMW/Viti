#!/usr/bin/env bash
# User-local install: binary, .desktop, icons. No root needed.
set -euo pipefail
cd "$(dirname "$0")"

# Native libraries the gtk-rs crates link against: pkg-config module, minimum
# version (matches the Cargo feature flags), Arch package, Debian package.
deps=(
  "gtk4             4.18 gtk4           libgtk-4-dev"
  "libadwaita-1     1.7  libadwaita     libadwaita-1-dev"
  "gtksourceview-5  5.0  gtksourceview5 libgtksourceview-5-dev"
  "vte-2.91-gtk4    0.78 vte4           libvte-2.91-gtk4-dev"
)
missing=()
for tool in cargo pkg-config; do
  command -v "$tool" >/dev/null || { echo "missing: $tool" >&2; exit 1; }
done
for d in "${deps[@]}"; do
  read -r module min arch deb <<<"$d"
  if ! pkg-config --exists "$module"; then
    echo "missing: $module >= $min" >&2
    missing+=("$arch:$deb")
  elif ! pkg-config --atleast-version="$min" "$module"; then
    echo "too old: $module $(pkg-config --modversion "$module") (need >= $min)" >&2
    missing+=("$arch:$deb")
  fi
done
if [ ${#missing[@]} -gt 0 ]; then
  if command -v pacman >/dev/null; then
    echo "install with: sudo pacman -S --needed ${missing[*]%%:*}" >&2
  elif command -v apt-get >/dev/null; then
    echo "install with: sudo apt install ${missing[*]#*:}" >&2
  fi
  exit 1
fi
# Runtime tools: optional, only some features need them.
for tool in ssh:"SSH tunnels" mongosh:"the embedded shell" nvim:"editing documents (or set another editor)"; do
  command -v "${tool%%:*}" >/dev/null || echo "note: ${tool%%:*} not found; needed for ${tool#*:}" >&2
done

cargo build --release
install -Dm755 target/release/viti ~/.local/bin/viti
# Launchers often lack ~/.local/bin on PATH, and .desktop files don't expand ~,
# so bake the absolute binary path into the installed copy.
mkdir -p ~/.local/share/applications
sed "s#^Exec=.*#Exec=$HOME/.local/bin/viti %u#" data/dev.turbinebmw.Viti.desktop \
  > ~/.local/share/applications/dev.turbinebmw.Viti.desktop
chmod 644 ~/.local/share/applications/dev.turbinebmw.Viti.desktop
for n in 16 32 48 64 128 256 512; do
  if [ -f data/icons/viti-$n.png ]; then
    install -Dm644 data/icons/viti-$n.png ~/.local/share/icons/hicolor/${n}x${n}/apps/dev.turbinebmw.Viti.png
  fi
done
if [ -f data/icons/viti.svg ]; then
  install -Dm644 data/icons/viti.svg ~/.local/share/icons/hicolor/scalable/apps/dev.turbinebmw.Viti.svg
fi
# ~/.local/share/icons/hicolor has no index.theme, so without --ignore-theme-index
# this silently fails and leaves a stale icon-theme.cache behind.
gtk4-update-icon-cache -q -f --ignore-theme-index ~/.local/share/icons/hicolor 2>/dev/null || true
update-desktop-database ~/.local/share/applications 2>/dev/null || true
echo "installed: ~/.local/bin/viti"
