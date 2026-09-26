use pay_lmm::{
    gateway::{Checkout, VerifiedEvent},
    store::{Database, Record},
    wire::{CreatePaymentRequest, Payment},
};

fn record(id: &str) -> Record {
    Record {
        payment: Payment {
            id: id.into(),
            merchant_order_id: id.into(),
            gateway_id: "gateway".into(),
            amount_minor: 100,
            currency: "USD".into(),
            method: "checkout".into(),
            status: "creating".into(),
            amount_basis: "total".into(),
            ..Default::default()
        },
        input: CreatePaymentRequest {
            merchant_order_id: id.into(),
            amount_minor: 100,
            currency: "USD".into(),
            method: "checkout".into(),
            description: "Test".into(),
            ..Default::default()
        },
        merchant_id: "merchant".into(),
        idempotency_key: id.into(),
        request_hash: format!("hash-{id}"),
        gateway_identity: "identity".into(),
        notify_url: "https://merchant.example/hook".into(),
        session_id: String::new(),
        provider_payment_id: String::new(),
    }
}
fn event(payment: &str, id: &str) -> VerifiedEvent {
    VerifiedEvent {
        id: id.into(),
        fingerprint: format!("fingerprint-{id}"),
        payment_id: payment.into(),
        provider_order_id: "ORD_1".into(),
        provider_payment_id: "PAY_1".into(),
        currency: "USD".into(),
        method: None,
        total_minor: 100,
        subtotal_minor: Some(100),
        charged_minor: 100,
    }
}
#[tokio::test]
async fn durable_recovery_single_instance_and_callback_before_create_response() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir
        .path()
        .join("payments.sqlite")
        .to_string_lossy()
        .into_owned();
    {
        let db = Database::open(&path, 512).unwrap();
        assert!(Database::open(&path, 512).is_err());
        db.reserve(record("pay_inflight")).await.unwrap();
        db.reserve(record("pay_paid")).await.unwrap();
        db.accept("gateway", "identity", event("pay_paid", "event-1"))
            .await
            .unwrap();
        let p = db
            .finish_create(
                "pay_paid",
                Some(Checkout {
                    url: "https://checkout.example/1".into(),
                    session_id: "SESSION_1".into(),
                }),
            )
            .await
            .unwrap();
        assert_eq!(p.status, "succeeded");
    }
    let db = Database::open(&path, 512).unwrap();
    assert_eq!(
        db.get("merchant", "pay_inflight").await.unwrap().status,
        "unknown"
    );
    assert_eq!(
        db.get("merchant", "pay_paid").await.unwrap().status,
        "succeeded"
    );
    db.accept("gateway", "identity", event("pay_paid", "event-1"))
        .await
        .unwrap();
    let count = db
        .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[tokio::test]
async fn receipt_reuse_amounts_and_provider_identity_are_checked_atomically() {
    let db = Database::open(":memory:", 512).unwrap();
    db.reserve(record("pay_1")).await.unwrap();
    db.reserve(record("pay_2")).await.unwrap();
    assert!(
        db.accept("gateway", "wrong-identity", event("pay_1", "event-1"))
            .await
            .is_err()
    );
    let mut wrong = event("pay_1", "event-1");
    wrong.currency = "CNY".into();
    assert!(db.accept("gateway", "identity", wrong).await.is_err());
    db.accept("gateway", "identity", event("pay_1", "event-1"))
        .await
        .unwrap();
    let mut changed = event("pay_1", "event-1");
    changed.fingerprint = "changed-body".into();
    assert!(db.accept("gateway", "identity", changed).await.is_err());
    assert!(
        db.accept("gateway", "identity", event("pay_2", "event-2"))
            .await
            .is_err()
    );
    assert_eq!(
        db.get("merchant", "pay_2").await.unwrap().status,
        "creating"
    );
    let count = db
        .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM receipts", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[tokio::test]
async fn notification_leases_recover_and_dead_letters_require_owner() {
    let db = Database::open(":memory:", 512).unwrap();
    db.reserve(record("pay_1")).await.unwrap();
    db.accept("gateway", "identity", event("pay_1", "event-1"))
        .await
        .unwrap();
    let first = db.claim().await.unwrap().unwrap();
    assert_eq!(first.attempt, 1);
    assert!(db.claim().await.unwrap().is_none());
    db.call(|c| {
        c.execute("UPDATE outbox SET lease_until=0", [])?;
        Ok(())
    })
    .await
    .unwrap();
    let second = db.claim().await.unwrap().unwrap();
    assert_eq!(second.attempt, 2);
    db.finish_delivery(&first.id, first.attempt, true)
        .await
        .unwrap();
    assert_eq!(
        db.get("merchant", "pay_1")
            .await
            .unwrap()
            .notification_status,
        "delivering"
    );
    db.call(|c| {
        c.execute("UPDATE outbox SET attempts=16,lease_until=0", [])?;
        Ok(())
    })
    .await
    .unwrap();
    assert!(db.claim().await.unwrap().is_none());
    assert_eq!(
        db.get("merchant", "pay_1")
            .await
            .unwrap()
            .notification_status,
        "dead"
    );
    assert!(db.retry_dead("other-merchant", "pay_1").await.is_err());
    assert_eq!(
        db.retry_dead("merchant", "pay_1")
            .await
            .unwrap()
            .notification_status,
        "pending"
    );
    let retried = db.claim().await.unwrap().unwrap();
    assert_eq!(retried.attempt, 1);
    db.finish_delivery(&retried.id, retried.attempt, true)
        .await
        .unwrap();
    assert_eq!(
        db.get("merchant", "pay_1")
            .await
            .unwrap()
            .notification_status,
        "delivered"
    );
}
#[tokio::test]
async fn subtotal_is_explicit_and_missing_evidence_cannot_pay_an_order() {
    let db = Database::open(":memory:", 512).unwrap();
    let mut r = record("pay_1");
    r.payment.amount_basis = "subtotal".into();
    db.reserve(r).await.unwrap();
    let mut e = event("pay_1", "event-1");
    e.total_minor = 110;
    e.charged_minor = 110;
    e.subtotal_minor = None;
    assert!(db.accept("gateway", "identity", e.clone()).await.is_err());
    e.subtotal_minor = Some(100);
    db.accept("gateway", "identity", e).await.unwrap();
    let p = db.get("merchant", "pay_1").await.unwrap();
    assert_eq!(p.amount_minor, 100);
    assert_eq!(p.charged_minor, 110);
}
