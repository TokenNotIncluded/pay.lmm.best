# Deployment

The repository does not deploy a live endpoint or modify DNS by itself. Point `pay.lmm.best` at your host, configure TLS, and use your own upstream merchant accounts.

## systemd

Build the binary, create a dedicated system user `pay-lmm`, and install the binary as `/usr/local/bin/pay-lmm`. Put configuration in `/etc/pay.lmm.best/config.toml` and credentials in an operator-owned `/etc/pay.lmm.best/secrets.env` with mode 0600. Set `server.database` to `/var/lib/pay-lmm/pay.sqlite3`; keep the database directory private. Install `pay-lmm.service`, reload systemd and start it only after validating your configuration and upstream sandbox callbacks.

The example Caddyfile terminates public TLS and forwards to the default loopback listener. It is a minimal reverse-proxy example, not a complete DDoS/connection-limiting deployment. Add edge rate and connection limits, outbound firewall restrictions and monitoring appropriate to your environment. Never log full callback query strings or bodies.

## Container

`docker build -t pay-lmm .` produces an unprivileged runtime image. Mount a configuration file read-only and a writable directory or named volume at `/var/lib/pay`. Inject secrets using your deployment platform, not image build arguments. Set `listen = "0.0.0.0:8080"` **inside the container** and `database = "/var/lib/pay/pay.sqlite3"`; expose the port only to your TLS reverse proxy. The host process and systemd example should retain a loopback listener.

No container image is published to a registry by this repository's initial CI. The CI binary artifact is not a deployment, a release, or a provider certification.

## Upstream setup and acceptance

For ePay, fill in your provider's HTTPS endpoint and merchant PID/key. The adapter supplies `/hooks/<gateway-id>` as `notify_url`. Ensure the configured ePay variant really follows the legacy CNY `submit.php` protocol.

For Waffo, obtain your own merchant ID, the corresponding environment's private API key, the platform's matching webhook public key, a store, and a published/available **one-time** product appropriate to that environment. Configure `order.completed` delivery to `/hooks/<gateway-id>` in the provider's dashboard. Do not assume creating this repository or running the binary configures the provider. Explicitly choose and verify the amount basis, supported currencies, tax behavior and exact checkout URL origin.

Use separate merchant secrets and notification endpoints for tests and production. Verify a successful order, rejected signature, wrong amount/currency, duplicate event, lost notification response, process restart and upstream timeout before accepting live orders. Test/provider credentials are not supplied by this project. Privacy-policy, terms and merchant-onboarding requirements remain the operator's responsibility with the upstream provider.

## Operations

Monitor `/healthz` and `/readyz`, process RSS, CPU, disk/WAL growth, rejected callback counts, `unknown` orders and `dead` notifications. Current error logs intentionally contain identifiers and short error codes only, not provider bodies or private material. Database inspection should be read-only; never patch payment state casually.

Use the SQLite backup API or stop the process for a consistent backup. Test restoration. Never run two processes against one database file, mount it on a network filesystem, or delete receipts to save disk without a deliberate idempotency retention policy. Keep old gateway identities alive until their in-flight orders are resolved.
