#!/usr/bin/env bash
# Guest preflight for an Apple Silicon Mac: check Tailscale and the host relay,
# then open Buzz and print the community URL to paste. See README.md.
#
# Usage: join-mac.sh --host <host-magicdns-name> [--port 3000]
set -euo pipefail

RELAY_HOST=""
RELAY_PORT="3000"
RELEASES_URL="https://github.com/block/buzz/releases/latest"

die() { echo "Error: $*" >&2; exit 1; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        --host) RELAY_HOST="${2:?--host needs a value}"; shift 2 ;;
        --port) RELAY_PORT="${2:?--port needs a value}"; shift 2 ;;
        -h | --help) echo "Usage: join-mac.sh --host <host-magicdns-name> [--port 3000]"; exit 0 ;;
        *) die "unknown argument: $1" ;;
    esac
done

[[ -n "$RELAY_HOST" ]] || die "--host is required (the host's Tailscale MagicDNS name)"
[[ "$RELAY_HOST" == *.* ]] || die "--host must be the full MagicDNS name (e.g. host.tailnet.ts.net)"
[[ "$(uname -s)-$(uname -m)" == "Darwin-arm64" ]] || die "this script is for Apple Silicon Macs"

if command -v tailscale >/dev/null 2>&1; then
    TS_CLI="tailscale"
elif [[ -x /Applications/Tailscale.app/Contents/MacOS/Tailscale ]]; then
    TS_CLI="/Applications/Tailscale.app/Contents/MacOS/Tailscale"
else
    open "https://tailscale.com/download/mac"
    die "install Tailscale, sign in, accept the host's invite, then re-run"
fi
"$TS_CLI" status >/dev/null 2>&1 || die "Tailscale is not connected; open Tailscale and sign in"
echo "ok   Tailscale connected"

# A plain GET is bound to a community by host name, so a name that doesn't
# match the host's RELAY_URL fails here (404) instead of inside the app.
url="http://${RELAY_HOST}:${RELAY_PORT}/"
curl --silent --fail --max-time 10 -o /dev/null "$url" \
    || die "no community at $url (host offline in Tailscale, relay down, or name differs from the host's RELAY_URL)"
echo "ok   relay reachable at ${RELAY_HOST}:${RELAY_PORT}"

if ! open -a Buzz 2>/dev/null; then
    open "$RELEASES_URL"
    die "install Buzz first: download Buzz_<version>_aarch64.dmg, drag Buzz to Applications, then re-run"
fi

cat <<EOF

In Buzz, add a community and paste exactly:
    ws://${RELAY_HOST}:${RELAY_PORT}
(Keep the ws:// prefix; without it the app assumes wss:// and fails.)
EOF
