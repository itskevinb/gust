#!/bin/sh
# Install or update gustd (root launchd daemon) and the gust CLI.
# Usually run for you by ./install.sh. Direct use: sudo sh deploy/install.sh
set -eu
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
cd "$(dirname "$0")/.."
USER_HOME=$(eval echo "~${SUDO_USER:?run via sudo from your account}")
CURVE="$USER_HOME/.config/gust/curve.conf"
LABEL=local.gust.gustd
PLIST=/Library/LaunchDaemons/$LABEL.plist

for app in "Macs Fan Control" "TG Pro"; do
  if pgrep -x "$app" >/dev/null; then
    echo "$app is running. Quit it first: two fan controllers fight over the fans."; exit 1
  fi
done
[ -f target/release/gustd ] || { echo "build first: cargo build --release --workspace"; exit 1; }
[ -f "$CURVE" ] || { echo "no curve at $CURVE"; exit 1; }

# Stop the old one (it hands fans back to macOS on SIGTERM) and wait until
# launchd has fully removed it, or bootstrap fails with an I/O error.
launchctl bootout system/$LABEL 2>/dev/null || true
i=0
while launchctl print system/$LABEL >/dev/null 2>&1 && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done

install -d -o root -g wheel -m 755 /usr/local/libexec /usr/local/bin
install -o root -g wheel -m 755 target/release/gustd /usr/local/libexec/gustd
install -o root -g wheel -m 755 target/release/gust /usr/local/bin/gust
sed "s|__CURVE__|$CURVE|" deploy/gustd.plist > "$PLIST"
chown root:wheel "$PLIST"; chmod 644 "$PLIST"
for try in 1 2 3 4 5; do
  launchctl bootstrap system "$PLIST" 2>/dev/null && break
  [ $try = 5 ] && { echo "launchctl bootstrap failed; try: sudo launchctl bootstrap system $PLIST"; exit 1; }
  sleep 1
done
echo "gustd installed. Watch it: tail -f /var/log/gustd.log   Status: gust status"
