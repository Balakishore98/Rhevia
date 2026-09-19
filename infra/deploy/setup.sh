#!/usr/bin/env bash
# Generates .env and turnserver.conf for a RheviaLink relay host.
#
#   ./setup.sh link.rhevia.app
#
# Run once on the VPS, then `docker compose up -d`.
set -euo pipefail

DOMAIN="${1:-}"
if [[ -z "$DOMAIN" ]]; then
  echo "usage: $0 <domain>    e.g. $0 link.rhevia.app" >&2
  exit 1
fi

cd "$(dirname "$0")"

if [[ -f .env ]]; then
  echo "refusing to overwrite an existing .env" >&2
  echo "the TURN secret in it is shared with running clients; delete it deliberately if you mean to rotate" >&2
  exit 1
fi

# The signaling server and coturn must agree on this, and it must never reach
# a client: it mints relay credentials.
SECRET="$(openssl rand -base64 32 | tr -d '\n')"

cat > .env <<ENV
RHEVIA_DOMAIN=${DOMAIN}
RHEVIA_TURN_SECRET=${SECRET}
ENV
chmod 600 .env

# coturn has no variable substitution, so the config is generated rather than
# templated at runtime.
sed \
  -e "s|^realm=.*|realm=${DOMAIN}|" \
  -e "s|^server-name=.*|server-name=${DOMAIN}|" \
  -e "s|^static-auth-secret=.*|static-auth-secret=${SECRET}|" \
  -e "s|^# cert=.*|cert=/caddy/caddy/certificates/acme-v02.api.letsencrypt.org-directory/${DOMAIN}/${DOMAIN}.crt|" \
  -e "s|^# pkey=.*|pkey=/caddy/caddy/certificates/acme-v02.api.letsencrypt.org-directory/${DOMAIN}/${DOMAIN}.key|" \
  ../coturn/turnserver.conf > turnserver.conf
chmod 600 turnserver.conf

PUBLIC_IP="$(curl -fsS --max-time 5 https://api.ipify.org || true)"
if [[ -n "$PUBLIC_IP" ]]; then
  # On a NATted cloud VM coturn must advertise the public address or every
  # relayed candidate is unreachable.
  PRIVATE_IP="$(hostname -I 2>/dev/null | awk '{print $1}' || true)"
  if [[ -n "$PRIVATE_IP" && "$PRIVATE_IP" != "$PUBLIC_IP" ]]; then
    echo "external-ip=${PUBLIC_IP}/${PRIVATE_IP}" >> turnserver.conf
    echo "  detected NAT: advertising ${PUBLIC_IP} for ${PRIVATE_IP}"
  fi
fi

cat <<DONE

  Wrote .env and turnserver.conf for ${DOMAIN}.

  Before starting:
    1. Point an A record for ${DOMAIN} at this host.
    2. Open  80/tcp 443/tcp  (TLS + signaling)
             3478/tcp 3478/udp 5349/tcp 5349/udp  (STUN/TURN)
             49152-65535/udp  (relay range)

  Then:
    docker compose up -d
    curl https://${DOMAIN}/healthz

  Verify relaying actually works before trusting it in a show:
    https://icetest.info/  — enter turn:${DOMAIN}:3478 with credentials from
    the signaling server, and confirm a "relay" candidate appears.

DONE
