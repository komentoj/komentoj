#!/usr/bin/env bash
# One-time local E2E setup for the GoToSocial test suite.
# Installs mkcert, generates TLS certs, updates /etc/hosts.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
CERTS_DIR="$SCRIPT_DIR/certs"
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
    komentoj.local gotosocial.local
echo "Certs written to $CERTS_DIR/"

# ── Copy CA for Docker container trust ───────────────────────────────────────
CAROOT=$(mkcert -CAROOT)
cp "$CAROOT/rootCA.pem" "$CERTS_DIR/rootCA.pem"
echo "CA cert copied to $CERTS_DIR/rootCA.pem"

# ── /etc/hosts ────────────────────────────────────────────────────────────────
if grep -q "komentoj.local" /etc/hosts; then
    echo "/etc/hosts entries already exist"
else
    echo "127.0.0.1 komentoj.local gotosocial.local" | sudo tee -a /etc/hosts
    echo "Added /etc/hosts entries"
fi

echo ""
echo "Setup complete. Start the stack with:"
echo "  docker compose -f e2e/gotosocial/docker-compose.yml up -d"
