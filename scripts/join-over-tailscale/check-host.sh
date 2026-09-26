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

ts_name="$(tailscale status --json | sed -n 's/.*"DNSName": *"\([^"]*\)\.".*/\1/p' | head -n1)"
if [[ -n "$ts_name" ]]; then ok "Tailscale up as ${ts_name}"; else bad "Tailscale is not connected"; fi

relay_url="$(sed -n 's/^RELAY_URL=//p' "$ENV_FILE" | tail -n1)"
bind_addr="$(sed -n 's/^BUZZ_BIND_ADDR=//p' "$ENV_FILE" | tail -n1)"
port="${bind_addr##*:}"; port="${port:-3000}"
expected="ws://${ts_name}:${port}"

if [[ "$relay_url" == "$expected" ]]; then
    ok "RELAY_URL=${relay_url}"
else
    bad "RELAY_URL=${relay_url:-<unset>}; set RELAY_URL=${expected} in ${ENV_FILE}, restart the relay, and connect your own apps with that URL too"
fi

if [[ "${bind_addr%%:*}" == "0.0.0.0" ]]; then
    ok "relay binds all interfaces"
else
    bad "BUZZ_BIND_ADDR=${bind_addr}; use 0.0.0.0:${port} so Tailscale can reach it"
fi

if curl --silent --fail --max-time 5 -H 'Accept: application/nostr+json' "http://${ts_name}:${port}/" >/dev/null; then
    ok "relay answers at http://${ts_name}:${port}"
else
    bad "relay not reachable at http://${ts_name}:${port} (running? macOS firewall?)"
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
