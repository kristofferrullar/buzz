# Agent handoff prompts

Paste the prompt for your machine into an AI coding agent. Run the host prompt
first. The steps live in [README.md](README.md); the prompts only add guardrails.

## Host

```text
Help me let a guest reach my Buzz relay over Tailscale.
In my Buzz checkout, get the helper files:
  git fetch origin claude/mac-onboarding-script
  git checkout origin/claude/mac-onboarding-script -- scripts/join-over-tailscale
Then follow the "Host" section of scripts/join-over-tailscale/README.md and report
each result.
Guardrails:
- Before changing RELAY_URL, stop and ask me. Changing it starts a new, empty community.
- Never edit the database, and never disable the macOS firewall.
- Don't commit or push anything.
Finish by telling me the Tailscale MagicDNS name to send the guest.
```

## Guest

Replace `<HOST_MAGICDNS_NAME>` with the name the host sends you.

```text
Help me join a Buzz community over Tailscale on this Apple Silicon Mac.
Host name: <HOST_MAGICDNS_NAME>
Read https://raw.githubusercontent.com/kristofferrullar/buzz/claude/mac-onboarding-script/scripts/join-over-tailscale/README.md
and follow its "Guest" section. Show me join-mac.sh before you run it.
Guardrails:
- Wait for me at each step that needs a click: installers, sign-ins, invites.
- If a check fails, explain it in plain words and fix only what it names.
```
