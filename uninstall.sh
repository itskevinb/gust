#!/bin/sh
# Remove gust. The fans go straight back to macOS control. Your curve file is kept.
set -eu
cd "$(dirname "$0")"
[ "$(id -u)" != 0 ] || { echo "run as yourself, not with sudo (it asks when needed)"; exit 1; }
LABEL=local.gust.bar
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
pkill -x gust-bar 2>/dev/null || true
rm -f "$HOME/Library/LaunchAgents/$LABEL.plist" "$HOME/.local/bin/gust-bar"
sudo sh deploy/uninstall.sh
echo "gust removed. Your curve is still at ~/.config/gust/curve.conf"
