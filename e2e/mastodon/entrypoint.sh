#!/bin/bash
set -euo pipefail

# Trust the mkcert CA so komentoj can make HTTPS requests to Caddy-fronted services
if [[ -f /e2e/certs/rootCA.pem ]]; then
    cp /e2e/certs/rootCA.pem /usr/local/share/ca-certificates/mkcert-local-ca.crt
    update-ca-certificates --fresh 2>/dev/null
fi

exec /usr/local/bin/komentoj
