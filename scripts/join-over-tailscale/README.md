# Join a Buzz relay over Tailscale (macOS, Apple Silicon)

A guest connects to a relay running on the host's machine, and neither needs
to be on the same local network. The guest builds the desktop app from this
pinned branch, so later pushes to `main` don't change what the guest runs.

## Host (the machine running the relay)

1. Set `RELAY_URL=ws://<your-magicdns-name>:3000` in `.env` and restart the relay.
   Connect your own desktop and mobile apps with that same URL, because NIP-42
   auth rejects any other relay URL.
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
