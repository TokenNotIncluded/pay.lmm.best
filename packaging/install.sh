#!/bin/sh
# Offline archive installer. No downloads, sudo, payment setup, or implicit start.
set -eu
prefix="${HOME:?HOME is required}/.local"
system=no
uninstall=no
while [ "$#" -gt 0 ]; do
    case "$1" in
        --prefix) [ "$#" -ge 2 ] || exit 2; prefix=$2; shift 2 ;;
        --system) system=yes; prefix=/usr/local; shift ;;
        --uninstall) uninstall=yes; shift ;;
        -h|--help) echo 'install.sh [--prefix ABSOLUTE_PATH | --system] [--uninstall]'; exit 0 ;;
        *) echo "Unknown argument: $1" >&2; exit 2 ;;
    esac
done
case "$prefix" in /|*/../*|*/..|*/./*|*/.|*'
'*) echo 'Unsafe prefix' >&2; exit 2;; /*) ;; *) echo 'Prefix must be absolute' >&2; exit 2;; esac
if [ "$system" = yes ]; then
    [ "$(id -u)" -eq 0 ] || { echo '--system requires root (invoke sudo explicitly).' >&2; exit 1; }
    [ "$prefix" = /usr/local ] || { echo '--system uses /usr/local; do not combine with --prefix.' >&2; exit 2; }
    [ ! -e /usr/bin/pay-lmm ] || { echo 'A native /usr/bin/pay-lmm installation exists; use its package manager instead.' >&2; exit 1; }
fi
if [ "$uninstall" = yes ]; then
    if [ "$system" = yes ]; then
        [ ! -f "$prefix/lib/pay-lmm/maintainer.sh" ] || sh "$prefix/lib/pay-lmm/maintainer.sh" stop
        # Only remove units carrying our installer ownership marker.
        for f in /etc/systemd/system/pay-lmm.service /etc/init.d/pay-lmm; do
            if [ -f "$f" ] && grep -q '^# Installed by pay-lmm archive installer$' "$f"; then rm -f "$f"; fi
        done
        if [ -d /run/systemd/system ]; then systemctl daemon-reload; fi
    fi
    rm -f "$prefix/bin/pay-lmm"
    rm -rf "$prefix/share/doc/pay-lmm" "$prefix/lib/pay-lmm"
    echo 'Program removed; configuration, service account and payment database retained.'
    exit 0
fi
base=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
cd "$base"
# Outer release checksums must also be verified before extracting the archive.
sha256sum -c SHA256SUMS
case "$(uname -s):$(uname -m)" in
    Linux:x86_64) arch=amd64 ;;
    Linux:aarch64|Linux:arm64) arch=arm64 ;;
    *) echo 'Unsupported OS/architecture; no changes made.' >&2; exit 1 ;;
esac
[ "$(cat ARCH)" = "$arch" ] || { echo 'Wrong archive architecture; no changes made.' >&2; exit 1; }
if [ "$system" = yes ]; then
    for unit in /etc/systemd/system/pay-lmm.service /etc/init.d/pay-lmm; do
        if [ -e "$unit" ] && ! grep -q '^# Installed by pay-lmm archive installer$' "$unit"; then
            echo "Existing administrator unit is not managed by this installer: $unit" >&2; exit 1
        fi
    done
fi
mkdir -p "$prefix/bin" "$prefix/share/doc/pay-lmm" "$prefix/lib/pay-lmm"
tmp=$(mktemp "$prefix/bin/.pay-lmm.XXXXXX")
trap 'rm -f "$tmp"' EXIT HUP INT TERM
cp pay-lmm "$tmp"
chmod 0755 "$tmp"
"$tmp" --version
mv -f "$tmp" "$prefix/bin/pay-lmm"
for f in README.md LICENSE SECURITY.md build-info.json; do cp "$f" "$prefix/share/doc/pay-lmm/"; done
for d in examples docs proto deploy; do cp -R "$d" "$prefix/share/doc/pay-lmm/"; done
cp deploy/maintainer.sh "$prefix/lib/pay-lmm/maintainer.sh"
chmod 0755 "$prefix/lib/pay-lmm/maintainer.sh"
if [ "$system" = yes ]; then
    sh "$prefix/lib/pay-lmm/maintainer.sh" setup
    if [ -x /sbin/openrc-run ]; then
        mkdir -p /etc/init.d
        { printf '#!/sbin/openrc-run\n# Installed by pay-lmm archive installer\n'; sed '1d;s|/usr/bin/pay-lmm|/usr/local/bin/pay-lmm|g' deploy/pay-lmm.openrc; } > /etc/init.d/pay-lmm
        chmod 0755 /etc/init.d/pay-lmm
    elif command -v systemctl >/dev/null 2>&1; then
        mkdir -p /etc/systemd/system
        { printf '# Installed by pay-lmm archive installer\n'; sed 's|/usr/bin/pay-lmm|/usr/local/bin/pay-lmm|g' deploy/pay-lmm.service; } > /etc/systemd/system/pay-lmm.service
        if [ -d /run/systemd/system ]; then systemctl daemon-reload; fi
    fi
fi
echo "Installed $prefix/bin/pay-lmm. No services enabled or restarted."
