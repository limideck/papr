#!/usr/bin/env bash
set -euo pipefail

# Run the local macOS release package from package-mac.sh.
#
# Usage:
#   scripts/run-mac.sh
#   PORT=8090 scripts/run-mac.sh
#   PAPR_MAC_ROOT=~/Deploy/papr-mac scripts/run-mac.sh

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MAC_ROOT="${PAPR_MAC_ROOT:-$HOME/Deploy/papr-mac}"
BIN="$MAC_ROOT/bin/papr-server"
ENV_FILE="$MAC_ROOT/.env"
ENV_EXAMPLE="$MAC_ROOT/.env.example"

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "error: run-mac.sh is for macOS only" >&2
  exit 1
fi

if [ ! -x "$BIN" ]; then
  echo "error: missing $BIN — run scripts/package-mac.sh first" >&2
  exit 1
fi

if [ ! -d "$MAC_ROOT/dist" ] || [ ! -f "$MAC_ROOT/dist/index.html" ]; then
  echo "error: missing $MAC_ROOT/dist — run scripts/package-mac.sh first" >&2
  exit 1
fi

mkdir -p "$MAC_ROOT/data" "$MAC_ROOT/logs"

if [ ! -f "$ENV_FILE" ]; then
  if [ -f "$ENV_EXAMPLE" ]; then
    cp "$ENV_EXAMPLE" "$ENV_FILE"
    chmod 600 "$ENV_FILE"
    echo "note: created $ENV_FILE from .env.example — edit secrets before exposing"
  else
    cat > "$ENV_FILE" <<ENVEOF
PAPR_DB=$MAC_ROOT/data/papr.db
PORT=8111
PAPR_STATIC_DIR=$MAC_ROOT/dist
PAPR_ADMIN_USER=admin
PAPR_ADMIN_PASSWORD=changeme
RUST_LOG=info
ENVEOF
    chmod 600 "$ENV_FILE"
  fi
fi

# Load package .env (PORT / secrets), then apply CLI overrides.
set -a
# shellcheck disable=SC1090
source "$ENV_FILE"
set +a

# Prefer absolute paths so cwd does not matter.
export PAPR_DB="${PAPR_DB:-$MAC_ROOT/data/papr.db}"
case "$PAPR_DB" in
  /*) ;;
  *) export PAPR_DB="$MAC_ROOT/${PAPR_DB#./}" ;;
esac

export PAPR_STATIC_DIR="${PAPR_STATIC_DIR:-$MAC_ROOT/dist}"
case "$PAPR_STATIC_DIR" in
  /*) ;;
  *) export PAPR_STATIC_DIR="$MAC_ROOT/${PAPR_STATIC_DIR#./}" ;;
esac

# PORT: explicit env wins over .env (default 8080 for local Mac).
export PORT="${PORT:-8080}"
export RUST_LOG="${RUST_LOG:-info}"

echo "==> papr-server (mac) PORT=$PORT"
echo "    PAPR_STATIC_DIR=$PAPR_STATIC_DIR"
echo "    PAPR_DB=$PAPR_DB"
echo "    binary=$BIN"
exec "$BIN"
