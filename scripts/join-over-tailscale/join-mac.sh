#!/usr/bin/env bash
# Set up Buzz on an Apple Silicon Mac and connect to a host's relay over Tailscale.
#
# Builds the desktop app from a pinned branch (same toolchain the host uses via
# Hermit), so later pushes to main never change what this Mac runs.
#
# Usage:
#   join-mac.sh --host <tailscale-magicdns-name> [--port 3000] [--dir ~/buzz]
#   join-mac.sh --start      # relaunch the app after the first setup
set -euo pipefail

REPO_URL="${BUZZ_REPO_URL:-https://github.com/kristofferrullar/buzz.git}"
PINNED_REF="${BUZZ_PINNED_REF:-claude/mac-onboarding-script}"
INSTALL_DIR="${HOME}/buzz"
RELAY_HOST=""
RELAY_PORT="3000"
START_ONLY=false

die() { echo "Error: $*" >&2; exit 1; }
step() { printf '\n==> %s\n' "$*"; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --host) RELAY_HOST="${2:?--host needs a value}"; shift 2 ;;
        --port) RELAY_PORT="${2:?--port needs a value}"; shift 2 ;;
        --dir) INSTALL_DIR="${2:?--dir needs a value}"; shift 2 ;;
        --start) START_ONLY=true; shift ;;
        -h | --help) echo "Usage: join-mac.sh --host <tailscale-name> [--port 3000] [--dir ~/buzz] | --start"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

launch() {
    cd "$INSTALL_DIR"
    # shellcheck disable=SC1091
    . ./bin/activate-hermit
    step "Starting Buzz (first build takes 15-30 min; later starts are fast)"
    exec just desktop-standalone
}

if [[ "$START_ONLY" == true ]]; then
    [[ -d "$INSTALL_DIR/.git" ]] || die "no checkout at $INSTALL_DIR; run setup first"
    launch
fi

[[ -n "$RELAY_HOST" ]] || die "--host is required (the host's Tailscale MagicDNS name)"
[[ "$RELAY_HOST" == *.* ]] || die "--host must be the full MagicDNS name (e.g. host.tailnet.ts.net), matching the host's RELAY_URL"

step "Checking this Mac"
[[ "$(uname -s)" == "Darwin" ]] || die "this script is for macOS"
[[ "$(uname -m)" == "arm64" ]] || die "expected Apple Silicon (arm64), got $(uname -m)"

step "Checking Xcode Command Line Tools (provides git and the C toolchain)"
if ! xcode-select -p >/dev/null 2>&1; then
    xcode-select --install || true
    die "finish the Command Line Tools installer window, then re-run this script"
fi

step "Checking Tailscale"
TS_CLI=""
for candidate in tailscale /Applications/Tailscale.app/Contents/MacOS/Tailscale; do
    if command -v "$candidate" >/dev/null 2>&1; then TS_CLI="$candidate"; break; fi
done
if [[ -z "$TS_CLI" ]]; then
    echo "Tailscale is not installed. Opening the download page."
    open "https://tailscale.com/download/mac"
    die "install Tailscale, sign in, accept the host's invite, then re-run this script"
fi
"$TS_CLI" status >/dev/null 2>&1 || die "Tailscale is not connected; open Tailscale and sign in"

step "Checking the host relay at ${RELAY_HOST}:${RELAY_PORT}"
# Plain GET goes through the relay's host-to-community binding, so a host name
# that doesn't match the host's RELAY_URL fails here instead of after the build.
if ! curl --silent --fail --max-time 10 -o /dev/null "http://${RELAY_HOST}:${RELAY_PORT}/"; then
    die "no community at http://${RELAY_HOST}:${RELAY_PORT} — host offline in Tailscale, relay down, or --host differs from the host's RELAY_URL"
fi

step "Fetching Buzz (${PINNED_REF}) into ${INSTALL_DIR}"
if [[ -d "$INSTALL_DIR/.git" ]]; then
    git -C "$INSTALL_DIR" fetch --depth 1 origin "$PINNED_REF"
    git -C "$INSTALL_DIR" checkout --detach FETCH_HEAD
else
    git clone --depth 1 --branch "$PINNED_REF" "$REPO_URL" "$INSTALL_DIR"
fi

cat <<EOF

==> Setup ready. When the Buzz window opens:
    1. Choose "Join" and paste the invite link you were sent.
       (Community URL if asked: ws://${RELAY_HOST}:${RELAY_PORT})
    2. Create your profile.

    To start Buzz again later:
      ${INSTALL_DIR}/scripts/join-over-tailscale/join-mac.sh --start
EOF

launch
