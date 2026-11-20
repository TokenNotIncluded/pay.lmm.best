#!/bin/sh
# Never enable/restart automatically, remove accounts, or delete payment evidence.
set -eu
case "${1:-}" in
  setup)
    for path in /etc/pay.lmm.best /var/lib/pay-lmm; do
      [ ! -L "$path" ] || { echo "Refusing symlinked service directory: $path" >&2; exit 1; }
    done
    if ! grep -q '^pay-lmm:' /etc/group; then
      if command -v groupadd >/dev/null 2>&1; then groupadd --system pay-lmm
      else addgroup -S pay-lmm; fi
    fi
    if ! id pay-lmm >/dev/null 2>&1; then
      if command -v useradd >/dev/null 2>&1; then
        useradd --system --gid pay-lmm --home-dir /var/lib/pay-lmm --no-create-home --shell /bin/false pay-lmm
      else
        adduser -S -D -H -h /var/lib/pay-lmm -s /bin/false -G pay-lmm pay-lmm
      fi
    fi
    [ "$(id -u pay-lmm)" -ne 0 ] || { echo 'Service account must not be root.' >&2; exit 1; }
    mkdir -p /etc/pay.lmm.best /var/lib/pay-lmm
    chown root:pay-lmm /etc/pay.lmm.best
    chmod 0750 /etc/pay.lmm.best
    chown pay-lmm:pay-lmm /var/lib/pay-lmm
    chmod 0700 /var/lib/pay-lmm
    if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then systemctl daemon-reload; fi
    echo 'pay-lmm installed; configure credentials before enabling the service. Existing services are NOT restarted.'
    ;;
  stop)
    if [ -d /run/systemd/system ] && command -v systemctl >/dev/null 2>&1; then
      if systemctl is-active --quiet pay-lmm.service; then systemctl stop pay-lmm.service; fi
      systemctl disable pay-lmm.service >/dev/null 2>&1 || true
    elif command -v rc-service >/dev/null 2>&1; then
      if rc-service pay-lmm status >/dev/null 2>&1; then rc-service pay-lmm stop; fi
      if command -v rc-update >/dev/null 2>&1; then rc-update del pay-lmm default >/dev/null 2>&1 || true; fi
    fi
    ;;
  *) echo 'Usage: maintainer.sh {setup|stop}' >&2; exit 2 ;;
esac
