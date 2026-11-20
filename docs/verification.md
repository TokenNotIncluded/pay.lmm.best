# Verification record

This file records an observed CI run, not a prediction or a production service-level claim.

## Reference run

- Source commit: [`bd22183ee9623a7df3e0e0e2391b2890a79604ea`](https://github.com/TokenNotIncluded/pay.lmm.best/commit/bd22183ee9623a7df3e0e0e2391b2890a79604ea).
- GitHub Actions: [Rust CI run 36255543678](https://github.com/TokenNotIncluded/pay.lmm.best/actions/runs/36255543678), test job `108441520121`.
- Execution date: 2026-09-26 UTC.
- Platform: Ubuntu 24.04.5, x86_64, Linux 6.17.0-1022-azure, glibc 2.39.
- Compiler: rustc 1.98.1; release profile from this repository.
- Subsequent initialization commits apply formatting and tighten CI permissions; future runs publish their own measured report.

## Automated checks observed

| Check | Result |
| --- | --- |
| Library unit tests | 7 passed |
| HTTP / JSON / Protobuf integration tests | 3 passed |
| Outbound transport integration test | 1 passed |
| Persistent storage / crash recovery tests | 4 passed |
| Waffo RSA and local protocol integration test | 1 passed |
| Total | **16 passed, 0 failed, 0 ignored** |
| `cargo clippy --locked --all-targets -- -D warnings` | Passed |
| `cargo build --locked --release` | Passed |
| Release-binary synthetic payment lifecycle | **64 orders, 64 verified merchant notifications** |

The Waffo integration test generates ephemeral RSA keys using OpenSSL and compares request signatures with independently produced OpenSSL signatures. A local HTTP provider validates canonical signed requests and returns synthetic checkout sessions. Signed callbacks cover valid completion, raw-body tampering, stale timestamps, wrong store, wrong environment, amount and tax mismatches, contradictory states, irrelevant subscription events, duplicate delivery, and a late successful callback resolving an `unknown` creation outcome. This is a protocol interoperability test, **not a live Waffo sandbox certification**.

The storage tests cover a callback arriving before the checkout response, restart recovery, single-process file locking, provider-payment identity reuse, event ID reuse with changed payload, atomic validation failures, expired notification leases, stale worker completions, dead-letter ownership, and explicit subtotal evidence.

## Observed process memory

The smoke script starts the release executable against a temporary SQLite file and a local Python merchant-notification receiver. It creates 64 ePay-format orders, submits locally generated synthetic signed callbacks, verifies all HMAC merchant notifications, and queries the completed orders. It never opens the returned checkout links or contacts a real payment gateway. Memory numbers come from `/proc/<pid>/status` of the Rust process only, excluding the Python driver and receiver.

```json
{
  "scope": "local-only synthetic ePay lifecycle smoke; not a stress benchmark",
  "platform": "Linux-6.17.0-1022-azure-x86_64-with-glibc2.39",
  "orders": 64,
  "verified_notifications": 64,
  "idle_rss_kib": 6348,
  "sampled_rss_kib": 6628,
  "process_peak_rss_kib": 6628,
  "threads_after_smoke": 2,
  "binary_bytes": 4603000
}
```

This corresponds to approximately **6.20 MiB idle RSS**, **6.47 MiB sampled / recorded peak RSS**, and a **4.39 MiB executable** for that run. It is not a concurrency benchmark, a maximum-RSS guarantee, a Waffo TLS-load measurement, a Go-versus-Rust comparison, or a promise about a different host. Runtime memory can vary with concurrency, TLS connections, configuration, operating system, allocator and workload.

Reproduce after building:

```sh
cargo build --release --locked
python3 scripts/smoke.py --binary target/release/pay-lmm
```

CI captures each run's `smoke-report.json` beside the Linux binary artifact and checks the order / notification counts. The final workflow uses fail-on-pipeline-error shell behavior, read-only repository permissions, pinned action revisions, and a formatting check rather than a repository-writing formatting job.

## Not verified by these checks

No production provider credentials, real card information or real money were used. Live ePay variant compatibility, Waffo account/product approval, tax configuration, checkout-domain allowlists, provider-side webhook registration, DNS/TLS deployment, independent security auditing, container-image execution and high-concurrency capacity still require their respective acceptance checks.

Refund initiation, subscription lifecycle handling, active upstream order queries and automatic reconciliation are intentionally not implemented in this initial version. Their advertised capability flags are false. A successful software test does not make them available, and an `unknown` order must not be treated as a successful payment without provider evidence.
