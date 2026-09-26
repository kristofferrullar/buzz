# Join a Buzz relay over Tailscale (macOS, Apple Silicon)

A guest connects to a relay running on the host's machine, and neither needs
to be on the same local network. The guest builds the desktop app from this
pinned branch, so later pushes to `main` don't change what the guest runs.

To have an AI coding agent do the setup, use the prompts in
[AGENT_HANDOFF.md](AGENT_HANDOFF.md).

## Host (the machine running the relay)

1. Make sure `RELAY_URL` in `.env` is `ws://<your-magicdns-name>:3000`, and
   connect your own apps with that same URL. NIP-42 auth rejects any other URL.
   **Warning:** the relay keys each community to the host in `RELAY_URL`. If
   you switch it (for example from `localhost`), a new, empty community is
   created on restart, and your existing channels and members stay under the
   old URL. Decide on this before switching.
2. Run `scripts/join-over-tailscale/check-host.sh`. It verifies Tailscale, the
   relay URL, the bind address and reachability, then prints what to send.
3. Invite the guest to your tailnet, or share this machine with them.
4. In Buzz, go to Settings > Members > Invites, create a single-use invite
   link and send it to the guest.

## Guest (her Mac)

Install [Tailscale](https://tailscale.com/download/mac), sign in, accept the
invite, then run:

```bash
curl -fsSL https://raw.githubusercontent.com/kristofferrullar/buzz/claude/mac-onboarding-script/scripts/join-over-tailscale/join-mac.sh \
  | bash -s -- --host <host-magicdns-name>
```

The script checks the Mac, Xcode Command Line Tools, Tailscale and whether the
relay is reachable. It then clones the pinned branch to `~/buzz` and runs
`just desktop-standalone`, which needs no Docker and no local relay. When the
app opens, paste the invite link.

To start Buzz again later, run `~/buzz/scripts/join-over-tailscale/join-mac.sh --start`.
