# Architecture

## Responsibility boundary

This is a payment-gateway protocol aggregator. A merchant supplies credentials for an existing provider. The provider owns checkout, payment processing and any settlement; the aggregator only routes messages, persists local order evidence and relays verified results. No ledger, balance, acquiring, settlement, payout or card-entry subsystem exists here.

The service is not a generic HTTP relay. Callers cannot supply upstream endpoints, secrets, arbitrary headers, notification URLs, or adapter code. These are operator-managed configuration. Merchants have separate API keys, order namespaces and channel allowlists.

## Modules

- `wire`: generated from the single canonical `.proto`, shared by JSON and binary HTTP bodies.
- `api`: authentication, bounded requests, HTTP representation and routing; no provider signing rules.
- `service`: resolves merchant and channel, reserves an order, invokes one adapter, and runs one delivery worker.
- `gateway::Adapter`: `prepare`, `create`, `verify`. Each protocol owns its exact wire format and signature algorithm. Verification produces a typed `VerifiedEvent`, never a database write.
- `store::Database`: a single SQLite connection behind a mutex, executed on the bounded blocking pool. Transactions bind provider payments, deduplicate receipts and enqueue notifications atomically.
- `network::SafeClient`: shared TLS client with public-address DNS policy, disabled proxy inheritance and redirects, response bounds and timeouts.
- `crypto` / `money`: narrowly scoped cryptographic interoperability and checked minor-unit conversions.

## Checkout sequence

1. Authenticate the merchant and decode JSON or Protobuf.
2. Validate business fields. Compute a hash of the normalized request, independent of HTTP encoding.
3. Look up `(merchant, idempotency_key)`. Same request returns the existing order; different request conflicts.
4. Resolve an allowed configured gateway. Persist `creating` before any upstream operation. `(merchant, merchant_order_id)` is unique too.
5. ePay returns a signed hosted-checkout URL without server-side charging. Waffo sends one signed create-session request with the local payment ID as the upstream idempotency key.
6. Persist checkout metadata and `pending`. On ambiguous failure, persist `unknown`. Never silently pick another gateway or submit a second creation request.

A response can race a callback. A completed payment remains `succeeded` even if a delayed create response or error arrives afterwards. Following a browser success URL never updates an order.

## Callback transaction

The adapter first verifies the raw provider payload. Waffo additionally requires the configured store and mode, consistent status fields, a valid signature timestamp and exact amount evidence. The store then checks the frozen gateway identity, order binding, currency, requested amount basis and actual charge. Provider order/payment IDs cannot be assigned to another local payment in the same gateway.

The transaction stores the successful state, durable receipt and one frozen notification payload. An upstream acknowledgement is sent only after commit. Replaying the same event is harmless. Reusing the event ID with changed contents fails closed. A later event cannot downgrade a successful payment.

Irrelevant, correctly signed Waffo lifecycle events are acknowledged without granting entitlements. Refunds and subscriptions are not implemented and are never simulated as successful operations.

## Notification delivery

The worker claims one persistent outbox row with a lease. It signs the exact frozen JSON bytes using the merchant's HMAC key and sends them to the snapshotted configured URL. A 2xx response completes delivery. Failures use exponential backoff (up to one hour) and become `dead` after 16 attempts. An expired lease is recoverable after a process crash. A stale worker completion cannot overwrite a later attempt.

Delivery is **at least once**. A receiver can commit an event and lose the response; a retry is then necessary and expected. Receivers must atomically deduplicate event IDs before performing their own business fulfillment.

## Resource model and limits

The executable has one Tokio event-loop thread, at most two blocking threads, one SQLite connection, one outbox worker, bounded active requests, bounded response buffers and a small explicitly configured SQLite cache. Idle HTTP connections are limited per upstream. There is no Redis, message-broker service, ORM or JVM/Node runtime dependency.

Orders, receipts and outbox rows grow on disk. This release intentionally does not auto-delete idempotency evidence. Disk capacity, backup and retention must be operated deliberately. Do not remove receipts casually: the upstream ePay protocol has no trustworthy callback timestamp.

Only one process may use a SQLite file; a filesystem lock rejects another instance. This is not a distributed or highly available storage implementation. Backups must include a consistent SQLite snapshot, not a live copy of only the main database file while its WAL is active.

## Frozen configuration and changes

Each payment records a digest of its gateway's non-secret configuration. Changing a channel's protocol, upstream identity, environment, product mapping, endpoint or credential environment-variable name while orders are pending causes old callbacks to fail closed. Preserve the old gateway configuration and ID until orders settle; add a new ID for changed accounts. Rotating a secret value under the same environment-variable name does not change this digest, but old provider signatures may then need the provider's supported key-rotation process.

There is no automatic cross-provider fallback after submission, currency conversion, standalone refunds, subscription entitlement engine or active reconciliation in this version. `unknown` requires provider-side investigation unless a valid callback later resolves it.
