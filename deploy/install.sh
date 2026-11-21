#!/bin/sh
# Install or update gustd as a root launchd daemon. Run: sudo sh deploy/install.sh
set -eu
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
cd "$(dirname "$0")/.."
USER_HOME=$(eval echo "~${SUDO_USER:?run via sudo from your account}")
CURVE="$USER_HOME/.config/gust/curve.conf"
LABEL=local.gust.gustd
PLIST=/Library/LaunchDaemons/$LABEL.plist

if pgrep -x "Macs Fan Control" >/dev/null; then
  echo "Macs Fan Control is running. Quit it first: two fan controllers fight."; exit 1
fi
[ -f target/release/gustd ] || { echo "build first: cargo build --release"; exit 1; }
[ -f "$CURVE" ] || { echo "no curve at $CURVE"; exit 1; }

# Stop the old one (it hands fans back to macOS on SIGTERM).
launchctl bootout system/$LABEL 2>/dev/null || true

install -d -o root -g wheel -m 755 /usr/local/libexec
install -o root -g wheel -m 755 target/release/gustd /usr/local/libexec/gustd
sed "s|__CURVE__|$CURVE|" deploy/gustd.plist > "$PLIST"
chown root:wheel "$PLIST"; chmod 644 "$PLIST"
launchctl bootstrap system "$PLIST"
echo "gustd installed. Watch it: tail -f /var/log/gustd.log   Status: gust status"
