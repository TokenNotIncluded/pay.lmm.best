# Deployment

Use [the standardized packaging and deployment guide](../docs/packaging.md) for
musl tarballs, deb/rpm packages, systemd/OpenRC, containers and version-tag releases.

The default database path for a system installation is
`/var/lib/pay-lmm/pay.sqlite3`; configuration is `/etc/pay.lmm.best/config.toml`
and protected credentials are `/etc/pay.lmm.best/secrets.env`. Packages create
an unprivileged account but never configure real providers or enable/restart
payment services automatically. Upgrades and removals retain configuration,
credentials, service accounts and payment evidence.

Native packages use `/usr/bin/pay-lmm`. The archive system installer uses
`/usr/local/bin/pay-lmm` and renders the corresponding service unit. Existing
administrator units are not silently overwritten. Check for old hand-installed
binaries and systemd overrides before switching installation methods.

`make image` prepares the same verified static binary used by other formats,
then builds the scratch image. Plain `docker build` requires the matching
`dist/container/<architecture>/pay-lmm` file to have already been produced by
`make package`. `compose.yaml` defaults to the local image rather than assuming
that a remote release exists. Configure container listening on `0.0.0.0:8080`
and a writable data volume; the runtime's default UID/GID is `10001:10001`.

The Caddyfile is a minimal TLS proxy example, not complete rate limiting or
DDoS protection. Do not log callback query strings, provider bodies or secrets.
Provider account setup, sandbox acceptance, privacy terms, domain DNS, TLS,
egress controls and consistent SQLite backups remain operator tasks. This
repository does not deploy a live payment endpoint simply by being built.
