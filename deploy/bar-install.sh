#!/bin/sh
# Install gust-bar to start at login (per user, no sudo). Run: sh deploy/bar-install.sh
set -eu
cd "$(dirname "$0")/.."
[ "$(id -u)" != 0 ] || { echo "run as yourself, not root"; exit 1; }
cargo build --release -p gust-bar
BIN="$HOME/.local/bin/gust-bar"
mkdir -p "$HOME/.local/bin" "$HOME/Library/LaunchAgents"
install -m 755 target/release/gust-bar "$BIN"
LABEL=local.gust.bar
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
cat > "$PLIST" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key><string>$LABEL</string>
	<key>ProgramArguments</key><array><string>$BIN</string></array>
	<key>RunAtLoad</key><true/>
	<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
	<key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
PL
pkill -x gust-bar 2>/dev/null || true   # stop any copy started by hand
launchctl bootout "gui/$(id -u)/$LABEL" 2>/dev/null || true
# bootout returns before launchd has finished removing the job; bootstrapping
# too early fails with "5: Input/output error". Wait for it, then retry a few times.
i=0
while launchctl print "gui/$(id -u)/$LABEL" >/dev/null 2>&1 && [ $i -lt 50 ]; do sleep 0.1; i=$((i + 1)); done
for try in 1 2 3 4 5; do
  if launchctl bootstrap "gui/$(id -u)" "$PLIST" 2>/dev/null; then
    echo "gust-bar installed; starts at login. Quitting it from its menu keeps it quit until next login."
    exit 0
  fi
  sleep 1
done
echo "launchctl bootstrap kept failing; try: launchctl bootstrap gui/$(id -u) $PLIST"
exit 1
