#!/usr/bin/env bash
set -euo pipefail

# Build a native macOS local package (darwin binary + Vite dist).
# Does NOT ship to Linux production — use deploy.sh --from-release for that.
#
# Usage:
#   scripts/package-mac.sh
#   PAPR_MAC_ROOT=~/Deploy/papr-mac scripts/package-mac.sh
#
# Layout:
#   $PAPR_MAC_ROOT/
#     bin/papr-server
#     dist/
#     data/
#     logs/
#     .env.example
#     (.env created on first run-mac.sh if missing)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: package-mac.sh is for macOS only (got $(uname -s))" >&2
  echo "       for Linux production: scripts/deploy.sh --from-release latest" >&2
  exit 1
fi

MAC_ROOT="${PAPR_MAC_ROOT:-$HOME/Deploy/papr-mac}"
BIN_DIR="$MAC_ROOT/bin"
STATIC_DIR="$MAC_ROOT/dist"
DATA_DIR="$MAC_ROOT/data"
LOG_DIR="$MAC_ROOT/logs"
ENV_EXAMPLE="$MAC_ROOT/.env.example"
PORT_DEFAULT="${PORT:-8080}"

echo "==> package-mac → $MAC_ROOT"
mkdir -p "$BIN_DIR" "$STATIC_DIR" "$DATA_DIR" "$LOG_DIR"

echo "==> cargo build --release -p papr-server (native $(uname -m))"
(
  cd "$REPO_ROOT"
  cargo build --release -p papr-server
)
cp -f "$REPO_ROOT/target/release/papr-server" "$BIN_DIR/papr-server"
chmod +x "$BIN_DIR/papr-server"
echo "    $(ls -la "$BIN_DIR/papr-server" | awk '{print $5" bytes"}') → $BIN_DIR/papr-server"

echo "==> pnpm build (frontend → dist/)"
(
  cd "$REPO_ROOT"
  if [ -f pnpm-lock.yaml ]; then
    pnpm install --frozen-lockfile
  else
    pnpm install
  fi
  pnpm build
)
rsync -a --delete \
  --exclude '._*' --exclude '.DS_Store' \
  "$REPO_ROOT/dist/" "$STATIC_DIR/"
echo "    frontend → $STATIC_DIR"

# Prefer server .env.example; seed PORT for local Mac (8080 default).
SRC_EXAMPLE="$REPO_ROOT/crates/papr-server/.env.example"
if [ -f "$SRC_EXAMPLE" ]; then
  cp -f "$SRC_EXAMPLE" "$ENV_EXAMPLE"
else
  cat > "$ENV_EXAMPLE" <<ENVEOF
PAPR_DB=./data/papr.db
PORT=$PORT_DEFAULT
PAPR_STATIC_DIR=./dist
PAPR_ADMIN_USER=admin
PAPR_ADMIN_PASSWORD=changeme
ENVEOF
fi
if grep -q '^PORT=' "$ENV_EXAMPLE" 2>/dev/null; then
  sed -i.bak "s|^PORT=.*|PORT=$PORT_DEFAULT|" "$ENV_EXAMPLE" && rm -f "$ENV_EXAMPLE.bak"
else
  echo "PORT=$PORT_DEFAULT" >> "$ENV_EXAMPLE"
fi
# Ensure relative paths for package-local layout
if grep -q '^PAPR_DB=' "$ENV_EXAMPLE" 2>/dev/null; then
  sed -i.bak 's|^PAPR_DB=.*|PAPR_DB=./data/papr.db|' "$ENV_EXAMPLE" && rm -f "$ENV_EXAMPLE.bak"
fi
if grep -q '^#\?PAPR_STATIC_DIR=' "$ENV_EXAMPLE" 2>/dev/null; then
  # Uncomment / set static dir
  if grep -q '^# PAPR_STATIC_DIR=' "$ENV_EXAMPLE"; then
    sed -i.bak 's|^# PAPR_STATIC_DIR=.*|PAPR_STATIC_DIR=./dist|' "$ENV_EXAMPLE" && rm -f "$ENV_EXAMPLE.bak"
  elif grep -q '^PAPR_STATIC_DIR=' "$ENV_EXAMPLE"; then
    sed -i.bak 's|^PAPR_STATIC_DIR=.*|PAPR_STATIC_DIR=./dist|' "$ENV_EXAMPLE" && rm -f "$ENV_EXAMPLE.bak"
  fi
else
  echo "PAPR_STATIC_DIR=./dist" >> "$ENV_EXAMPLE"
fi

echo "OK: mac package ready at $MAC_ROOT"
echo "    edit $MAC_ROOT/.env (copied from .env.example on first run)"
echo "    then: scripts/run-mac.sh"
echo "    production Linux: scripts/deploy.sh --from-release latest"
