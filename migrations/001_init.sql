BEGIN IMMEDIATE;
CREATE TABLE payments (
  id TEXT PRIMARY KEY,
  merchant_id TEXT NOT NULL,
  merchant_order_id TEXT NOT NULL,
  idempotency_key TEXT NOT NULL,
  gateway_id TEXT NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('creating','pending','unknown','succeeded')),
  provider_order_id TEXT,
  provider_payment_id TEXT,
  data TEXT NOT NULL CHECK(json_valid(data)),
  UNIQUE(merchant_id, merchant_order_id),
  UNIQUE(merchant_id, idempotency_key),
  UNIQUE(gateway_id, provider_order_id),
  UNIQUE(gateway_id, provider_payment_id)
);
CREATE TABLE receipts (
  gateway_id TEXT NOT NULL,
  event_id TEXT NOT NULL,
  fingerprint TEXT NOT NULL,
  payment_id TEXT NOT NULL REFERENCES payments(id),
  received_at INTEGER NOT NULL,
  PRIMARY KEY(gateway_id,event_id)
);
CREATE TABLE outbox (
  id TEXT PRIMARY KEY,
  merchant_id TEXT NOT NULL,
  payment_id TEXT NOT NULL UNIQUE REFERENCES payments(id),
  url TEXT NOT NULL,
  payload BLOB NOT NULL,
  status TEXT NOT NULL CHECK(status IN ('pending','delivering','delivered','dead')),
  attempts INTEGER NOT NULL DEFAULT 0,
  due INTEGER NOT NULL,
  lease_until INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX outbox_due ON outbox(status,due);
PRAGMA user_version=1;
COMMIT;
