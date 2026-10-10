#!/usr/bin/env bash
# Host check: can a guest reach this machine's relay over Tailscale? See README.md.
#
# Usage (from the repo root): scripts/join-over-tailscale/check-host.sh [path/to/.env]
set -euo pipefail

ENV_FILE="${1:-.env}"
fail=0
ok() { echo "ok   $*"; }
bad() { echo "FAIL $*"; fail=1; }

command -v tailscale >/dev/null 2>&1 || { echo "FAIL tailscale CLI not found on PATH" >&2; exit 1; }
[[ -f "$ENV_FILE" ]] || { echo "FAIL no $ENV_FILE; run from the repo root" >&2; exit 1; }

ts_name="$({ tailscale status --json 2>/dev/null || true; } | sed -n 's/.*"DNSName": *"\([^"]*\)\.".*/\1/p' | head -n1)"
[[ -n "$ts_name" ]] || { echo "FAIL Tailscale is not connected; open Tailscale and sign in" >&2; exit 1; }
ok "Tailscale up as ${ts_name}"

# Last assignment wins, surrounding quotes stripped; defaults match the relay's.
env_value() { sed -n "s/^$1=//p" "$ENV_FILE" | tail -n1 | sed -e 's/^["'\'']//' -e 's/["'\'']$//'; }
relay_url="$(env_value RELAY_URL)"; relay_url="${relay_url:-ws://localhost:3000}"
bind_addr="$(env_value BUZZ_BIND_ADDR)"; bind_addr="${bind_addr:-0.0.0.0:3000}"
port="${bind_addr##*:}"
expected="ws://${ts_name}:${port}"

if [[ "$relay_url" == "$expected" ]]; then
    ok "RELAY_URL=${relay_url}"
else
    bad "RELAY_URL=${relay_url}; guests need ${expected}. Read the README warning before changing it"
fi

if [[ "${bind_addr%%:*}" == "0.0.0.0" ]]; then
    ok "relay listens on all interfaces"
else
    bad "BUZZ_BIND_ADDR=${bind_addr}; use 0.0.0.0:${port} so Tailscale can reach it"
fi

# A plain GET is bound to a community by host name, so an unmapped name
# (e.g. RELAY_URL edited but relay not restarted) fails with 404.
if curl --silent --fail --max-time 5 -o /dev/null "http://${ts_name}:${port}/"; then
    ok "relay serves a community at ${ts_name}:${port}"
else
    bad "no community at http://${ts_name}:${port} (relay down, firewall, or relay not restarted)"
fi

echo
[[ "$fail" -eq 0 ]] && echo "Ready. Guest runs: join-mac.sh --host ${ts_name}"
exit "$fail"
