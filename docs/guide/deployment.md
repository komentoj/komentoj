# Deployment

## systemd service

Create `/etc/systemd/system/komentoj.service`:

```ini
[Unit]
Description=komentoj ActivityPub comment server
After=network.target postgresql.service redis.service

[Service]
Type=simple
User=komentoj
Group=komentoj
WorkingDirectory=/opt/komentoj
ExecStart=/opt/komentoj/komentoj
Environment=KOMENTOJ_CONFIG=/etc/komentoj/config.toml
Restart=on-failure
RestartSec=5s

# Hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ReadWritePaths=/var/log/komentoj

[Install]
WantedBy=multi-user.target
```

```sh
# Create system user
useradd -r -s /usr/sbin/nologin komentoj

# Place binary and config
install -m 755 target/release/komentoj /opt/komentoj/komentoj
install -m 640 -o root -g komentoj config.toml /etc/komentoj/config.toml

# Enable and start
systemctl daemon-reload
systemctl enable --now komentoj
journalctl -u komentoj -f
```

## nginx reverse proxy

komentoj must be accessible over HTTPS. The simplest setup is nginx in front with a Let's Encrypt certificate.

```nginx
server {
    listen 443 ssl http2;
    server_name comments.example.com;

    ssl_certificate     /etc/letsencrypt/live/comments.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/comments.example.com/privkey.pem;

    # ActivityPub requires these content types to be passed through intact
    location / {
        proxy_pass         http://127.0.0.1:8080;
        proxy_set_header   Host              $host;
        proxy_set_header   X-Real-IP         $remote_addr;
        proxy_set_header   X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header   X-Forwarded-Proto $scheme;

        # Allow large Note payloads
        client_max_body_size 4m;

        # Keep connections alive for fan-out delivery
        proxy_http_version 1.1;
        proxy_set_header   Connection "";
    }
}

server {
    listen 80;
    server_name comments.example.com;
    return 301 https://$host$request_uri;
}
```

Obtain a certificate with Certbot:

```sh
certbot --nginx -d comments.example.com
```

## Docker

```dockerfile
FROM rust:1.75-slim as builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/komentoj /usr/local/bin/komentoj
EXPOSE 8080
CMD ["komentoj"]
```

```yaml
# docker-compose.yml
services:
  komentoj:
    build: .
    environment:
      KOMENTOJ_CONFIG: /config/config.toml
    volumes:
      - ./config.toml:/config/config.toml:ro
    ports:
      - "8080:8080"
    depends_on:
      - postgres
      - redis

  postgres:
    image: postgres:16-alpine
    environment:
      POSTGRES_DB: komentoj
      POSTGRES_USER: komentoj
      POSTGRES_PASSWORD: change-me
    volumes:
      - pgdata:/var/lib/postgresql/data

  redis:
    image: redis:7-alpine
    volumes:
      - redisdata:/data

volumes:
  pgdata:
  redisdata:
```

## Logging

komentoj uses structured tracing logs. Control verbosity with the `RUST_LOG` environment variable:

```sh
# Default (info and above for komentoj, warn for dependencies)
RUST_LOG=komentoj=info

# Verbose (all debug logs)
RUST_LOG=komentoj=debug

# Quiet (errors only)
RUST_LOG=komentoj=warn
```

## Health check

komentoj does not expose a dedicated health endpoint. Use the actor endpoint as a readiness probe:

```sh
curl -sf https://comments.example.com/actor \
  -H "Accept: application/activity+json" | grep '"type":"Service"'
```

Or check that the WebFinger resolves:

```sh
curl -sf "https://comments.example.com/.well-known/webfinger?resource=acct:comments@comments.example.com"
```

## Backup

The only stateful component is PostgreSQL. Back up the `komentoj` database with standard `pg_dump`:

```sh
pg_dump komentoj | gzip > komentoj-$(date +%Y%m%d).sql.gz
```

The RSA keypair is stored in the `instance_keys` table. Losing it means remote servers will reject your signatures until they pick up the new key — back it up along with the rest of the database.
