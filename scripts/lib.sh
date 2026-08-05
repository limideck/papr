#!/usr/bin/env bash
# Shared config + helpers for papr-server deploy/service scripts.
# Override via environment or an untracked scripts/deploy.env (never commit secrets).

# Repo paths — resolved when this file is sourced (not from inside functions).
_PAPR_SCRIPTS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
_PAPR_REPO_ROOT="$(cd "$_PAPR_SCRIPTS_DIR/.." && pwd)"

# --- Load local untracked deploy env (host/path/password stay off git) ---------
# Prefer scripts/deploy.env; also accept repo-root .env.deploy.
_load_deploy_env() {
  local f
  for f in \
    "$_PAPR_SCRIPTS_DIR/deploy.env" \
    "$_PAPR_REPO_ROOT/.env.deploy" \
    "$_PAPR_REPO_ROOT/deploy.env"; do
    if [ -f "$f" ]; then
      set -a
      # shellcheck disable=SC1090
      source "$f"
      set +a
      return 0
    fi
  done
}
_load_deploy_env

# --- Shared config -----------------------------------------------------------
# Local package directory (binary + frontend + data + logs).
DEPLOY_ROOT="${PAPR_DEPLOY_ROOT:-${DEPLOY_ROOT:-$HOME/Deploy/papr}}"
LABEL="${PAPR_LAUNCHD_LABEL:-com.papr.server}"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
# Production default listen port (override with PORT= for local launchd / Vite).
PORT="${PORT:-7400}"

BIN_DIR="$DEPLOY_ROOT/bin"
BIN="$BIN_DIR/papr-server"
RUNNER="$BIN_DIR/run-papr-server.sh"
CTL="$BIN_DIR/papr-ctl.sh"
STATIC_DIR="$DEPLOY_ROOT/dist"
DATA_DIR="$DEPLOY_ROOT/data"
RUN_DIR="$DEPLOY_ROOT/run"
DB_PATH="${PAPR_DB:-$DATA_DIR/papr.db}"
LOG_DIR="$DEPLOY_ROOT/logs"
ENV_FILE="$DEPLOY_ROOT/.env"
ENV_EXAMPLE="$DEPLOY_ROOT/.env.example"

# Remote ship target (used by deploy.sh / restart.sh).
#   PAPR_DEPLOY_HOST=user@host   (or DEPLOY_HOST / SSH Host alias)
#   PAPR_DEPLOY_PATH=/product/papr
DEPLOY_HOST="${PAPR_DEPLOY_HOST:-${DEPLOY_HOST:-}}"
DEPLOY_PATH="${PAPR_DEPLOY_PATH:-${DEPLOY_PATH:-/product/papr}}"

# SSH knobs (optional):
#   PAPR_SSH_CONFIG=/path/to/ssh_config   → ssh -F … (must be OpenSSH config, not expect)
#   PAPR_SSH_PORT=22
#   PAPR_SSH_PASSWORD=…                   → sshpass -e (never commit)
PAPR_SSH_CONFIG="${PAPR_SSH_CONFIG:-}"
PAPR_SSH_PORT="${PAPR_SSH_PORT:-}"
PAPR_SSH_PASSWORD="${PAPR_SSH_PASSWORD:-}"

# SSH_OPTS — filled by build_ssh_opts (array of extra ssh/rsync-ssh flags).
SSH_OPTS=()

# build_ssh_opts — populate SSH_OPTS from PAPR_SSH_*.
build_ssh_opts() {
  SSH_OPTS=()
  if [ -n "$PAPR_SSH_CONFIG" ] && [ -f "$PAPR_SSH_CONFIG" ]; then
    if head -n 1 "$PAPR_SSH_CONFIG" | grep -qiE '^#!.*expect'; then
      echo "note: $PAPR_SSH_CONFIG is an expect login script, not OpenSSH config — ignoring -F" >&2
      echo "      use scripts/deploy.env (PAPR_DEPLOY_HOST=user@host) + SSH keys or PAPR_SSH_PASSWORD" >&2
    else
      SSH_OPTS+=(-F "$PAPR_SSH_CONFIG")
    fi
  fi
  if [ -n "$PAPR_SSH_PORT" ]; then
    SSH_OPTS+=(-p "$PAPR_SSH_PORT")
  fi
  SSH_OPTS+=(-o BatchMode=no -o StrictHostKeyChecking=accept-new)
}

# remote_ssh ARGS… — ssh to DEPLOY_HOST (or first arg if it looks like a host).
remote_ssh() {
  build_ssh_opts
  if [ -n "$PAPR_SSH_PASSWORD" ]; then
    if ! command -v sshpass >/dev/null 2>&1; then
      echo "error: PAPR_SSH_PASSWORD is set but sshpass is not installed" >&2
      echo "       brew install sshpass   # or: apt install sshpass" >&2
      echo "       (preferred: ssh-copy-id and drop the password)" >&2
      return 1
    fi
    SSHPASS="$PAPR_SSH_PASSWORD" sshpass -e ssh "${SSH_OPTS[@]}" "$@"
  else
    ssh "${SSH_OPTS[@]}" "$@"
  fi
}

# remote_rsync RSYNC_ARGS… — rsync over the same SSH transport as remote_ssh.
remote_rsync() {
  build_ssh_opts
  local rsh
  if [ -n "$PAPR_SSH_PASSWORD" ]; then
    if ! command -v sshpass >/dev/null 2>&1; then
      echo "error: PAPR_SSH_PASSWORD is set but sshpass is not installed" >&2
      return 1
    fi
    # Expand opts safely into the remote shell command string.
    rsh="sshpass -e ssh"
    local o
    for o in "${SSH_OPTS[@]}"; do
      rsh+=" $(printf '%q' "$o")"
    done
    SSHPASS="$PAPR_SSH_PASSWORD" rsync -e "$rsh" "$@"
  else
    rsh="ssh"
    local o
    for o in "${SSH_OPTS[@]}"; do
      rsh+=" $(printf '%q' "$o")"
    done
    rsync -e "$rsh" "$@"
  fi
}
# remote_has_rsync — 0 if remote PATH has rsync (cached per shell).
_REMOTE_HAS_RSYNC=""
remote_has_rsync() {
  if [ -n "$_REMOTE_HAS_RSYNC" ]; then
    [ "$_REMOTE_HAS_RSYNC" = "1" ]
    return
  fi
  if remote_ssh "$DEPLOY_HOST" "command -v rsync >/dev/null 2>&1"; then
    _REMOTE_HAS_RSYNC=1
    return 0
  fi
  _REMOTE_HAS_RSYNC=0
  return 1
}

# remote_sync_dir LOCAL_DIR REMOTE_DIR — mirror a tree (delete extras).
# Prefers rsync; falls back to tar-over-ssh when the server has no rsync binary.
remote_sync_dir() {
  local src="$1"
  local dest="$2"
  if [ ! -d "$src" ]; then
    echo "error: remote_sync_dir: not a directory: $src" >&2
    return 1
  fi
  if remote_has_rsync; then
    remote_rsync -az --delete \
      --exclude '._*' --exclude '.DS_Store' \
      "$src/" "$DEPLOY_HOST:$dest/"
  else
    echo "    note: remote has no rsync — tar-over-ssh → $dest"
    # COPYFILE_DISABLE avoids AppleDouble noise from macOS tar.
    COPYFILE_DISABLE=1 tar -C "$src" -cf - \
      --exclude '._*' --exclude '.DS_Store' \
      . | remote_ssh "$DEPLOY_HOST" \
      "rm -rf '$dest' && mkdir -p '$dest' && tar -C '$dest' -xf - && find '$dest' -name '._*' -delete 2>/dev/null || true"
  fi
}

# remote_put_file LOCAL_FILE REMOTE_FILE — copy one file.
remote_put_file() {
  local src="$1"
  local dest="$2"
  if [ ! -f "$src" ]; then
    echo "error: remote_put_file: missing $src" >&2
    return 1
  fi
  if remote_has_rsync; then
    remote_rsync -az "$src" "$DEPLOY_HOST:$dest"
    return
  fi
  build_ssh_opts
  # ssh uses -p for port; scp uses -P.
  local scp_opts=()
  local i=0
  while [ $i -lt ${#SSH_OPTS[@]} ]; do
    if [ "${SSH_OPTS[$i]}" = "-p" ]; then
      i=$((i + 1))
      scp_opts+=(-P "${SSH_OPTS[$i]}")
    else
      scp_opts+=("${SSH_OPTS[$i]}")
    fi
    i=$((i + 1))
  done
  if [ -n "$PAPR_SSH_PASSWORD" ]; then
    SSHPASS="$PAPR_SSH_PASSWORD" sshpass -e scp "${scp_opts[@]}" "$src" "$DEPLOY_HOST:$dest"
  else
    scp "${scp_opts[@]}" "$src" "$DEPLOY_HOST:$dest"
  fi
}



# ensure_deploy_dirs — create the package layout under DEPLOY_ROOT.
ensure_deploy_dirs() {
  mkdir -p "$BIN_DIR" "$STATIC_DIR" "$DATA_DIR" "$LOG_DIR" "$RUN_DIR"
}

# install_env_example — refresh non-secret template; production PORT default.
install_env_example() {
  local example
  example="$_PAPR_REPO_ROOT/crates/papr-server/.env.example"
  if [ -f "$example" ]; then
    cp -f "$example" "$ENV_EXAMPLE"
  else
    cat > "$ENV_EXAMPLE" <<ENVEOF
PAPR_DB=./data/papr.db
PORT=7400
PAPR_STATIC_DIR=./dist
PAPR_ADMIN_USER=admin
PAPR_ADMIN_PASSWORD=changeme
ENVEOF
  fi
  # Seed production-oriented defaults into the packaged example (local Vite still uses crates/ copy at 8080).
  if grep -q '^PORT=' "$ENV_EXAMPLE" 2>/dev/null; then
    sed -i.bak "s|^PORT=.*|PORT=$PORT|" "$ENV_EXAMPLE" && rm -f "$ENV_EXAMPLE.bak"
  else
    echo "PORT=$PORT" >> "$ENV_EXAMPLE"
  fi
}

# write_runner — loads .env then execs papr-server (paths relative to package root).
write_runner() {
  ensure_deploy_dirs
  cat > "$RUNNER" <<'RUNEOF'
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [ -f "$ROOT/.env" ]; then
  set -a
  # shellcheck disable=SC1091
  source "$ROOT/.env"
  set +a
fi
export PAPR_DB="${PAPR_DB:-$ROOT/data/papr.db}"
export PAPR_STATIC_DIR="${PAPR_STATIC_DIR:-$ROOT/dist}"
export PORT="${PORT:-7400}"
export RUST_LOG="${RUST_LOG:-info}"
exec "$ROOT/bin/papr-server"
RUNEOF
  chmod +x "$RUNNER"
}

# write_ctl — PID-file start/stop/restart/status (no pm2 required).
write_ctl() {
  ensure_deploy_dirs
  cat > "$CTL" <<'CTLEOF'
#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PIDFILE="${PAPR_PIDFILE:-$ROOT/run/papr-server.pid}"
LOGFILE="${PAPR_LOGFILE:-$ROOT/logs/server.log}"
RUNNER="$ROOT/bin/run-papr-server.sh"
mkdir -p "$(dirname "$PIDFILE")" "$(dirname "$LOGFILE")"

is_running() {
  if [ -f "$PIDFILE" ]; then
    local pid
    pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      return 0
    fi
  fi
  return 1
}

do_stop() {
  if ! is_running; then
    rm -f "$PIDFILE"
    echo "papr-server: not running"
    return 0
  fi
  local pid
  pid="$(cat "$PIDFILE")"
  echo "papr-server: stopping pid $pid"
  kill "$pid" 2>/dev/null || true
  local i
  for i in $(seq 1 20); do
    kill -0 "$pid" 2>/dev/null || break
    sleep 0.25
  done
  if kill -0 "$pid" 2>/dev/null; then
    echo "papr-server: force kill $pid"
    kill -9 "$pid" 2>/dev/null || true
  fi
  rm -f "$PIDFILE"
}

do_start() {
  if is_running; then
    echo "papr-server: already running (pid $(cat "$PIDFILE"))"
    return 0
  fi
  if [ ! -x "$RUNNER" ]; then
    echo "error: missing $RUNNER" >&2
    return 1
  fi
  if [ ! -x "$ROOT/bin/papr-server" ]; then
    echo "error: missing $ROOT/bin/papr-server" >&2
    return 1
  fi
  # Prefer systemd when the unit is active/enabled (root or user).
  if command -v systemctl >/dev/null 2>&1; then
    if systemctl is-active --quiet papr-server 2>/dev/null \
      || systemctl is-enabled --quiet papr-server 2>/dev/null; then
      echo "papr-server: restarting via systemctl"
      systemctl restart papr-server
      return 0
    fi
    if systemctl --user is-active --quiet papr-server 2>/dev/null \
      || systemctl --user is-enabled --quiet papr-server 2>/dev/null; then
      echo "papr-server: restarting via systemctl --user"
      systemctl --user restart papr-server
      return 0
    fi
  fi
  echo "papr-server: starting (logs → $LOGFILE)"
  nohup "$RUNNER" >>"$LOGFILE" 2>&1 &
  echo $! >"$PIDFILE"
  sleep 0.5
  if is_running; then
    echo "papr-server: started pid $(cat "$PIDFILE")"
  else
    echo "error: papr-server failed to stay up — see $LOGFILE" >&2
    return 1
  fi
}

cmd="${1:-}"
case "$cmd" in
  start) do_start ;;
  stop) do_stop ;;
  restart) do_stop; do_start ;;
  status)
    if is_running; then
      echo "papr-server: running pid $(cat "$PIDFILE")"
      exit 0
    fi
    echo "papr-server: stopped"
    exit 1
    ;;
  *)
    echo "usage: $(basename "$0") {start|stop|restart|status}" >&2
    exit 2
    ;;
esac
CTLEOF
  chmod +x "$CTL"
}

# seed_env_file — copy the example env if none exists yet (placeholders only).
seed_env_file() {
  local example
  example="$_PAPR_REPO_ROOT/crates/papr-server/.env.example"
  install_env_example
  if [ -f "$ENV_FILE" ]; then
    return 0
  fi
  if [ -f "$example" ]; then
    cp "$example" "$ENV_FILE"
    if grep -q '^PAPR_DB=' "$ENV_FILE" 2>/dev/null; then
      sed -i.bak "s|^PAPR_DB=.*|PAPR_DB=$DB_PATH|" "$ENV_FILE" && rm -f "$ENV_FILE.bak"
    else
      echo "PAPR_DB=$DB_PATH" >> "$ENV_FILE"
    fi
    if grep -q '^# PAPR_STATIC_DIR=' "$ENV_FILE" 2>/dev/null; then
      sed -i.bak "s|^# PAPR_STATIC_DIR=.*|PAPR_STATIC_DIR=$STATIC_DIR|" "$ENV_FILE" && rm -f "$ENV_FILE.bak"
    elif grep -q '^PAPR_STATIC_DIR=' "$ENV_FILE" 2>/dev/null; then
      sed -i.bak "s|^PAPR_STATIC_DIR=.*|PAPR_STATIC_DIR=$STATIC_DIR|" "$ENV_FILE" && rm -f "$ENV_FILE.bak"
    else
      echo "PAPR_STATIC_DIR=$STATIC_DIR" >> "$ENV_FILE"
    fi
    if grep -q '^PORT=' "$ENV_FILE" 2>/dev/null; then
      sed -i.bak "s|^PORT=.*|PORT=$PORT|" "$ENV_FILE" && rm -f "$ENV_FILE.bak"
    fi
    chmod 600 "$ENV_FILE"
    echo "    seeded $ENV_FILE from .env.example — edit secrets before production use"
  else
    cat > "$ENV_FILE" <<ENVEOF
PAPR_DB=$DB_PATH
PORT=$PORT
PAPR_STATIC_DIR=$STATIC_DIR
PAPR_ADMIN_USER=admin
PAPR_ADMIN_PASSWORD=changeme
ENVEOF
    chmod 600 "$ENV_FILE"
    echo "    wrote placeholder $ENV_FILE — edit secrets before production use"
  fi
}

# seed_remote_env — create DEPLOY_PATH/.env on the host if missing (relative paths).
seed_remote_env() {
  if [ -z "$DEPLOY_HOST" ]; then
    return 0
  fi
  remote_ssh "$DEPLOY_HOST" "test -f '$DEPLOY_PATH/.env'" 2>/dev/null && return 0
  echo "    seeding remote .env at $DEPLOY_PATH/.env (edit secrets on the host)"
  remote_ssh "$DEPLOY_HOST" "cat > '$DEPLOY_PATH/.env' <<'ENVEOF'
PAPR_DB=$DEPLOY_PATH/data/papr.db
PORT=$PORT
PAPR_STATIC_DIR=$DEPLOY_PATH/dist
PAPR_ADMIN_USER=admin
PAPR_ADMIN_PASSWORD=changeme
# PAPR_ADMIN_RESET=0
RUST_LOG=info
ENVEOF
chmod 600 '$DEPLOY_PATH/.env'"
}

# remote_restart — systemd if available, else papr-ctl.sh PID manager.
remote_restart() {
  if [ -z "$DEPLOY_HOST" ]; then
    echo "error: PAPR_DEPLOY_HOST not set" >&2
    return 1
  fi
  echo "==> restart on $DEPLOY_HOST:$DEPLOY_PATH"
  remote_ssh "$DEPLOY_HOST" "bash -s" <<REMOTE
set -euo pipefail
cd '$DEPLOY_PATH'
if command -v systemctl >/dev/null 2>&1; then
  if systemctl is-active --quiet papr-server 2>/dev/null \
    || systemctl is-enabled --quiet papr-server 2>/dev/null; then
    if [ "\$(id -u)" = "0" ]; then
      systemctl restart papr-server
    else
      sudo systemctl restart papr-server
    fi
    exit 0
  fi
  if systemctl --user is-active --quiet papr-server 2>/dev/null \
    || systemctl --user is-enabled --quiet papr-server 2>/dev/null; then
    systemctl --user restart papr-server
    exit 0
  fi
fi
if [ -x bin/papr-ctl.sh ]; then
  bin/papr-ctl.sh restart
  exit 0
fi
echo "error: no systemd unit and missing bin/papr-ctl.sh" >&2
exit 1
REMOTE
}

# remote_health_check — curl /api/health on the remote loopback.
remote_health_check() {
  local url="http://127.0.0.1:${PORT}/api/health"
  echo "==> remote health check ($url)"
  remote_ssh "$DEPLOY_HOST" "bash -s" <<REMOTE
set -euo pipefail
sleep 2
for i in \$(seq 1 20); do
  if curl -sf '$url' >/dev/null 2>&1; then
    echo "OK: healthy"
    exit 0
  fi
  sleep 0.5
done
echo "!! HEALTH CHECK FAILED" >&2
exit 1
REMOTE
}

# reload_service LABEL PLIST — bootout + bootstrap + kickstart (macOS launchd).
reload_service() {
  local label="$1" plist="$2" uid domain i
  uid="$(id -u)"
  domain="gui/$uid"

  launchctl bootout "$domain/$label" 2>/dev/null || true
  for i in $(seq 1 20); do
    launchctl print "$domain/$label" >/dev/null 2>&1 || break
    sleep 0.5
  done

  for i in $(seq 1 6); do
    if launchctl bootstrap "$domain" "$plist" 2>/dev/null; then
      launchctl kickstart -k "$domain/$label" 2>/dev/null || true
      return 0
    fi
    sleep 1
  done

  echo "!! launchctl bootstrap failed after retries for $label" >&2
  return 1
}

# kickstart_service LABEL — restart an already-bootstrapped job in place.
kickstart_service() {
  local label="$1"
  launchctl kickstart -k "gui/$(id -u)/$label"
}

# health_check URL — poll until 2xx (up to ~12s). 0 = healthy.
health_check() {
  local url="$1" i
  sleep 2
  for i in $(seq 1 20); do
    if curl -sf "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.5
  done
  return 1
}
