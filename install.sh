#!/usr/bin/env bash
# User-local install: binary, .desktop, icons. No root needed.
set -euo pipefail
cd "$(dirname "$0")"
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
