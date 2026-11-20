use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{Path, State},
    http::{HeaderMap, Request, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use pay_lmm::{
    api,
    config::Config,
    crypto,
    service::Service,
    wire::{CreatePaymentRequest, GatewayList, Payment},
};
use prost::Message;
use ring::hmac;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::Mutex;
use tower::ServiceExt;
use zeroize::Zeroizing;

const API: &str = "merchant-api-aaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SECRET: &str = "provider-webhook-bbbbbbbbbbbbbbbbbbbbbbbb";
#[derive(Clone)]
struct Upstream {
    origin: String,
    captured: Arc<Mutex<Value>>,
    posts: Arc<AtomicUsize>,
    reject: Arc<AtomicBool>,
    recurring: Arc<AtomicBool>,
}
async fn stripe_create(State(s): State<Upstream>, headers: HeaderMap, body: Bytes) -> Response {
    assert_eq!(headers["authorization"], "Bearer sk_test_fake_stripe_key");
    assert_eq!(headers["stripe-version"], "2025-06-30.basil");
    assert_eq!(headers["content-type"], "application/x-www-form-urlencoded");
    let f: BTreeMap<String, String> = url::form_urlencoded::parse(&body).into_owned().collect();
    assert_eq!(f["mode"], "payment");
    assert_eq!(f["automatic_tax[enabled]"], "false");
    assert_eq!(f["adaptive_pricing[enabled]"], "false");
    assert_eq!(f["allow_promotion_codes"], "false");
    assert_eq!(f["line_items[0][quantity]"], "1");
    assert_eq!(f["line_items[0][price_data][unit_amount]"], "1230");
    assert_eq!(f["line_items[0][price_data][currency]"], "usd");
    assert_eq!(
        f["client_reference_id"],
        headers["idempotency-key"].to_str().unwrap()
    );
    let meta = json!({"pay_lmm_payment_id":f["metadata[pay_lmm_payment_id]"],"pay_lmm_request_hash":f["metadata[pay_lmm_request_hash]"],"pay_lmm_binding":f["metadata[pay_lmm_binding]"]});
    *s.captured.lock().await = meta;
    let n = s.posts.fetch_add(1, Ordering::SeqCst) + 1;
    if s.reject.load(Ordering::SeqCst) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    axum::Json(json!({"id":format!("cs_test_{n}"),"object":"checkout.session","mode":"payment","livemode":false,"client_reference_id":f["client_reference_id"],"amount_total":1230,"currency":"usd","url":format!("{}/checkout/{n}#provider-fragment",s.origin)})).into_response()
}
async fn creem_product(
    State(s): State<Upstream>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    assert_eq!(headers["x-api-key"], "creem_test_fake_key");
    assert_eq!(id, "prod_credits");
    axum::Json(json!({"id":id,"mode":"test","object":"product","currency":"USD","billing_type":if s.recurring.load(Ordering::SeqCst){"recurring"}else{"onetime"},"status":"active","tax_mode":"inclusive"})).into_response()
}
async fn creem_create(State(s): State<Upstream>, headers: HeaderMap, body: Bytes) -> Response {
    assert_eq!(headers["x-api-key"], "creem_test_fake_key");
    let data: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(data["product_id"], "prod_credits");
    assert_eq!(data["units"], 1);
    assert_eq!(data["custom_price"], 1230);
    assert_eq!(data["request_id"], data["metadata"]["pay_lmm_payment_id"]);
    *s.captured.lock().await = data["metadata"].clone();
    let n = s.posts.fetch_add(1, Ordering::SeqCst) + 1;
    if s.reject.load(Ordering::SeqCst) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    axum::Json(json!({"id":format!("ch_{n}"),"mode":"test","product":"prod_credits","request_id":data["request_id"],"custom_price":1230,"checkout_url":format!("{}/checkout/{n}",s.origin)})).into_response()
}
async fn lemon_store(headers: HeaderMap) -> Response {
    assert_eq!(headers["authorization"], "Bearer lemon_test_fake_key");
    axum::Json(json!({"data":{"type":"stores","id":"123","attributes":{"currency":"USD"}}}))
        .into_response()
}
async fn lemon_variant(State(s): State<Upstream>, headers: HeaderMap) -> Response {
    assert_eq!(headers["accept"], "application/vnd.api+json");
    axum::Json(json!({"data":{"type":"variants","id":"456","attributes":{"test_mode":true,"is_subscription":s.recurring.load(Ordering::SeqCst),"pay_what_you_want":false,"status":"published"}}})).into_response()
}
async fn lemon_create(State(s): State<Upstream>, headers: HeaderMap, body: Bytes) -> Response {
    assert_eq!(headers["authorization"], "Bearer lemon_test_fake_key");
    assert_eq!(headers["content-type"], "application/vnd.api+json");
    let data: Value = serde_json::from_slice(&body).unwrap();
    let a = &data["data"]["attributes"];
    assert_eq!(a["custom_price"], 1230);
    assert_eq!(a["test_mode"], true);
    assert_eq!(a["preview"], true);
    assert_eq!(a["checkout_options"]["discount"], false);
    assert_eq!(a["checkout_options"]["skip_trial"], true);
    assert_eq!(a["product_options"]["enabled_variants"], json!([456]));
    assert_eq!(data["data"]["relationships"]["store"]["data"]["id"], "123");
    *s.captured.lock().await = a["checkout_data"]["custom"].clone();
    let n = s.posts.fetch_add(1, Ordering::SeqCst) + 1;
    if s.reject.load(Ordering::SeqCst) {
        return StatusCode::BAD_GATEWAY.into_response();
    }
    axum::Json(json!({"data":{"type":"checkouts","id":format!("lemon-checkout-{n}"),"attributes":{"test_mode":true,"store_id":123,"variant_id":456,"custom_price":1230,"preview":{"currency":"USD","subtotal":1230,"discount_total":0,"tax":246,"total":1476},"url":format!("{}/checkout/{n}",s.origin)}}})).into_response()
}
fn config(kind: &str, origin: &str) -> Config {
    let options = match kind {
        "stripe" => {
            r#"[gateways.stripe]
api_key_env="STRIPE"
webhook_secret_env="SECRET"
mode="test"
api_version="2025-06-30.basil"
"#
        }
        "creem" => {
            r#"[gateways.creem]
api_key_env="CREEM"
webhook_secret_env="SECRET"
mode="test"
[[gateways.creem.products]]
alias="credits"
id="prod_credits"
currency="USD"
"#
        }
        "lemon_squeezy" => {
            r#"[gateways.lemon_squeezy]
api_key_env="LEMON"
webhook_secret_env="SECRET"
mode="test"
store_id="123"
[[gateways.lemon_squeezy.products]]
alias="credits"
id="456"
amount_basis="subtotal"
"#
        }
        _ => panic!("bad protocol"),
    };
    toml::from_str(&format!(
        r#"
[server]
database=":memory:"
allow_loopback_http=true
[[merchants]]
id="app"
api_key_env="API"
webhook_secret_env="MERCHANT_SECRET"
webhook_url="https://merchant.example/notify"
default_gateway="gateway"
gateways=["gateway"]
[[gateways]]
id="gateway"
protocol="{kind}"
base_url="{origin}"
checkout_origins=["{origin}"]
currencies=["USD"]
methods=["checkout"]
{options}
"#
    ))
    .unwrap()
}
fn load(name: &str) -> anyhow::Result<Zeroizing<String>> {
    Ok(Zeroizing::new(
        match name {
            "API" => API,
            "SECRET" => SECRET,
            "MERCHANT_SECRET" => "merchant-webhook-cccccccccccccccccccccccc",
            "STRIPE" => "sk_test_fake_stripe_key",
            "CREEM" => "creem_test_fake_key",
            "LEMON" => "lemon_test_fake_key",
            _ => panic!("unexpected key"),
        }
        .into(),
    ))
}
struct Harness {
    service: Arc<Service>,
    app: Router,
    upstream: Upstream,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn harness(kind: &str) -> Harness {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let upstream = Upstream {
        origin: origin.clone(),
        captured: Arc::new(Mutex::new(Value::Null)),
        posts: Arc::new(AtomicUsize::new(0)),
        reject: Arc::new(AtomicBool::new(false)),
        recurring: Arc::new(AtomicBool::new(false)),
    };
    let router = Router::new()
        .route("/v1/checkout/sessions", post(stripe_create))
        .route("/v1/products/{id}", get(creem_product))
        .route("/v1/stores/123", get(lemon_store))
        .route("/v1/variants/456", get(lemon_variant))
        .route(
            "/v1/checkouts",
            if kind == "creem" {
                post(creem_create)
            } else {
                post(lemon_create)
            },
        )
        .with_state(upstream.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let service = Service::with_secrets(config(kind, &origin), load).unwrap();
    let app = api::router(service.clone());
    Harness {
        service,
        app,
        upstream,
        task,
    }
}
fn input(kind: &str, id: &str) -> CreatePaymentRequest {
    CreatePaymentRequest {
        merchant_order_id: id.into(),
        amount_minor: 1230,
        currency: "USD".into(),
        method: "checkout".into(),
        product: if kind == "stripe" {
            String::new()
        } else {
            "credits".into()
        },
        description: "Order with & unicode 商品".into(),
        ..Default::default()
    }
}
async fn create(h: &Harness, kind: &str, id: &str) -> Payment {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/payments")
        .header("Authorization", format!("Bearer {API}"))
        .header("Content-Type", "application/x-protobuf")
        .header("Accept", "application/x-protobuf")
        .header("Idempotency-Key", id)
        .body(Body::from(input(kind, id).encode_to_vec()))
        .unwrap();
    let res = h.app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let body = to_bytes(res.into_body(), 65536).await.unwrap();
    assert!(status.is_success(), "{kind} HTTP {status}: {body:?}");
    Payment::decode(body).unwrap()
}
fn event(kind: &str, meta: Value, n: u32) -> Value {
    match kind {
        "stripe" => {
            json!({"id":format!("evt_{n}"),"type":"checkout.session.completed","livemode":false,"data":{"object":{"id":format!("cs_test_{n}"),"object":"checkout.session","mode":"payment","status":"complete","payment_status":"paid","livemode":false,"amount_total":1230,"amount_subtotal":1230,"currency":"usd","payment_intent":format!("pi_{n}"),"client_reference_id":meta["pay_lmm_payment_id"],"metadata":meta,"total_details":{"amount_discount":0,"amount_shipping":0,"amount_tax":0}}}})
        }
        "creem" => {
            json!({"id":format!("evt_{n}"),"eventType":"checkout.completed","created_at":1,"object":{"id":format!("ch_{n}"),"object":"checkout","mode":"test","status":"completed","product":"prod_credits","request_id":meta["pay_lmm_payment_id"],"metadata":meta,"order":{"id":format!("ord_{n}"),"mode":"test","product":"prod_credits","amount":1230,"amount_paid":1230,"amount_due":1230,"discount_amount":0,"currency":"USD","status":"paid","type":"onetime"}}})
        }
        _ => {
            json!({"meta":{"event_name":"order_created","custom_data":meta},"data":{"type":"orders","id":n.to_string(),"attributes":{"store_id":123,"currency":"USD","status":"paid","refunded":false,"test_mode":true,"subtotal":1230,"discount_total":0,"tax":246,"total":1476,"tax_inclusive":false,"first_order_item":{"order_id":n,"variant_id":456}}}})
        }
    }
}
fn signature(kind: &str, body: &[u8], t: i64) -> (&'static str, String) {
    let key = hmac::Key::new(hmac::HMAC_SHA256, SECRET.as_bytes());
    if kind == "stripe" {
        let mut message = format!("{t}.").into_bytes();
        message.extend_from_slice(body);
        (
            "stripe-signature",
            format!(
                "t={t},v1={}",
                hex::encode(hmac::sign(&key, &message).as_ref())
            ),
        )
    } else {
        (
            if kind == "creem" {
                "creem-signature"
            } else {
                "x-signature"
            },
            hex::encode(hmac::sign(&key, body).as_ref()),
        )
    }
}
async fn send(h: &Harness, kind: &str, e: &Value) -> StatusCode {
    let body = serde_json::to_vec(e).unwrap();
    let (name, sig) = signature(kind, &body, pay_lmm::now());
    h.app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/hooks/gateway")
                .header("Content-Type", "application/json")
                .header(name, sig)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}
async fn exercise(kind: &str) {
    let h = harness(kind).await;
    let p = create(&h, kind, "order-1").await;
    assert_eq!(p.status, "pending", "{kind}");
    assert_eq!(h.upstream.posts.load(Ordering::SeqCst), 1);
    assert_eq!(create(&h, kind, "order-1").await.id, p.id);
    assert_eq!(h.upstream.posts.load(Ordering::SeqCst), 1);
    let list = h
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/gateways")
                .header("Authorization", format!("Bearer {API}"))
                .header("Accept", "application/x-protobuf")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let list = GatewayList::decode(to_bytes(list.into_body(), 65536).await.unwrap()).unwrap();
    assert_eq!(list.gateways[0].protocol, kind);
    assert!(!list.gateways[0].refunds);
    if kind == "stripe" {
        assert!(p.checkout_url.ends_with("#provider-fragment"));
        assert!(list.gateways[0].products.is_empty());
    } else {
        assert_eq!(list.gateways[0].products, ["credits"]);
    }
    let e = event(kind, h.upstream.captured.lock().await.clone(), 1);
    for case in [
        "amount",
        "currency",
        "mode",
        "binding",
        "state",
        "product",
        "missing_amount",
    ] {
        let mut bad = e.clone();
        let (amount, currency, mode, meta, state, product) = match kind {
            "stripe" => (
                "/data/object/amount_total",
                "/data/object/currency",
                "/data/object/livemode",
                "/data/object/metadata/pay_lmm_binding",
                "/data/object/status",
                "/data/object/id",
            ),
            "creem" => (
                "/object/order/amount",
                "/object/order/currency",
                "/object/mode",
                "/object/metadata/pay_lmm_binding",
                "/object/order/status",
                "/object/order/product",
            ),
            _ => (
                "/data/attributes/total",
                "/data/attributes/currency",
                "/data/attributes/test_mode",
                "/meta/custom_data/pay_lmm_binding",
                "/data/attributes/status",
                "/data/attributes/first_order_item/variant_id",
            ),
        };
        let (path, value) = match case {
            "amount" => (amount, json!(1231)),
            "currency" => (currency, json!("EUR")),
            "mode" => (
                mode,
                if kind == "creem" {
                    json!("prod")
                } else {
                    json!(kind != "lemon_squeezy")
                },
            ),
            "binding" => (meta, json!("00".repeat(32))),
            "state" => (state, json!("failed")),
            "product" => (
                product,
                if kind == "lemon_squeezy" {
                    json!(999)
                } else {
                    json!("other")
                },
            ),
            _ => (amount, Value::Null),
        };
        *bad.pointer_mut(path).unwrap() = value;
        assert!(
            send(&h, kind, &bad).await.is_client_error(),
            "{kind} accepted {case}"
        );
        assert_eq!(
            h.service.db.get("app", &p.id).await.unwrap().status,
            "pending"
        );
    }
    let raw = serde_json::to_vec(&e).unwrap();
    let (name, sig) = signature(kind, &raw, pay_lmm::now());
    let mut corrupted = raw.clone();
    corrupted.push(b' ');
    let invalid = Request::builder()
        .method("POST")
        .uri("/hooks/gateway")
        .header("Content-Type", "application/json")
        .header(name, &sig)
        .body(Body::from(corrupted))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(invalid).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
    let duplicate = Request::builder()
        .method("POST")
        .uri("/hooks/gateway")
        .header("Content-Type", "application/json")
        .header(name, &sig)
        .header(name, &sig)
        .body(Body::from(raw))
        .unwrap();
    assert!(
        h.app
            .clone()
            .oneshot(duplicate)
            .await
            .unwrap()
            .status()
            .is_client_error()
    );
    assert_eq!(send(&h, kind, &e).await, StatusCode::OK);
    assert_eq!(send(&h, kind, &e).await, StatusCode::OK);
    let paid = h.service.db.get("app", &p.id).await.unwrap();
    assert_eq!(paid.status, "succeeded");
    assert_eq!(
        paid.charged_minor,
        if kind == "lemon_squeezy" { 1476 } else { 1230 }
    );
    let count = h
        .service
        .db
        .call(|c| Ok(c.query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get::<_, i64>(0))?))
        .await
        .unwrap();
    assert_eq!(count, 1);
    h.upstream.reject.store(true, Ordering::SeqCst);
    let unknown = create(&h, kind, "order-2").await;
    assert_eq!(unknown.status, "unknown");
    assert_eq!(create(&h, kind, "order-2").await.id, unknown.id);
    assert_eq!(h.upstream.posts.load(Ordering::SeqCst), 2);
    let late = event(kind, h.upstream.captured.lock().await.clone(), 2);
    assert_eq!(send(&h, kind, &late).await, StatusCode::OK);
    assert_eq!(
        h.service.db.get("app", &unknown.id).await.unwrap().status,
        "succeeded"
    );
}
#[tokio::test]
async fn stripe_json_webhooks_protobuf_api_and_idempotency() {
    exercise("stripe").await;
}
#[tokio::test]
async fn creem_json_webhooks_protobuf_api_and_idempotency() {
    exercise("creem").await;
}
#[tokio::test]
async fn lemon_jsonapi_webhooks_protobuf_api_and_idempotency() {
    exercise("lemon_squeezy").await;
}
#[tokio::test]
async fn stripe_delayed_payments_signature_rotation_and_replay_window() {
    let h = harness("stripe").await;
    let p = create(&h, "stripe", "delayed").await;
    let mut e = event("stripe", h.upstream.captured.lock().await.clone(), 1);
    e["data"]["object"]["payment_status"] = json!("unpaid");
    assert_eq!(send(&h, "stripe", &e).await, StatusCode::OK);
    assert_eq!(
        h.service.db.get("app", &p.id).await.unwrap().status,
        "pending"
    );
    e["type"] = json!("checkout.session.async_payment_succeeded");
    e["data"]["object"]["payment_status"] = json!("paid");
    let body = serde_json::to_vec(&e).unwrap();
    for timestamp in [pay_lmm::now() - 301, pay_lmm::now() + 120] {
        let (name, sig) = signature("stripe", &body, timestamp);
        let req = Request::builder()
            .method("POST")
            .uri("/hooks/gateway")
            .header("Content-Type", "application/json")
            .header(name, sig)
            .body(Body::from(body.clone()))
            .unwrap();
        assert_eq!(
            h.app.clone().oneshot(req).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    let (name, sig) = signature("stripe", &body, pay_lmm::now());
    let header = format!("v1={},v0=ignored,{sig}", "00".repeat(32));
    let req = Request::builder()
        .method("POST")
        .uri("/hooks/gateway")
        .header("Content-Type", "application/json")
        .header(name, header)
        .body(Body::from(body))
        .unwrap();
    assert_eq!(
        h.app.clone().oneshot(req).await.unwrap().status(),
        StatusCode::OK
    );
    assert_eq!(
        h.service.db.get("app", &p.id).await.unwrap().status,
        "succeeded"
    );
}
#[tokio::test]
async fn catalog_preflight_prevents_creating_recurring_checkouts() {
    for kind in ["creem", "lemon_squeezy"] {
        let h = harness(kind).await;
        h.upstream.recurring.store(true, Ordering::SeqCst);
        let p = create(&h, kind, "bad-product").await;
        assert_eq!(p.status, "unknown");
        assert!(p.checkout_url.is_empty());
        assert_eq!(h.upstream.posts.load(Ordering::SeqCst), 0);
    }
}
#[test]
fn rejects_mixed_config_bad_origins_and_mode_mismatched_secrets() {
    for kind in ["stripe", "creem", "lemon_squeezy"] {
        let mut c = config(kind, "http://localhost:8080");
        assert!(c.validate().is_ok());
        c.gateways[0].epay = Some(pay_lmm::config::EpayConfig {
            pid: "x".into(),
            key_env: "y".into(),
        });
        assert!(c.validate().is_err());
        let mut c = config(kind, "https://evil.example");
        c.server.allow_loopback_http = false;
        assert!(c.validate().is_err());
    }
    for kind in ["stripe", "creem"] {
        let mut c = config(kind, "http://localhost:8080");
        if let Some(s) = &mut c.gateways[0].stripe {
            s.mode = pay_lmm::providers::Mode::Prod;
        }
        if let Some(s) = &mut c.gateways[0].creem {
            s.mode = pay_lmm::providers::Mode::Prod;
        }
        assert!(Service::with_secrets(c, load).is_err());
    }
}
#[test]
fn existing_gateway_identity_is_unchanged_by_new_optional_config() {
    let old = r#"{"id":"legacy","protocol":"epay","base_url":"https://epay.example","checkout_origins":["https://epay.example"],"currencies":["CNY"],"methods":["alipay"],"epay":{"pid":"1","key_env":"EPAY"},"waffo":null}"#;
    let gateway: pay_lmm::config::GatewayConfig = serde_json::from_str(old).unwrap();
    let serialized = serde_json::to_vec(&gateway).unwrap();
    assert_eq!(crypto::hash(old.as_bytes()), crypto::hash(&serialized));
}
#[test]
fn browser_fragment_does_not_weaken_outbound_ssrf_policy() {
    let allowed = vec!["https://checkout.stripe.com".into()];
    assert!(
        pay_lmm::network::checkout_url(
            "https://checkout.stripe.com/c/pay/123#abc",
            &allowed,
            false
        )
        .unwrap()
        .ends_with("#abc")
    );
    assert!(
        pay_lmm::network::safe_url("https://checkout.stripe.com/c/pay/123#abc", false).is_err()
    );
    assert!(pay_lmm::network::checkout_url("https://127.0.0.1/#abc", &allowed, false).is_err());
    assert!(
        pay_lmm::network::checkout_url(
            "https://checkout.stripe.com@evil.example/#abc",
            &allowed,
            false
        )
        .is_err()
    );
}

#[tokio::test]
async fn creem_requires_collected_money_not_only_a_list_price() {
    let h = harness("creem").await;
    let p = create(&h, "creem", "paid-evidence").await;
    let e = event("creem", h.upstream.captured.lock().await.clone(), 1);
    for (field, value) in [
        ("amount_paid", json!(0)),
        ("amount_paid", json!(1229)),
        ("amount_paid", json!(1231)),
        ("amount_paid", Value::Null),
        ("amount_due", json!(1231)),
        ("discount_amount", json!(1)),
        ("refunded_amount", json!(1)),
    ] {
        let mut bad = e.clone();
        bad["object"]["order"][field] = value;
        assert!(
            send(&h, "creem", &bad).await.is_client_error(),
            "accepted {field}"
        );
        assert_eq!(
            h.service.db.get("app", &p.id).await.unwrap().status,
            "pending"
        );
    }
    let mut old = e.clone();
    old["object"]["order"]
        .as_object_mut()
        .unwrap()
        .remove("amount_paid");
    assert!(send(&h, "creem", &old).await.is_client_error());
    assert_eq!(send(&h, "creem", &e).await, StatusCode::OK);
    assert_eq!(
        h.service.db.get("app", &p.id).await.unwrap().charged_minor,
        1230
    );
}

#[test]
fn documented_configuration_examples_validate_without_loading_secrets() {
    for source in [
        include_str!("../examples/config.toml"),
        include_str!("../examples/waffo.toml"),
        include_str!("../examples/gateways.toml"),
    ] {
        let c: Config = toml::from_str(source).unwrap();
        c.validate().unwrap();
    }
}
