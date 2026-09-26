# Security and operational contract

## Secrets and trust

Credentials are read from environment variables whose names appear in the operator-only TOML. Merchant API keys and outbound HMAC secrets must be different and at least 32 bytes long; generate random values, not passwords. API key hashes are kept in memory. Provider private keys remain server-side. No real keys are checked into this repository.

Use separate deployments or at minimum separate merchant keys, notification endpoints, gateway IDs and stores for testing and production. A test callback is valid only in its explicitly configured test channel. The downstream merchant must never grant live goods based on a test channel. No automatic test-key fallback is used in production verification.

Use HTTPS at the public endpoint, an authenticated administrative host, restrictive file permissions and a private configuration directory. The example service uses `UMask=0077`. Protect the SQLite file, WAL, lock file and backups: they contain business metadata and checkout URLs. Do not add access logs that include query strings on ePay callbacks, bodies, Authorization headers or provider signatures.

## Verify merchant notifications

`X-Pay-Signature` is `t=<unix_seconds>,v1=<hex_hmac_sha256>`. Sign the ASCII timestamp, a dot, then the **exact raw HTTP body**. Do not deserialize and reserialize before verification. Recommended receiver clock tolerance: five minutes. Delivery retries have a fresh timestamp and signature but the same event ID and body.

Example Python verification (standard library only):

```python
import hashlib
import hmac
import json
import time

def verify_notification(raw: bytes, header: str, secret: bytes, now=None):
    if len(raw) > 65536 or len(header) > 512:
        raise ValueError('oversized notification')
    pairs = [part.strip().split('=', 1) for part in header.split(',')]
    if any(len(pair) != 2 for pair in pairs):
        raise ValueError('invalid signature header')
    fields = dict(pairs)
    if len(pairs) != 2 or set(fields) != {'t', 'v1'}:
        raise ValueError('duplicate or missing signature fields')
    timestamp = fields['t']
    if not timestamp.isascii() or not timestamp.isdigit():
        raise ValueError('invalid timestamp')
    now = time.time() if now is None else now
    if abs(now - int(timestamp)) > 300:
        raise ValueError('stale notification')
    expected = hmac.new(secret, timestamp.encode('ascii') + b'.' + raw,
                        hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected, fields['v1']):
        raise ValueError('invalid signature')
    event = json.loads(raw)
    if event.get('event_type') != 'payment.succeeded':
        raise ValueError('unexpected event type')
    return event
```

After verification, compare the payment's merchant order ID, currency, amount and gateway against your own original order. In one business-database transaction, insert a unique event ID and grant the purchased goods only if that insertion is new. Then return 2xx. Header `X-Pay-Event-Id` must agree with the signed body's `id`; the signed body is authoritative.

When `amount_basis=subtotal`, `amount_minor` represents the requested subtotal and `charged_minor` represents the verified total paid including tax. Otherwise the request amount is the verified total. There is no automatic FX rate or amount rounding. Current currencies are explicitly enumerated; unsupported currencies are rejected.

## Provider callbacks

Waffo: RSA-SHA256 (PKCS#1 v1.5), minimum RSA 2048 bits; `X-Waffo-Signature` signs `<milliseconds>.<raw_body>`. The past-facing window is 45 minutes and future allowance one minute, matching the referenced official SDK's retry model. Replay deduplication persists independently of this window. Merchant API request timestamps use **seconds**, unlike webhook timestamps. Configured public keys are supplied by the provider, not by the incoming request.

ePay: legacy v1 MD5 signing is implemented only for compatibility. This weaker legacy scheme is not used for our API key authentication or merchant notifications. MD5 signatures cannot be upgraded without upstream support. Use trusted gateways over HTTPS; confirm their exact implementation and risk acceptance. Duplicate form fields and malformed percent encodings are rejected. Provider PID, order binding, amount, payment type and success state are checked after signature validation.

## Outbound network policy

No caller-supplied URL is accepted in the order schema. Configuration URLs must use HTTPS, except an explicit loopback-only development mode. Userinfo, fragments and literal non-public IPs are rejected. The HTTP resolver validates the addresses that the connection actually uses, including IPv4-mapped IPv6. A DNS answer containing a non-public address fails closed. Redirects and inherited HTTP proxy settings are disabled. Responses are bounded, and connections and total operations have timeouts.

These application controls complement, not replace, a network egress policy. Deploy an outbound firewall denying metadata endpoints, internal ranges and unneeded destinations. DNS, firewall, CA trust, filesystem permissions and provider configuration are operator trust boundaries.

## Availability and known limitations

The request semaphore bounds application work; a reverse proxy must also bound concurrent TCP connections, header timeouts and unauthenticated request rates. There is no built-in distributed rate limiter. HTTP bodies and upstream responses default to 64 KiB; compressed bodies are not accepted. SQLite requires local reliable storage, one process and verified backups.

A successful HTTP redirect or a provider timeout is never payment evidence. Do not manually mark `unknown` orders successful without provider-side proof. There is no authenticated manual mark-paid endpoint, and no automatic retry-create or active provider lookup. Unexpected signed events are not money movements.

No independent security audit or real merchant sandbox certification is claimed. Integration tests use local mock providers and ephemeral RSA keys, and are distinct from live financial transactions.
