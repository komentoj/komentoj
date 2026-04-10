#!/usr/bin/env bash
# One-time local E2E setup for the Mastodon test suite.
# Installs mkcert, generates TLS certs, generates VAPID keys, updates /etc/hosts.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CERTS_DIR="$SCRIPT_DIR/certs"
ENV_FILE="$SCRIPT_DIR/.env"
mkdir -p "$CERTS_DIR"

# ── Install mkcert ────────────────────────────────────────────────────────────
if ! command -v mkcert &>/dev/null; then
    echo "Installing mkcert..."
    if command -v brew &>/dev/null; then
        brew install mkcert nss
    elif command -v apt-get &>/dev/null; then
        sudo apt-get install -y libnss3-tools
        MKCERT_URL="https://dl.filippo.io/mkcert/latest?for=linux/amd64"
        sudo curl -fsSL "$MKCERT_URL" -o /usr/local/bin/mkcert
        sudo chmod +x /usr/local/bin/mkcert
    else
        echo "Please install mkcert manually: https://github.com/FiloSottile/mkcert"
        exit 1
    fi
fi

# ── Generate leaf certs ───────────────────────────────────────────────────────
mkcert \
    -cert-file "$CERTS_DIR/local.crt" \
    -key-file  "$CERTS_DIR/local.key" \
    komentoj.local mastodon.local
echo "Certs written to $CERTS_DIR/"

# ── Copy CA for Docker container trust ───────────────────────────────────────
CAROOT=$(mkcert -CAROOT)
cp "$CAROOT/rootCA.pem" "$CERTS_DIR/rootCA.pem"
echo "CA cert copied to $CERTS_DIR/rootCA.pem"

# ── Generate VAPID keys ───────────────────────────────────────────────────────
echo "Generating Mastodon VAPID keys..."
VAPID_OUTPUT=$(docker run --rm ghcr.io/mastodon/mastodon:latest \
    bundle exec rake mastodon:webpush:generate_vapid_key 2>/dev/null)
VAPID_PRIVATE_KEY=$(echo "$VAPID_OUTPUT" | grep VAPID_PRIVATE_KEY | cut -d= -f2-)
VAPID_PUBLIC_KEY=$(echo "$VAPID_OUTPUT"  | grep VAPID_PUBLIC_KEY  | cut -d= -f2-)

cat > "$ENV_FILE" <<EOF
VAPID_PRIVATE_KEY=${VAPID_PRIVATE_KEY}
VAPID_PUBLIC_KEY=${VAPID_PUBLIC_KEY}
EOF
echo "VAPID keys written to $ENV_FILE"
echo "  Source this file before starting the stack:"
echo "    set -a; source $ENV_FILE; set +a"

# ── /etc/hosts ────────────────────────────────────────────────────────────────
if grep -q "mastodon.local" /etc/hosts; then
    echo "/etc/hosts entries already exist"
else
    echo "127.0.0.1 komentoj.local mastodon.local" | sudo tee -a /etc/hosts
    echo "Added /etc/hosts entries"
fi

echo ""
echo "Setup complete. Start the stack with:"
echo "  set -a; source e2e/mastodon/.env; set +a"
echo "  docker compose -f e2e/mastodon/docker-compose.yml up -d"
