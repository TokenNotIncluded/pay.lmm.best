#!/usr/bin/env python3
"""Validate the scratch image, its non-root user and a local-only payment flow."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import time
import urllib.parse
import urllib.request


def command(*args):
    return subprocess.check_output(args, text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('image')
    args = parser.parse_args()
    info = json.loads(command('docker', 'image', 'inspect', args.image))[0]
    assert info['Config']['User'] == '10001:10001'
    assert info['Config']['Entrypoint'] == ['/usr/bin/pay-lmm']
    assert command('docker', 'run', '--rm', args.image, '--version').startswith('pay-lmm ')
    with tempfile.TemporaryDirectory(prefix='pay-lmm-image-') as directory:
        root = Path(directory)
        root.chmod(0o755)
        config = root / 'config.toml'
        config.write_text('''[server]
listen="0.0.0.0:8080"
public_url="https://pay.example.com"
database="/var/lib/pay-lmm/pay.sqlite3"
allow_loopback_http=true
[[merchants]]
id="qa"
api_key_env="QA_API"
webhook_secret_env="QA_HMAC"
webhook_url="http://127.0.0.1:9/no-receiver"
default_gateway="epay"
gateways=["epay"]
[[gateways]]
id="epay"
protocol="epay"
base_url="https://epay.example.com"
checkout_origins=["https://epay.example.com"]
currencies=["CNY"]
methods=["alipay"]
[gateways.epay]
pid="1000"
key_env="QA_EPAY"
''')
        config.chmod(0o644)
        cid = command('docker', 'run', '--detach', '--read-only', '--cap-drop=ALL',
                      '--security-opt=no-new-privileges', '--tmpfs', '/tmp:rw,nosuid,noexec',
                      '--tmpfs', '/var/lib/pay-lmm:rw,uid=10001,gid=10001,mode=0700',
                      '-p', '127.0.0.1::8080', '--mount', f'type=bind,source={config},target=/etc/pay.lmm.best/config.toml,readonly',
                      '-e', 'QA_API=' + 'a' * 40, '-e', 'QA_HMAC=' + 'b' * 40, '-e', 'QA_EPAY=synthetic-only', args.image)
        try:
            port = command('docker', 'port', cid, '8080/tcp').rsplit(':', 1)[1]
            origin = f'http://127.0.0.1:{port}'
            opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

            def request(path, payload=None):
                body = json.dumps(payload).encode() if payload is not None else None
                req = urllib.request.Request(origin + path, body, headers={
                    'Authorization': 'Bearer ' + 'a' * 40,
                    'Content-Type': 'application/json', 'Idempotency-Key': 'image-qa',
                })
                with opener.open(req, timeout=5) as response:
                    return response.status, response.read(65536)

            for _ in range(100):
                try:
                    if request('/readyz')[0] == 200:
                        break
                except OSError:
                    time.sleep(0.1)
            else:
                raise RuntimeError('Container did not become ready')
            status, body = request('/v1/payments', {'merchant_order_id': 'image-qa', 'amount_minor': 123,
                'currency': 'CNY', 'method': 'alipay', 'description': 'Synthetic image check'})
            assert status == 201
            payment = json.loads(body)
            values = {'pid': '1000', 'out_trade_no': payment['id'], 'trade_no': 'IMAGE_QA',
                'type': 'alipay', 'money': '1.23', 'trade_status': 'TRADE_SUCCESS'}
            canonical = '&'.join(f'{key}={values[key]}' for key in sorted(values)) + 'synthetic-only'
            values['sign'] = hashlib.md5(canonical.encode(), usedforsecurity=False).hexdigest()
            values['sign_type'] = 'MD5'
            assert request('/hooks/epay?' + urllib.parse.urlencode(values))[0] == 200
            assert json.loads(request('/v1/payments/' + payment['id'])[1])['status'] == 'succeeded'
            print('PASS: scratch image, default non-root UID, read-only rootfs, SQLite and verified callback')
        except Exception:
            print(command('docker', 'logs', cid))
            raise
        finally:
            subprocess.run(['docker', 'rm', '--force', cid], check=True)


if __name__ == '__main__':
    main()
