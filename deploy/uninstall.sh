#!/bin/sh
# Remove gustd and the gust CLI. Stopping gustd hands the fans back to macOS.
# Usually run for you by ./uninstall.sh. Direct use: sudo sh deploy/uninstall.sh
set -eu
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
LABEL=local.gust.gustd
launchctl bootout system/$LABEL 2>/dev/null || true
rm -f /Library/LaunchDaemons/$LABEL.plist /usr/local/libexec/gustd /usr/local/bin/gust /var/run/gust.json
echo "gustd removed; fans are back on macOS control. (Log left at /var/log/gustd.log.)"
