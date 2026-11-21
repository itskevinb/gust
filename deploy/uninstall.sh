#!/bin/sh
# Remove gustd. Stopping it hands the fans back to macOS. Run: sudo sh deploy/uninstall.sh
set -eu
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
LABEL=local.gust.gustd
launchctl bootout system/$LABEL 2>/dev/null || true
rm -f /Library/LaunchDaemons/$LABEL.plist /usr/local/libexec/gustd /var/run/gust.json
echo "gustd removed; fans are back on macOS control."
