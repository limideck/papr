# scripts — package, deploy & service management

Ops helpers for **Papr Web** (`papr-server`). Production updates should **not**
compile on a Mac or on the server: GitHub Actions builds Linux amd64, attaches a
tarball to the Release, and `deploy.sh --from-release` downloads + rsyncs it.

**PM2 is not required** — `papr-server` is a Rust binary, not Node. Prefer
systemd on Linux, or `papr-ctl.sh` for a rootless start/stop/restart.

No Docker — the project ships a single static binary plus a Vite `dist/` tree.

## Production: tag → Actions → deploy

```bash
# 1) One-time: SSH target (gitignored — never commit secrets)
cp scripts/deploy.env.example scripts/deploy.env
# edit PAPR_DEPLOY_HOST=root@YOUR_IP
chmod 600 scripts/deploy.env

# 2) Cut a release (pushes tag → Release Server workflow on ubuntu-latest)
git tag v0.15.0
git push origin v0.15.0
# wait for .github/workflows/release-server.yml (asset: papr-linux-amd64-v0.15.0.tar.gz)

# 3) Ship from the Release (no local cargo/pnpm build)
scripts/deploy.sh --from-release v0.15.0
# or: scripts/deploy.sh --from-release latest
```

Or dry-run the path:

```bash
scripts/deploy.sh --dry-run --from-release latest
```

### GitHub permissions (one-time)

The **Release Server** workflow uses the default `GITHUB_TOKEN` with
`permissions: contents: write` — enough to create/update a Release and upload
`papr-linux-amd64-<tag>.tar.gz` in the same repo. No deploy SSH keys or secrets
belong in the repo; keep them in local `scripts/deploy.env`.

To re-run without a new tag: Actions → **Release Server** → Run workflow → enter
tag (e.g. `v0.15.0` or `v0.0.1`).

**Tag → workflow map**

| Tag / trigger | Workflow | Asset |
|---------------|----------|--------|
| `vX.Y.Z` (e.g. `v0.0.1`) | **Release Server** | `papr-linux-amd64-vX.Y.Z.tar.gz` |

There is no desktop/Tauri release workflow in CI. Deploy with
`scripts/deploy.sh --from-release vX.Y.Z` (or `latest`).

## Scripts

| Script | What it does |
|--------|--------------|
| `deploy.sh --from-release [tag|latest]` | **Production:** download Release tarball → sync → restart → health-check |
| `package.sh` | Local `cargo` + `pnpm` → `$PAPR_DEPLOY_ROOT` (macOS / matching-arch only) |
| `package-mac.sh` | **Mac local:** native darwin release + dist → `~/Deploy/papr-mac` |
| `run-mac.sh` | **Mac local:** run packaged binary (PORT default 8080, loads `.env`) |
| `dev.sh` | Debug `cargo run -p papr-server`; use with `pnpm dev` or `pnpm build` |
| `deploy.sh` | Local package, then rsync + remote restart (or local launchd) |
| `deploy.sh --restart-only` / `restart.sh` | Restart + health-check without rebuild |
| `deploy.sh --dry-run` | Print planned actions |
| `install-service.sh` | Write macOS launchd plist (does not start) |
| `uninstall-service.sh` | Stop + remove the launchd plist |
| `lib.sh` | Shared paths, SSH helpers, launchd + remote ctl |
| `papr-server.service` | systemd unit template (`/product/papr`) |
| `deploy.env.example` | Copy → `deploy.env` (gitignored) for host/path |

## Package layout

```
$PAPR_DEPLOY_ROOT/          # local package dir (default: ~/Deploy/papr)
  bin/papr-server
  bin/run-papr-server.sh    # loads .env then execs the binary
  bin/papr-ctl.sh           # start|stop|restart|status (PID file; no pm2)
  dist/
  data/
  run/                      # papr-server.pid
  logs/
  .env                      # secrets (chmod 600; never commit)
  .env.example
```

Release tarball (`papr-linux-amd64-<tag>.tar.gz`) contains `bin/`, `dist/`,
`.env.example`, and `papr-server.service` — never a real `.env`.

Production remote default: **`/product/papr`**, port **`7400`**.

## One-time setup (production Linux)

`/data/ssh/bryan` on this workstation is an **expect password login script**, not
an OpenSSH config. Do **not** point `PAPR_SSH_CONFIG` at it. Put host/path in an
untracked env file instead:

```bash
cp scripts/deploy.env.example scripts/deploy.env
# edit PAPR_DEPLOY_HOST=root@YOUR_IP  (and optional PAPR_SSH_PASSWORD)
chmod 600 scripts/deploy.env
```

Prefer SSH keys (no password in files):

```bash
ssh-copy-id -p 22 root@YOUR_SERVER_IP
```

If you must use a password: `brew install sshpass`, then set `PAPR_SSH_PASSWORD`
in `scripts/deploy.env` (gitignored) or export it for one shot.

### First ship (from Release)

```bash
scripts/deploy.sh --from-release latest
```

This downloads the linux amd64 tarball, syncs `bin/` + `dist/` to `/product/papr` (rsync, or tar-over-ssh if the server has no `rsync`),
seeds `.env` only if missing, restarts via systemd (if installed) or `papr-ctl.sh`,
then checks `http://127.0.0.1:7400/api/health` over SSH.

Edit remote secrets once:

```bash
ssh root@YOUR_SERVER_IP 'nano /product/papr/.env'   # set PAPR_ADMIN_PASSWORD
scripts/restart.sh
```

### Optional: systemd (recommended on Linux)

```bash
# on the server (after first ship):
sudo cp /product/papr/papr-server.service /etc/systemd/system/papr-server.service
sudo systemctl daemon-reload
sudo systemctl enable --now papr-server
curl -sf http://127.0.0.1:7400/api/health
```

Without systemd, `bin/papr-ctl.sh` manages a PID file under `run/`.

### Later updates

```bash
scripts/deploy.sh --from-release v0.16.0   # preferred
scripts/deploy.sh --from-release latest
scripts/restart.sh                         # restart only
```

Remote `.env` is **not** overwritten unless `PAPR_SYNC_ENV=1`.

**Do not** use plain `scripts/deploy.sh` (local `cargo`/`pnpm`) for production
unless you are on a Linux x86_64 builder that matches the server. A macOS arm64
binary will not run on a typical Linux VPS.

## Mac local package / run (not production)

Use these on your Mac to exercise the **same static `dist/` + release binary** shape
as production, without shipping a darwin binary to Linux.

```bash
scripts/package-mac.sh          # pnpm build + cargo --release → ~/Deploy/papr-mac
# edit ~/Deploy/papr-mac/.env   # created from .env.example on first run
scripts/run-mac.sh              # PORT=8080, PAPR_STATIC_DIR + PAPR_DB under package dir
# open http://127.0.0.1:8080
```

| | Mac local | Production Linux |
|--|-----------|------------------|
| Package | `scripts/package-mac.sh` → `~/Deploy/papr-mac` | GitHub Actions tarball |
| Ship | _(stays on Mac)_ | `scripts/deploy.sh --from-release [tag|latest]` |
| Binary | native darwin | linux amd64 from Release |
| Port default | **8080** | **7400** |
| Frontend | Vite `dist/` in package | same, from Release (or hot-sync `dist/` only) |

**Do not** rsync a Mac-built `bin/papr-server` to `/product/papr` — it will not run
on Linux. Production updates must use `--from-release`. Syncing **only** `dist/`
from a Mac `pnpm build` is fine when you need a UI hotfix without a new Release.

### Dev loop (API + Vite)

```bash
scripts/dev.sh                  # cargo run -p papr-server on :8080
# other terminal:
pnpm dev                        # :5173, proxies /api → :8080
```

Or serve the production build from the debug server: `pnpm build` then
`PAPR_STATIC_DIR=dist scripts/dev.sh`.

`pnpm dev` (Vite HMR) can look slightly different from the minified static build
served in production / `run-mac.sh` — that is expected, not a stale deploy.

## Local macOS (launchd)

Useful for local smoke tests via LaunchAgent — not for production Linux.

```bash
PORT=8080 scripts/install-service.sh   # once (use 8080 if you want Vite-dev parity)
# edit ~/Deploy/papr/.env
PAPR_DEPLOY_HOST= PORT=8080 scripts/deploy.sh
```

Default `PORT` in these scripts is **7400** (production). Override for local use.

If `scripts/deploy.env` sets `PAPR_DEPLOY_HOST`, `deploy.sh` always ships remote.
For local launchd only: `PAPR_DEPLOY_HOST= PORT=8080 scripts/deploy.sh`.

Unregister: `scripts/uninstall-service.sh`.

## Env knobs

| Env | Default | Meaning |
|-----|---------|---------|
| `PAPR_DEPLOY_ROOT` | `~/Deploy/papr` | Local package directory (`package.sh` / launchd) |
| `PAPR_MAC_ROOT` | `~/Deploy/papr-mac` | Mac local package (`package-mac.sh` / `run-mac.sh`) |
| `PAPR_DEPLOY_HOST` | _(from deploy.env)_ | If set, ship via rsync |
| `PAPR_DEPLOY_PATH` | `/product/papr` | Remote directory |
| `PAPR_GH_REPO` | _(detect via gh/git)_ | `owner/name` for Release download |
| `PORT` | `7400` | HTTP port |
| `PAPR_SSH_CONFIG` | _(empty)_ | `ssh -F` OpenSSH config |
| `PAPR_SSH_PORT` | _(empty)_ | `ssh -p` |
| `PAPR_SSH_PASSWORD` | _(empty)_ | `sshpass -e` (prefer keys) |
| `PAPR_SYNC_ENV` | `0` | Set `1` to overwrite remote `.env` |
| `PAPR_LAUNCHD_LABEL` | `com.papr.server` | LaunchAgent label |

### `.env` (on the package / server)

| Variable | Purpose |
|----------|---------|
| `PAPR_DB` | SQLite path |
| `PORT` | Listen port |
| `PAPR_STATIC_DIR` | Frontend dir |
| `PAPR_ADMIN_USER` / `PAPR_ADMIN_PASSWORD` | Admin seed |
| `PAPR_ADMIN_RESET` | `1` to reset admin password on startup |
| `PAPR_WORDCLOUD_DIR` | Optional override for wordcloud JSON dir (non-seed = read/write) |
| `PAPR_WORDCLOUD_COW_DIR` | Optional COW overlay dir (default: `{dirname(PAPR_DB)}/wordcloud`) |
| `RUST_LOG` | Tracing filter |

Never commit a real `.env` or `scripts/deploy.env`.

## Fresh-box checklist

1. `scripts/deploy.env` from the example (host → `/product/papr`, port 7400).
2. SSH keys (or sshpass + password in deploy.env only).
3. Tag + push → wait for **Release Server** asset on GitHub Releases.
4. `scripts/deploy.sh --from-release vX.Y.Z` — then edit remote `.env` admin password.
5. Optionally install `papr-server.service` via systemd.
6. Confirm `GET /api/health` on `:7400`.
