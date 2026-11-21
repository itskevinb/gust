#!/bin/sh
# Install gust: builds it, sets up a starter curve, installs the fan daemon
# (asks for your password once) and the menu bar app.
#   ./install.sh            install or update everything
set -eu
cd "$(dirname "$0")"
[ "$(id -u)" != 0 ] || { echo "run as yourself, not with sudo (it asks when needed)"; exit 1; }
[ "$(uname -s)" = Darwin ] || { echo "gust only runs on macOS"; exit 1; }
[ "$(uname -m)" = arm64 ] || { echo "gust only supports Apple Silicon Macs"; exit 1; }
command -v cargo >/dev/null || { echo "needs Rust: install from https://rustup.rs then re-run"; exit 1; }

echo "==> building"
cargo build --release --workspace

CURVE="$HOME/.config/gust/curve.conf"
if [ ! -f "$CURVE" ]; then
  mkdir -p "$(dirname "$CURVE")"
  cp examples/curve.conf "$CURVE"
  echo "==> starter curve written to $CURVE"
else
  echo "==> keeping your curve at $CURVE"
fi
./target/release/gust curve --table "$CURVE" >/dev/null || { echo "your curve has an error; fix it and re-run"; exit 1; }

echo "==> installing the fan daemon (needs your password)"
sudo sh deploy/install.sh

echo "==> installing the menu bar app"
sh deploy/bar-install.sh

echo
echo "Done. Look for the temperature in your menu bar. 'gust status' shows what gustd is doing."
echo "Remove everything with ./uninstall.sh"
