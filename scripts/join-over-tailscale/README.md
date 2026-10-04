# Join a Buzz relay over Tailscale (macOS, Apple Silicon)

A guest uses the Buzz desktop app to connect to a relay on the host's machine.
The two machines don't need to share a local network. Tailscale is the access
boundary: only devices in the host's tailnet can reach the relay.

**Status:** parked. Not yet tested end to end on a real Mac or tailnet.

## Host (the machine running the relay)

1. `RELAY_URL` in `.env` must be `ws://<your-magicdns-name>:3000`. The relay
   rejects clients that connect with any other URL. Connect your own apps with
   the same URL.

   > **Warning:** the relay ties each community to the host name in
   > `RELAY_URL`. If you change it (for example from `localhost`), a new, empty
   > community is created on restart. Existing channels and members stay under
   > the old URL. Decide before you switch.

2. From the repo root, run `scripts/join-over-tailscale/check-host.sh`. It
   checks Tailscale, `RELAY_URL`, the bind address, and that a community is
   served at your Tailscale name.
3. Add the guest to your tailnet, or share this machine with them from the
   Tailscale admin console.
4. Optional: if you set `BUZZ_REQUIRE_RELAY_MEMBERSHIP=true`, also send an
   invite link (Settings > Invites). This needs an owner or admin
   role. By default, any device that can reach the relay can join.

## Guest (Apple Silicon Mac)

1. Install [Tailscale](https://tailscale.com/download/mac), sign in, and
   accept the host's invite.
2. Install Buzz: download `Buzz_<version>_aarch64.dmg` from the
   [latest release](https://github.com/block/buzz/releases/latest) and drag
   Buzz to Applications.
3. Run the preflight. It checks Tailscale and the relay, opens Buzz, and
   prints the URL to paste:

   ```bash
   curl -fsSL https://raw.githubusercontent.com/kristofferrullar/buzz/claude/mac-onboarding-script/scripts/join-over-tailscale/join-mac.sh -o join-mac.sh
   bash join-mac.sh --host <host-magicdns-name>
   ```

4. In Buzz, add a community and paste `ws://<host-magicdns-name>:3000`. Keep the
   `ws://` prefix: without it the app assumes `wss://`, and the connection fails.

## Notes

- **Version skew:** the release app and a host relay built from `main` can
  differ in version. If something misbehaves, the guest can instead build the
  app from this branch: `. ./bin/activate-hermit && just desktop-standalone`
  in a clone. This needs the Xcode Command Line Tools, takes 15–30 minutes the
  first time, and uses several GB of disk.
- **Agent handoff:** [AGENT_HANDOFF.md](AGENT_HANDOFF.md) has one prompt for
  each side, for running the setup through an AI coding agent.
