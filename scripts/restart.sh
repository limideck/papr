#!/usr/bin/env bash
set -euo pipefail

# Restart papr-server without rebuilding (remote or local launchd).
# Equivalent to: scripts/deploy.sh --restart-only

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "$SCRIPT_DIR/deploy.sh" --restart-only "$@"
