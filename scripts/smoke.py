#!/usr/bin/env python3
"""Run the release binary against synthetic LOCAL callbacks; never contact a PSP.

The RSS readings are Linux process samples, not a throughput benchmark or SLA.
Only the Python standard library is required. No production credentials are used.
"""
import argparse
import hashlib
import hmac
import http.server
import json
import os
from pathlib import Path
import platform
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.parse
import urllib.request


def memory(pid):
    result = {}
    path = Path(f'/proc/{pid}/status')
    if path.exists():
        for line in path.read_text().splitlines():
            key, _, value = line.partition(':')
            if key in ('VmRSS', 'VmHWM', 'Threads'):
                result[key] = int(value.strip().split()[0])
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='target/release/pay-lmm')
    args = parser.parse_args()
    binary = Path(args.binary).resolve()
    api_key, webhook_key = secrets.token_hex(32), secrets.token_hex(32)
    epay_key = secrets.token_hex(16)
    received = set()
    receipt_lock = threading.Lock()

    class Receiver(http.server.BaseHTTPRequestHandler):
        def do_POST(self):
            body = self.rfile.read(int(self.headers.get('Content-Length', '0')))
            fields = dict(part.strip().split('=', 1) for part in self.headers['X-Pay-Signature'].split(','))
            expected = hmac.new(webhook_key.encode(), fields['t'].encode() + b'.' + body, hashlib.sha256).hexdigest()
            assert hmac.compare_digest(expected, fields['v1'])
            event = json.loads(body)
            assert event['event_type'] == 'payment.succeeded'
            assert event['id'] == self.headers['X-Pay-Event-Id']
            with receipt_lock:
                received.add(event['id'])
            self.send_response(204)
            self.end_headers()

        def log_message(self, *_):
            pass

    receiver = http.server.HTTPServer(('127.0.0.1', 0), Receiver)
    receiver_thread = threading.Thread(target=receiver.serve_forever, daemon=True)
    receiver_thread.start()
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        port = sock.getsockname()[1]
    origin = f'http://127.0.0.1:{port}'
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def request(path, data=None, headers=None):
        req = urllib.request.Request(origin + path, data=data, headers=headers or {})
        with opener.open(req, timeout=5) as response:
            return response.status, response.read(65537)

    with tempfile.TemporaryDirectory(prefix='pay-lmm-smoke-') as directory:
        path = Path(directory)
        config = path / 'config.toml'
        config.write_text(f'''[server]
listen = "127.0.0.1:{port}"
public_url = "{origin}"
database = "{path / 'pay.sqlite3'}"
allow_loopback_http = true
[[merchants]]
id = "smoke"
api_key_env = "SMOKE_API"
webhook_secret_env = "SMOKE_WEBHOOK"
webhook_url = "http://127.0.0.1:{receiver.server_port}/notify"
default_gateway = "epay"
gateways = ["epay"]
[[gateways]]
id = "epay"
protocol = "epay"
base_url = "https://epay.example.com"
checkout_origins = ["https://epay.example.com"]
currencies = ["CNY"]
methods = ["alipay"]
[gateways.epay]
pid = "1000"
key_env = "SMOKE_EPAY"
''')
        env = dict(os.environ, SMOKE_API=api_key, SMOKE_WEBHOOK=webhook_key, SMOKE_EPAY=epay_key)
        with (path / 'server.log').open('wb') as log:
            process = subprocess.Popen([str(binary), '--config', str(config)], env=env, stdout=log, stderr=log)
            try:
                for _ in range(100):
                    if process.poll() is not None:
                        raise RuntimeError('binary exited before becoming ready')
                    try:
                        if request('/readyz')[0] == 200:
                            break
                    except OSError:
                        time.sleep(0.05)
                else:
                    raise RuntimeError('service did not become ready')
                idle = memory(process.pid)
                payments = []
                for i in range(64):
                    data = json.dumps({'merchant_order_id': f'smoke-{i}', 'amount_minor': 123,
                                       'currency': 'CNY', 'method': 'alipay', 'description': 'Synthetic smoke order'}).encode()
                    status, body = request('/v1/payments', data, {'Authorization': f'Bearer {api_key}',
                        'Content-Type': 'application/json', 'Idempotency-Key': f'smoke-{i}'})
                    assert status == 201
                    payment = json.loads(body)
                    payments.append(payment['id'])
                    values = {'pid': '1000', 'type': 'alipay', 'out_trade_no': payment['id'],
                              'trade_no': f'SYNTHETIC_{i}', 'money': '1.23', 'trade_status': 'TRADE_SUCCESS'}
                    canonical = '&'.join(f'{key}={values[key]}' for key in sorted(values)) + epay_key
                    values['sign'] = hashlib.md5(canonical.encode(), usedforsecurity=False).hexdigest()
                    values['sign_type'] = 'MD5'
                    assert request('/hooks/epay?' + urllib.parse.urlencode(values))[0] == 200
                for _ in range(200):
                    with receipt_lock:
                        done = len(received)
                    if done == len(payments):
                        break
                    time.sleep(0.05)
                assert done == 64, f'only {done}/64 notifications delivered'
                for payment in payments:
                    _, body = request('/v1/payments/' + payment, headers={'Authorization': f'Bearer {api_key}'})
                    assert json.loads(body)['status'] == 'succeeded'
                loaded = memory(process.pid)
                report = {'scope': 'local-only synthetic ePay lifecycle smoke; not a stress benchmark',
                          'platform': platform.platform(), 'orders': len(payments), 'verified_notifications': done,
                          'idle_rss_kib': idle.get('VmRSS'), 'sampled_rss_kib': loaded.get('VmRSS'),
                          'process_peak_rss_kib': loaded.get('VmHWM'), 'threads_after_smoke': loaded.get('Threads'),
                          'binary_bytes': binary.stat().st_size}
                print(json.dumps(report, indent=2))
            finally:
                process.terminate()
                try:
                    process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
    receiver.shutdown()
    receiver.server_close()


if __name__ == '__main__':
    main()
