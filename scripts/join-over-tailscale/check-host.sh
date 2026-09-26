#!/usr/bin/env bash
# Host-side check: is this machine's relay reachable by a guest over Tailscale?
#
# NIP-42 auth rejects clients whose relay URL differs from RELAY_URL, so the
# relay must advertise the Tailscale name the guest connects to.
#
# Usage: scripts/join-over-tailscale/check-host.sh [path/to/.env]
set -euo pipefail

ENV_FILE="${1:-.env}"
fail=0
ok() { echo "  ok   $*"; }
bad() { echo "  FAIL $*"; fail=1; }

command -v tailscale >/dev/null 2>&1 || { echo "tailscale CLI not found on PATH" >&2; exit 1; }
[[ -f "$ENV_FILE" ]] || { echo "no $ENV_FILE; run from the repo root" >&2; exit 1; }

ts_name="$({ tailscale status --json 2>/dev/null || true; } | sed -n 's/.*"DNSName": *"\([^"]*\)\.".*/\1/p' | head -n1)"
[[ -n "$ts_name" ]] || { echo "  FAIL Tailscale is not connected; open Tailscale and sign in" >&2; exit 1; }
ok "Tailscale up as ${ts_name}"

env_value() { sed -n "s/^$1=//p" "$ENV_FILE" | tail -n1 | sed -e 's/^["'\'']//' -e 's/["'\'']$//'; }
relay_url="$(env_value RELAY_URL)"
# The relay defaults to 0.0.0.0:3000 when BUZZ_BIND_ADDR is unset.
bind_addr="$(env_value BUZZ_BIND_ADDR)"; bind_addr="${bind_addr:-0.0.0.0:3000}"
port="${bind_addr##*:}"; port="${port:-3000}"
expected="ws://${ts_name}:${port}"

if [[ "$relay_url" == "$expected" ]]; then
    ok "RELAY_URL=${relay_url}"
else
    bad "RELAY_URL=${relay_url:-<unset>}; guests need RELAY_URL=${expected}. WARNING: the relay keys each community to its host, so switching starts an EMPTY community; existing channels stay under the old URL (see README)"
fi

if [[ "${bind_addr%%:*}" == "0.0.0.0" ]]; then
    ok "relay binds all interfaces"
else
    bad "BUZZ_BIND_ADDR=${bind_addr}; use 0.0.0.0:${port} so Tailscale can reach it"
fi

# Plain GET (no NIP-11 Accept header) passes the relay's host-to-community
# binding, so an unmapped host (RELAY_URL changed but relay not restarted) fails.
if curl --silent --fail --max-time 5 -o /dev/null "http://${ts_name}:${port}/"; then
    ok "relay serves a community at http://${ts_name}:${port}"
else
    bad "no community at http://${ts_name}:${port} (relay down, firewall, or RELAY_URL changed without restarting the relay)"
fi

echo
if [[ "$fail" -eq 0 ]]; then
    echo "Ready. Send your guest:"
    echo "  1. A Tailscale invite (admin console > Users > Invite, or share this machine)."
    echo "  2. An invite link from the Buzz app: Settings > Members > Invite."
    echo "  3. This command:"
    echo "     curl -fsSL https://raw.githubusercontent.com/kristofferrullar/buzz/claude/mac-onboarding-script/scripts/join-over-tailscale/join-mac.sh | bash -s -- --host ${ts_name}"
fi
exit "$fail"
