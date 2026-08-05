#!/usr/bin/env bash
set -euo pipefail

# Build papr-server + frontend locally and install into DEPLOY_ROOT.
# Does not start or touch the launchd/systemd service.
#
# Usage:
#   scripts/package.sh
#   PAPR_DEPLOY_ROOT=/opt/papr scripts/package.sh
#
# Build on a machine matching production OS/arch (macOS arm64 binary will not
# run on Linux x86_64). Cross-compile is out of scope for these scripts.
#
# Output layout:
#   $DEPLOY_ROOT/bin/papr-server
#   $DEPLOY_ROOT/bin/run-papr-server.sh
#   $DEPLOY_ROOT/bin/papr-ctl.sh
#   $DEPLOY_ROOT/dist/
#   $DEPLOY_ROOT/data/
#   $DEPLOY_ROOT/run/
#   $DEPLOY_ROOT/logs/
#   $DEPLOY_ROOT/.env / .env.example

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

DEV_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "==> package papr-server → $DEPLOY_ROOT"
ensure_deploy_dirs
write_runner
write_ctl
install_env_example
seed_env_file

echo "==> cargo build --release -p papr-server"
(
  cd "$DEV_ROOT"
  cargo build --release -p papr-server
)
cp -f "$DEV_ROOT/target/release/papr-server" "$BIN"
chmod +x "$BIN"
echo "    $(ls -la "$BIN" | awk '{print $5" bytes"}') → $BIN"
echo "    note: binary is for $(uname -s)/$(uname -m) — must match the deploy host"

echo "==> pnpm build (frontend → dist/)"
(
  cd "$DEV_ROOT"
  if [ -f pnpm-lock.yaml ]; then
    pnpm install --frozen-lockfile
  else
    pnpm install
  fi
  pnpm build
)
rsync -a --delete --exclude '._*' --exclude '.DS_Store' "$DEV_ROOT/dist/" "$STATIC_DIR/"
echo "    frontend → $STATIC_DIR"

echo "OK: package ready at $DEPLOY_ROOT"
echo "    edit $ENV_FILE then: scripts/install-service.sh (macOS) or scripts/deploy.sh (remote)"
