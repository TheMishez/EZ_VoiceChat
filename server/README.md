# EZ server

Your own voice server for EZ VoiceChat. It's a standard Mumble server with settings for low-latency team voice, so normal Mumble clients can join too.

## What you need
- A small VPS. A Linode Nanode (1 GB, about $5/month) is enough for a few dozen people.
- Pick the region closest to you and your team. That matters more for delay than anything else.

## Set it up on Linode
1. Create a Linode with **Ubuntu 24.04** in the region nearest your team.
2. SSH in as root and run:
   ```bash
   curl -fsSL https://raw.githubusercontent.com/TheMishez/EZ_VoiceChat/main/server/setup-linode.sh | bash
   ```
3. Answer the two questions (server name and optional join password).
4. In EZ VoiceChat: **Add a server → Mumble → your Linode's IP**, port 64738.

The script installs Docker, opens port 64738 (TCP and UDP) in the firewall, applies small network tweaks and starts the server. It restarts on its own after a reboot.

## Admin
- The SuperUser password is in `/opt/ez-voice/.env`.
- To change the name or password, edit `.env`, then run `docker compose up -d` in `/opt/ez-voice`.
- Update the server: `cd /opt/ez-voice && docker compose pull && docker compose up -d`.
- Back up `/opt/ez-voice/data`: it holds channels, registered users and permissions.

## Run it anywhere else
Any machine with Docker works:
```bash
cd server
echo "EZ_ADMIN_PASSWORD=change-me" > .env
docker compose up -d
```
If it runs at home, forward port 64738 TCP and UDP on your router.

## Why it's fast
- Opus only, so no client falls back to an older codec.
- Clients send voice over UDP, with a TCP fallback when UDP is blocked. EZ VoiceChat shows which one is in use next to the ping.
- EZ VoiceChat sends 10 ms frames by default, so the server adds no buffering on top of network delay.
