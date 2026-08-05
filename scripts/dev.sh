#!/usr/bin/env bash
set -euo pipefail

# Quick local API for development (debug binary).
#
# Frontend options:
#   A) Terminal 1: scripts/dev.sh
#      Terminal 2: pnpm dev          # Vite on :5173, proxies /api → :8080
#   B) pnpm build && PAPR_STATIC_DIR=dist PORT=8080 scripts/dev.sh
#      then open http://127.0.0.1:8080
#
# For a release-shaped local package, use package-mac.sh + run-mac.sh instead.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

export PORT="${PORT:-8080}"
export PAPR_DB="${PAPR_DB:-$REPO_ROOT/papr.db}"
export RUST_LOG="${RUST_LOG:-info}"

# Serve built UI if present (optional for Vite-only frontend work).
if [ -z "${PAPR_STATIC_DIR:-}" ] && [ -f "$REPO_ROOT/dist/index.html" ]; then
  export PAPR_STATIC_DIR="$REPO_ROOT/dist"
fi

# Load crates/papr-server/.env if present (dev secrets).
if [ -f "$REPO_ROOT/crates/papr-server/.env" ]; then
  set -a
  # shellcheck disable=SC1091
  source "$REPO_ROOT/crates/papr-server/.env"
  set +a
  # Re-apply intentional overrides after source
  export PORT="${PORT:-8080}"
fi

echo "==> cargo run -p papr-server (PORT=$PORT)"
echo "    frontend: pnpm dev (proxy /api) OR pnpm build + PAPR_STATIC_DIR"
cd "$REPO_ROOT"
exec cargo run -p papr-server
