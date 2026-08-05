#!/usr/bin/env bash
set -euo pipefail

# Install the macOS launchd plist for papr-server (`com.papr.server` by default).
# Does not bootstrap or start the service — package/deploy.sh does that after
# building. Run once before the first local deploy.
#
# Usage:
#   scripts/install-service.sh
#   PORT=8080 scripts/install-service.sh
#   PAPR_DEPLOY_ROOT=/opt/papr scripts/install-service.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: install-service.sh is for macOS launchd"
  echo "       on Linux, use the systemd unit example in scripts/README.md"
  exit 1
fi

ensure_deploy_dirs
write_runner
write_ctl
seed_env_file

echo "==> write launchd plist → $PLIST"
mkdir -p "$(dirname "$PLIST")"
cat > "$PLIST" <<PLISTEOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$LABEL</string>
	<key>ProgramArguments</key>
	<array>
		<string>$RUNNER</string>
	</array>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
		<key>RUST_LOG</key>
		<string>info</string>
	</dict>
	<key>WorkingDirectory</key>
	<string>$DEPLOY_ROOT</string>
	<key>KeepAlive</key>
	<true/>
	<key>RunAtLoad</key>
	<true/>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>StandardErrorPath</key>
	<string>$LOG_DIR/server.log</string>
	<key>StandardOutPath</key>
	<string>$LOG_DIR/server.log</string>
</dict>
</plist>
PLISTEOF

echo "OK: service plist installed; the service has not been started"
echo "    edit $ENV_FILE (admin password, etc.), then: scripts/deploy.sh"
