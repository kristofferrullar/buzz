# Agent handoff prompts

Paste one prompt into an AI coding agent (for example Claude Code) running on
the machine named in its heading. Run the host prompt first. Each prompt is
self-contained.

## 1. Host: the machine that runs the relay

```text
You are preparing my Buzz relay so a guest on another network can join it over Tailscale.
Repo: my local Buzz checkout (the current directory). Helper scripts are on branch
claude/mac-onboarding-script of https://github.com/kristofferrullar/buzz under
scripts/join-over-tailscale/ (if they're missing locally, run
`git fetch origin claude/mac-onboarding-script` and then
`git checkout origin/claude/mac-onboarding-script -- scripts/join-over-tailscale`).

Do these steps in order and report each result:
1. Activate the toolchain: `. ./bin/activate-hermit`.
2. Run `scripts/join-over-tailscale/check-host.sh` and show me its output.
3. If it reports a RELAY_URL mismatch, STOP and ask me before changing anything.
   The relay keys each community to the host in RELAY_URL, so switching RELAY_URL to
   the Tailscale name starts a NEW, EMPTY community. My existing channels stay under
   the old URL. Ask which URL my phone uses over Tailscale. If my phone already
   works over Tailscale, find out why before changing anything. Never edit the
   `communities` table or rewrite hosts in the database.
4. Only after I confirm: set RELAY_URL in .env, restart the relay the way I normally
   run it (`just relay` or `just dev`), and re-run check-host.sh until everything passes.
5. If the reachability check fails and the macOS firewall is on, tell me how to
   allow the relay binary. Do not disable the firewall.
6. Tell me to: invite the guest to my tailnet (or share this machine with them), and
   create a single-use invite link in Buzz under Settings > Members > Invites.
7. Print the exact guest command that check-host.sh gives, and the host MagicDNS name.
Do not commit, push, or change any other files.
```

## 2. Guest: her Mac (Apple Silicon)

Replace `<HOST_MAGICDNS_NAME>` with the name the host prompt printed. Send the
invite link separately; it's a secret.

```text
You are setting up the Buzz desktop app on my Apple Silicon Mac so I can join a
friend's Buzz relay over Tailscale. The host's Tailscale MagicDNS name is
<HOST_MAGICDNS_NAME> (relay port 3000).

Do these steps in order and report each result:
1. Confirm this Mac is Apple Silicon: `uname -m` must print arm64.
2. Ensure the Xcode Command Line Tools are installed (`xcode-select -p`). If they're
   missing, run `xcode-select --install` and wait for me to finish the installer.
3. Ensure Tailscale is installed and signed in, and that I've accepted the host's
   invite. If it isn't, open https://tailscale.com/download/mac and wait for me.
   Then check `tailscale status` (or
   /Applications/Tailscale.app/Contents/MacOS/Tailscale status).
4. Download and run the setup script:
   curl -fsSL https://raw.githubusercontent.com/kristofferrullar/buzz/claude/mac-onboarding-script/scripts/join-over-tailscale/join-mac.sh -o /tmp/join-mac.sh
   Show me the script, then run: bash /tmp/join-mac.sh --host <HOST_MAGICDNS_NAME>
   The first build takes 15-30 minutes. Let it finish.
5. If the script stops with an error, explain it in plain words and fix only what it
   names (for example Tailscale not connected or the wrong host name). Do not edit the
   Buzz source code.
6. When the Buzz window opens, tell me to choose Join, paste the invite link I
   received, and create my profile.
7. Tell me how to start Buzz again later:
   ~/buzz/scripts/join-over-tailscale/join-mac.sh --start
```
