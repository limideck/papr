#!/usr/bin/env bash
set -euo pipefail

# Package papr-server, then either:
#   1) Download a GitHub Release linux amd64 tarball and ship (production), or
#   2) Build locally, ship to a remote host (PAPR_DEPLOY_HOST) + restart, or
#   3) Reload the local macOS launchd service if installed, or
#   4) Print next steps if neither applies.
#
# Usage:
#   scripts/deploy.sh --from-release [tag|latest]   # production (no local compile)
#   scripts/deploy.sh                               # local package + optional rsync
#   scripts/deploy.sh --restart-only                # remote/local restart, no rebuild
#   scripts/deploy.sh --dry-run --from-release latest
#
# Configure host/path in an untracked scripts/deploy.env (see deploy.env.example).
# Remote sync never overwrites an existing remote .env unless PAPR_SYNC_ENV=1.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=lib.sh
source "$SCRIPT_DIR/lib.sh"

RESTART_ONLY=0
FROM_RELEASE=""
DRY_RUN=0
POSITIONAL=()

usage() {
  cat <<USAGE
Usage: scripts/deploy.sh [options] [--from-release [tag|latest]]

  Production (recommended):
    --from-release [tag|latest]
        Download papr-linux-amd64-*.tar.gz from GitHub Releases (via gh or curl),
        rsync to PAPR_DEPLOY_PATH, restart, health-check. No compile on Mac/server.

  Local package (macOS launchd / matching-arch builder):
    (default)  package.sh → optional rsync → restart → health-check

  Other:
    --restart-only, -r   skip download/build/rsync; restart + health-check only
    --dry-run            print actions without downloading/rsync/restart
    -h, --help           this help

Env (or scripts/deploy.env):
  PAPR_DEPLOY_HOST   user@host or SSH Host alias (required for remote ship)
  PAPR_DEPLOY_PATH   remote dir (default /product/papr)
  PORT               listen port (default 7400)
  PAPR_GH_REPO       owner/name (default: detect via gh/git remote)
  PAPR_SSH_CONFIG    OpenSSH config file for ssh -F (not expect scripts)
  PAPR_SSH_PORT      ssh -p
  PAPR_SSH_PASSWORD  optional; requires sshpass (prefer SSH keys)
  PAPR_SYNC_ENV=1    also sync .env to remote (destructive to remote secrets)

One-time GitHub: workflow needs permissions.contents: write (default GITHUB_TOKEN
is enough for same-repo Releases). Locally: \`gh auth login\` to download private
releases, or public assets work unauthenticated via the API.
USAGE
}

while [ $# -gt 0 ]; do
  case "$1" in
    --restart-only|-r) RESTART_ONLY=1; shift ;;
    --from-release)
      shift
      if [ $# -gt 0 ] && [[ "$1" != -* ]]; then
        FROM_RELEASE="$1"
        shift
      else
        FROM_RELEASE="latest"
      fi
      ;;
    --dry-run) DRY_RUN=1; shift ;;
    -h|--help) usage; exit 0 ;;
    --)
      shift
      POSITIONAL+=("$@")
      break
      ;;
    -*)
      echo "unknown arg: $1 (try --help)" >&2
      exit 2
      ;;
    *)
      POSITIONAL+=("$1")
      shift
      ;;
  esac
done

# Allow: scripts/deploy.sh --from-release   OR   scripts/deploy.sh v0.15.0 as tag after flag already set
if [ ${#POSITIONAL[@]} -gt 0 ]; then
  echo "unknown arg: ${POSITIONAL[0]} (try --help)" >&2
  exit 2
fi

if [ "$RESTART_ONLY" = "1" ] && [ -n "$FROM_RELEASE" ]; then
  echo "error: use either --restart-only or --from-release, not both" >&2
  exit 2
fi

# resolve_gh_repo — owner/name for Releases API / gh.
resolve_gh_repo() {
  if [ -n "${PAPR_GH_REPO:-}" ]; then
    echo "$PAPR_GH_REPO"
    return 0
  fi
  if command -v gh >/dev/null 2>&1; then
    local r
    r="$(gh repo view --json nameWithOwner -q .nameWithOwner 2>/dev/null || true)"
    if [ -n "$r" ]; then
      echo "$r"
      return 0
    fi
  fi
  local url
  url="$(git -C "$_PAPR_REPO_ROOT" remote get-url origin 2>/dev/null || true)"
  # git@github.com:owner/repo.git  or  https://github.com/owner/repo.git
  if [[ "$url" =~ github\.com[:/]([^/]+)/([^/.]+)(\.git)?$ ]]; then
    echo "${BASH_REMATCH[1]}/${BASH_REMATCH[2]}"
    return 0
  fi
  echo "l0ng-ai/papr"
}

# download_release_asset TAG_OR_LATEST DEST_DIR — fetches papr-linux-amd64-*.tar.gz
# Status on stderr; absolute path of the tarball on stdout.
download_release_asset() {
  local tag="$1" dest="$2" repo pattern asset
  repo="$(resolve_gh_repo)"
  pattern='papr-linux-amd64-*.tar.gz'
  mkdir -p "$dest"

  if [ "$DRY_RUN" = "1" ]; then
    echo "[dry-run] would download $pattern from $repo release=$tag → $dest" >&2
    return 0
  fi

  if command -v gh >/dev/null 2>&1; then
    echo "==> gh release download ($repo) tag=$tag pattern=$pattern" >&2
    if [ "$tag" = "latest" ]; then
      gh release download --repo "$repo" --pattern "$pattern" --dir "$dest" --clobber
    else
      gh release download "$tag" --repo "$repo" --pattern "$pattern" --dir "$dest" --clobber
    fi
  else
    echo "==> curl GitHub API ($repo) tag=$tag" >&2
    local api_url json browser_url
    if [ "$tag" = "latest" ]; then
      api_url="https://api.github.com/repos/${repo}/releases/latest"
    else
      api_url="https://api.github.com/repos/${repo}/releases/tags/${tag}"
    fi
    json="$(curl -fsSL "$api_url")"
    browser_url="$(printf '%s' "$json" | python3 -c '
import json,sys,re
data=json.load(sys.stdin)
pat=re.compile(r"^papr-linux-amd64-.*\.tar\.gz$")
for a in data.get("assets") or []:
  if pat.match(a.get("name") or ""):
    print(a["browser_download_url"]); break
else:
  sys.exit("no papr-linux-amd64-*.tar.gz asset on this release")
')"
    asset="$(basename "$browser_url")"
    echo "    → $asset" >&2
    curl -fsSL -o "$dest/$asset" -L "$browser_url"
  fi

  local found
  found="$(find "$dest" -maxdepth 1 -name 'papr-linux-amd64-*.tar.gz' | head -n 1)"
  if [ -z "$found" ]; then
    echo "error: no papr-linux-amd64-*.tar.gz downloaded into $dest" >&2
    echo "       ensure Release Server workflow published the asset for this tag" >&2
    return 1
  fi
  echo "    downloaded $(basename "$found")" >&2
  printf '%s\n' "$found"
}

# ship_staging_to_remote STAGING_DIR — rsync bin/dist/service/example; preserve remote .env
ship_staging_to_remote() {
  local staging="$1"
  if [ -z "$DEPLOY_HOST" ]; then
    echo "error: PAPR_DEPLOY_HOST not set (copy scripts/deploy.env.example → scripts/deploy.env)" >&2
    return 1
  fi

  echo "==> ship → $DEPLOY_HOST:$DEPLOY_PATH"
  if [ "$DRY_RUN" = "1" ]; then
    echo "[dry-run] would mkdir remote dirs, rsync bin/ + dist/ from $staging,"
    echo "          seed .env if missing, restart, health-check :$PORT"
    return 0
  fi

  if [ ! -x "$staging/bin/papr-server" ]; then
    echo "error: missing $staging/bin/papr-server" >&2
    return 1
  fi
  if [ ! -d "$staging/dist" ]; then
    echo "error: missing $staging/dist" >&2
    return 1
  fi

  remote_ssh "$DEPLOY_HOST" "mkdir -p '$DEPLOY_PATH/bin' '$DEPLOY_PATH/dist' '$DEPLOY_PATH/data' '$DEPLOY_PATH/logs' '$DEPLOY_PATH/run'"

  # Prefer rsync; lib.sh falls back to tar/scp when the server has no rsync.
  remote_sync_dir "$staging/bin" "$DEPLOY_PATH/bin"
  remote_sync_dir "$staging/dist" "$DEPLOY_PATH/dist"

  if [ -f "$staging/.env.example" ]; then
    remote_put_file "$staging/.env.example" "$DEPLOY_PATH/.env.example"
  elif [ -f "$ENV_EXAMPLE" ]; then
    remote_put_file "$ENV_EXAMPLE" "$DEPLOY_PATH/.env.example"
  fi

  if [ "${PAPR_SYNC_ENV:-0}" = "1" ] && [ -f "$ENV_FILE" ]; then
    echo "    PAPR_SYNC_ENV=1 — will sync .env"
    remote_put_file "$ENV_FILE" "$DEPLOY_PATH/.env"
    remote_ssh "$DEPLOY_HOST" "chmod 600 '$DEPLOY_PATH/.env'"
  else
    seed_remote_env
  fi

  if [ -f "$staging/papr-server.service" ]; then
    remote_put_file "$staging/papr-server.service" "$DEPLOY_PATH/papr-server.service"
  elif [ -f "$SCRIPT_DIR/papr-server.service" ]; then
    remote_put_file "$SCRIPT_DIR/papr-server.service" "$DEPLOY_PATH/papr-server.service"
  fi

  echo "OK: shipped to $DEPLOY_HOST:$DEPLOY_PATH"
  remote_restart
  remote_health_check
  echo "OK: papr-server live on $DEPLOY_HOST:$PORT"
}

# --- --from-release ----------------------------------------------------------
if [ -n "$FROM_RELEASE" ]; then
  if [ -z "$DEPLOY_HOST" ] && [ "$DRY_RUN" != "1" ]; then
    echo "error: --from-release requires PAPR_DEPLOY_HOST" >&2
    echo "       cp scripts/deploy.env.example scripts/deploy.env && edit host" >&2
    exit 1
  fi

  TMP="$(mktemp -d "${TMPDIR:-/tmp}/papr-release.XXXXXX")"
  cleanup() { rm -rf "$TMP"; }
  trap cleanup EXIT

  echo "==> deploy from GitHub Release ($FROM_RELEASE)"
  echo "    repo=$(resolve_gh_repo) → $DEPLOY_HOST:$DEPLOY_PATH"

  if [ "$DRY_RUN" = "1" ]; then
    download_release_asset "$FROM_RELEASE" "$TMP" >/dev/null || true
    echo "[dry-run] extract tarball → stage → rsync → restart → :$PORT/api/health"
    ship_staging_to_remote "$TMP/stage"
    exit 0
  fi

  ASSET_PATH="$(download_release_asset "$FROM_RELEASE" "$TMP")"
  STAGE="$TMP/stage"
  mkdir -p "$STAGE"
  echo "==> extract $(basename "$ASSET_PATH")"
  tar -xzf "$ASSET_PATH" -C "$STAGE"
  ship_staging_to_remote "$STAGE"
  exit 0
fi

# --- --restart-only ----------------------------------------------------------
if [ "$RESTART_ONLY" = "1" ]; then
  if [ -n "$DEPLOY_HOST" ]; then
    if [ "$DRY_RUN" = "1" ]; then
      echo "[dry-run] would remote_restart + remote_health_check on $DEPLOY_HOST:$DEPLOY_PATH"
      exit 0
    fi
    remote_restart
    remote_health_check
    exit 0
  fi
  if [ -f "$PLIST" ]; then
    echo "==> kickstart launchd ($LABEL)"
    if [ "$DRY_RUN" = "1" ]; then
      echo "[dry-run] would kickstart $LABEL and health-check :$PORT"
      exit 0
    fi
    kickstart_service "$LABEL"
    echo "==> health check (http://127.0.0.1:$PORT/api/health)"
    if health_check "http://127.0.0.1:$PORT/api/health"; then
      echo "OK: papr-server live on :$PORT"
      exit 0
    fi
    echo "!! HEALTH CHECK FAILED — inspect $LOG_DIR/server.log" >&2
    exit 1
  fi
  echo "error: --restart-only needs PAPR_DEPLOY_HOST or a local launchd plist" >&2
  exit 1
fi

# --- local package path ------------------------------------------------------
if [ "$DRY_RUN" = "1" ]; then
  echo "[dry-run] would run scripts/package.sh → $DEPLOY_ROOT"
  if [ -n "$DEPLOY_HOST" ]; then
    echo "[dry-run] would rsync to $DEPLOY_HOST:$DEPLOY_PATH, restart, health-check"
  elif [ -f "$PLIST" ]; then
    echo "[dry-run] would reload launchd $LABEL"
  fi
  exit 0
fi

"$SCRIPT_DIR/package.sh"

if [ -n "$DEPLOY_HOST" ]; then
  # Reuse the same ship path as --from-release (local package dir as staging).
  ship_staging_to_remote "$DEPLOY_ROOT"
  exit 0
fi

if [ -f "$PLIST" ]; then
  echo "==> bootstrap and start launchd service ($LABEL)"
  reload_service "$LABEL" "$PLIST"

  echo "==> health check (http://127.0.0.1:$PORT/api/health)"
  if health_check "http://127.0.0.1:$PORT/api/health"; then
    echo "OK: papr-server live on :$PORT"
    echo "    logs: $LOG_DIR/server.log"
    exit 0
  fi
  echo "!! HEALTH CHECK FAILED — inspect $LOG_DIR/server.log"
  exit 1
fi

echo "OK: package built at $DEPLOY_ROOT (no service reload)"
echo "    production:   scripts/deploy.sh --from-release latest"
echo "    local macOS:  scripts/install-service.sh && scripts/deploy.sh"
echo "    remote ship:  copy scripts/deploy.env.example → scripts/deploy.env, then scripts/deploy.sh"
