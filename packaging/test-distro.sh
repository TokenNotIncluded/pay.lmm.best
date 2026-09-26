#!/bin/sh
# Runs only inside an ephemeral CI distribution container; never on the host.
set -eu
[ -f /.dockerenv ] || { echo 'This destructive package test requires a Docker container.' >&2; exit 1; }
[ -d /src/dist ] && [ -d /src/qa ] || exit 1
. /etc/os-release
case "$ID" in
  debian|ubuntu)
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y --no-install-recommends python3 passwd util-linux
    format=deb
    ;;
  fedora|rocky|almalinux)
    dnf -y install python3 shadow-utils util-linux
    format=rpm
    ;;
  opensuse*)
    zypper --non-interactive --gpg-auto-import-keys install python3 shadow util-linux
    format=rpm
    ;;
  alpine)
    apk add --no-cache python3 openrc shadow
    format=tar
    ;;
  arch)
    pacman -Syu --noconfirm python shadow util-linux
    format=tar
    ;;
  *) echo "Unsupported QA image: $ID" >&2; exit 2 ;;
esac
(cd /src/dist && sha256sum -c "checksums-${BUILD_ARCH}.txt")
(cd /src/qa && sha256sum -c "checksums-${BUILD_ARCH}.txt")
case "$format" in
  deb) dpkg -i /src/dist/*.deb; binary=/usr/bin/pay-lmm ;;
  rpm) rpm -Uvh /src/dist/*.rpm; binary=/usr/bin/pay-lmm ;;
  tar)
    mkdir -p /tmp/release
    tar -xzf /src/dist/*.tar.gz -C /tmp/release
    set -- /tmp/release/pay-lmm-*
    bundle=$1
    sh "$bundle/install.sh" --system
    binary=/usr/local/bin/pay-lmm
    # Verify integrity is checked BEFORE replacing or running a corrupted binary.
    cp -R "$bundle" /tmp/corrupt-bundle
    printf '\nCORRUPTED\n' >> /tmp/corrupt-bundle/pay-lmm
    if sh /tmp/corrupt-bundle/install.sh --prefix /tmp/rejected-install; then exit 1; fi
    [ ! -e /tmp/rejected-install/bin/pay-lmm ]
    ;;
esac
[ "$(id -u pay-lmm)" -ne 0 ]
[ "$(stat -c %a /etc/pay.lmm.best)" = 750 ]
[ "$(stat -c %a /var/lib/pay-lmm)" = 700 ]
[ ! -e /run/pay-lmm.pid ]
[ ! -e /etc/systemd/system/multi-user.target.wants/pay-lmm.service ]
"$binary" --version
python3 - "$binary" <<'PY'
import hashlib, json, pathlib, sys
metadata = json.loads(next(pathlib.Path('/src/dist').glob('*.build.json')).read_text())
assert hashlib.sha256(pathlib.Path(sys.argv[1]).read_bytes()).hexdigest() == metadata['binary_sha256']
assert metadata['dirty'] is False
PY
# Exercise a full 64-order local-only lifecycle as the unprivileged service user.
su -s /bin/sh pay-lmm -c "python3 /src/scripts/smoke.py --binary $binary"
printf '# QA operator configuration - must survive\n' > /etc/pay.lmm.best/config.toml
printf "QA_SECRET='keep-this-synthetic-value'\n" > /etc/pay.lmm.best/secrets.env
chmod 0600 /etc/pay.lmm.best/secrets.env
python3 - <<'PY'
import sqlite3
with sqlite3.connect('/var/lib/pay-lmm/qa-evidence.sqlite3') as db:
    db.execute('CREATE TABLE evidence(id INTEGER PRIMARY KEY, value TEXT)')
    db.execute("INSERT INTO evidence VALUES(1, 'must-survive-upgrade-and-removal')")
PY
case "$format" in
  deb)
    dpkg -i /src/dist/*.deb
    dpkg -i /src/qa/*.deb
    dpkg --purge pay-lmm
    ;;
  rpm)
    rpm -Uvh --replacepkgs /src/dist/*.rpm
    rpm -Uvh /src/qa/*.rpm
    rpm -e pay-lmm
    ;;
  tar)
    sh "$bundle/install.sh" --system
    if [ "$ID" = alpine ]; then /etc/init.d/pay-lmm describe; fi
    sh "$bundle/install.sh" --system --uninstall
    ;;
esac
[ ! -e "$binary" ]
grep -q 'QA operator configuration' /etc/pay.lmm.best/config.toml
grep -q 'keep-this-synthetic-value' /etc/pay.lmm.best/secrets.env
[ "$(stat -c %a /etc/pay.lmm.best/secrets.env)" = 600 ]
id pay-lmm
python3 - <<'PY'
import sqlite3
with sqlite3.connect('/var/lib/pay-lmm/qa-evidence.sqlite3') as db:
    assert db.execute('SELECT value FROM evidence WHERE id=1').fetchone()[0] == 'must-survive-upgrade-and-removal'
PY
printf '\nPASS: %s / %s / %s: install, unprivileged lifecycle, upgrade/reinstall, removal, data retention\n' "$ID" "$BUILD_ARCH" "$format"
