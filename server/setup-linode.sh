#!/usr/bin/env bash
# EZ server one-shot setup for a fresh Ubuntu/Debian Linode (or any VPS).
# Usage (as root):  curl -fsSL https://raw.githubusercontent.com/TheMishez/EZ_VoiceChat/main/server/setup-linode.sh | bash
set -euo pipefail

DIR=/opt/ez-voice
REPO=https://raw.githubusercontent.com/TheMishez/EZ_VoiceChat/main/server

if [ "$(id -u)" -ne 0 ]; then echo "Run as root (sudo -i)"; exit 1; fi

echo "==> Installing Docker"
if ! command -v docker >/dev/null; then
  curl -fsSL https://get.docker.com | sh
fi
systemctl enable --now docker

echo "==> Firewall: SSH + Mumble (64738 TCP/UDP)"
if command -v ufw >/dev/null || apt-get install -y ufw >/dev/null; then
  ufw allow OpenSSH >/dev/null
  ufw allow 64738/tcp >/dev/null
  ufw allow 64738/udp >/dev/null
  ufw --force enable >/dev/null
fi

echo "==> Low-latency network tweaks"
cat >/etc/sysctl.d/90-ez-voice.conf <<'EOF'
net.core.rmem_max=4194304
net.core.wmem_max=4194304
net.core.default_qdisc=fq
EOF
sysctl --system >/dev/null || true

echo "==> Server files in $DIR"
mkdir -p "$DIR/data"
cd "$DIR"
curl -fsSL "$REPO/docker-compose.yml" -o docker-compose.yml
curl -fsSL "$REPO/mumble-server.ini" -o mumble-server.ini
# The container runs as uid 10000.
chown -R 10000:10000 data || true

if [ ! -f .env ]; then
  ADMIN=$(tr -dc 'A-Za-z0-9' </dev/urandom | head -c 20)
  read -r -p "Server name [EZ Server]: " NAME </dev/tty || true
  read -r -p "Join password (leave empty for none): " PASS </dev/tty || true
  cat >.env <<EOF
EZ_SERVER_NAME=${NAME:-EZ Server}
EZ_SERVER_PASSWORD=${PASS:-}
EZ_ADMIN_PASSWORD=$ADMIN
EOF
  chmod 600 .env
fi

docker compose pull
docker compose up -d

IP=$(curl -fsS4 https://ifconfig.me || hostname -I | awk '{print $1}')
echo
echo "========================================================"
echo " EZ server is running."
echo " In EZ VoiceChat: Add a server -> Mumble -> $IP (port 64738)"
echo " Admin (SuperUser) password is in $DIR/.env"
echo "========================================================"
