use std::{collections::BTreeMap, sync::Arc};
use axum::{Router, body::{Body, to_bytes}, http::{Request, StatusCode}, routing::post};
use pay_lmm::{api, config::Config, crypto, service::Service, wire::{CreatePaymentRequest, Payment}};
use prost::Message;
use tower::ServiceExt;
use zeroize::Zeroizing;

fn service(notify: &str) -> Arc<Service> {
    let config:Config=toml::from_str(&format!(r#"
[server]
database=":memory:"
allow_loopback_http=true
[[merchants]]
id="app"
api_key_env="API"
webhook_secret_env="WEBHOOK"
webhook_url="{notify}"
default_gateway="epay"
gateways=["epay"]
[[merchants]]
id="other"
api_key_env="OTHER"
webhook_secret_env="OTHER_WEBHOOK"
webhook_url="{notify}"
default_gateway="epay"
gateways=["epay"]
[[gateways]]
id="epay"
protocol="epay"
base_url="https://epay.example.com"
checkout_origins=["https://epay.example.com"]
currencies=["CNY"]
methods=["alipay","wxpay"]
[gateways.epay]
pid="1000"
key_env="EPAY"
"#)).unwrap();
    Service::with_secrets(config,|name|Ok(Zeroizing::new(match name {"API"=>"a".repeat(40),"OTHER"=>"b".repeat(40),"WEBHOOK"=>"c".repeat(40),"OTHER_WEBHOOK"=>"d".repeat(40),"EPAY"=>"epay-test-key".into(),_=>panic!("unexpected secret")}))).unwrap()
}
fn input() -> CreatePaymentRequest {
    CreatePaymentRequest{merchant_order_id:"order-1".into(),amount_minor:1230,currency:"CNY".into(),method:"alipay".into(),description:"Test order".into(),..Default::default()}
}
fn create_request(input: &CreatePaymentRequest, key: &str, protobuf: bool) -> Request<Body> {
    let content=if protobuf {"application/x-protobuf"} else {"application/json"};
    let body=if protobuf {input.encode_to_vec()} else {serde_json::to_vec(input).unwrap()};
    Request::builder().method("POST").uri("/v1/payments").header("Authorization",format!("Bearer {}","a".repeat(40))).header("Idempotency-Key",key).header("Content-Type",content).header("Accept",content).body(Body::from(body)).unwrap()
}
fn callback(p: &Payment, amount: &str, key: &str) -> String {
    let mut values=BTreeMap::from([("pid".into(),"1000".into()),("out_trade_no".into(),p.id.clone()),("trade_no".into(),"TRADE_001".into()),("type".into(),"alipay".into()),("money".into(),amount.into()),("trade_status".into(),"TRADE_SUCCESS".into()),("sign_type".into(),"MD5".into())]);
    values.insert("sign".into(),crypto::epay_signature(&values,key));
    url::form_urlencoded::Serializer::new(String::new()).extend_pairs(values).finish()
}
#[tokio::test]
async fn json_protobuf_idempotency_tenant_isolation_and_verified_delivery() {
    let delivered=Arc::new(tokio::sync::Mutex::new(0usize));
    let counter=delivered.clone();
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let notify=format!("http://{}/notify",listener.local_addr().unwrap());
    let receiver=Router::new().route("/notify",post(move |headers:axum::http::HeaderMap,body:axum::body::Bytes|{
        let count=counter.clone();async move {
            let header=headers["x-pay-signature"].to_str().unwrap();
            let parts:Vec<_>=header.split(',').collect();
            let timestamp=parts[0].strip_prefix("t=").unwrap();
            let signature=hex::decode(parts[1].strip_prefix("v1=").unwrap()).unwrap();
            let mut signed=timestamp.as_bytes().to_vec();signed.push(b'.');signed.extend_from_slice(&body);
            let key=ring::hmac::Key::new(ring::hmac::HMAC_SHA256,"c".repeat(40).as_bytes());
            ring::hmac::verify(&key,&signed,&signature).unwrap();
            let event:serde_json::Value=serde_json::from_slice(&body).unwrap();
            assert_eq!(event["event_type"],"payment.succeeded");
            *count.lock().await+=1;
            StatusCode::NO_CONTENT
        }
    }));
    let server=tokio::spawn(async move {axum::serve(listener,receiver).await.unwrap();});
    let service=service(&notify);let app=api::router(service.clone());
    let response=app.clone().oneshot(create_request(&input(),"idem-1",false)).await.unwrap();
    assert_eq!(response.status(),StatusCode::CREATED);
    let p:Payment=serde_json::from_slice(&to_bytes(response.into_body(),65536).await.unwrap()).unwrap();
    assert_eq!(p.status,"pending");assert!(p.checkout_url.starts_with("https://epay.example.com/submit.php?"));
    let repeated=app.clone().oneshot(create_request(&input(),"idem-1",true)).await.unwrap();
    assert_eq!(repeated.status(),StatusCode::OK);
    assert_eq!(repeated.headers()["content-type"],"application/x-protobuf");
    let p2=Payment::decode(to_bytes(repeated.into_body(),65536).await.unwrap()).unwrap();assert_eq!(p.id,p2.id);
    let mut changed=input();changed.amount_minor+=1;
    assert_eq!(app.clone().oneshot(create_request(&changed,"idem-1",false)).await.unwrap().status(),StatusCode::CONFLICT);
    assert_eq!(app.clone().oneshot(create_request(&input(),"another-key",false)).await.unwrap().status(),StatusCode::CONFLICT);
    let other=Request::builder().uri(format!("/v1/payments/{}",p.id)).header("Authorization",format!("Bearer {}","b".repeat(40))).body(Body::empty()).unwrap();
    assert_eq!(app.clone().oneshot(other).await.unwrap().status(),StatusCode::NOT_FOUND);
    let return_page=Request::builder().uri(format!("/return/{}",p.id)).body(Body::empty()).unwrap();
    assert_eq!(app.clone().oneshot(return_page).await.unwrap().status(),StatusCode::OK);
    assert_eq!(service.db.get("app",&p.id).await.unwrap().status,"pending");
    for (amount,key,status) in [("12.30","wrong",StatusCode::UNAUTHORIZED),("12.31","epay-test-key",StatusCode::UNPROCESSABLE_ENTITY),("12.30","epay-test-key",StatusCode::OK),("12.30","epay-test-key",StatusCode::OK)] {
        let req=Request::builder().uri(format!("/hooks/epay?{}",callback(&p,amount,key))).body(Body::empty()).unwrap();
        assert_eq!(app.clone().oneshot(req).await.unwrap().status(),status);
    }
    assert_eq!(service.db.get("app",&p.id).await.unwrap().status,"succeeded");
    assert!(service.deliver_once().await.unwrap());
    assert!(!service.deliver_once().await.unwrap());
    assert_eq!(*delivered.lock().await,1);
    assert_eq!(service.db.get("app",&p.id).await.unwrap().notification_status,"delivered");
    let receipts=service.db.call(|c|Ok(c.query_row("SELECT COUNT(*) FROM receipts",[],|r|r.get::<_,i64>(0))?)).await.unwrap();assert_eq!(receipts,1);
    server.abort();
}
#[tokio::test]
async fn concurrent_requests_have_one_order() {
    let service=service("https://merchant.example/notify");let mut tasks=Vec::new();
    for _ in 0..16 {
        let service=service.clone();
        tasks.push(tokio::spawn(async move {service.create(&service.merchants["app"],"concurrent",input()).await.unwrap()}));
    }
    let mut ids=std::collections::HashSet::new();let mut new=0;
    for task in tasks {let(p,created)=task.await.unwrap();ids.insert(p.id);new+=usize::from(created);}
    assert_eq!(ids.len(),1);assert_eq!(new,1);
}
#[tokio::test]
async fn bounded_and_strict_inputs() {
    let service=service("https://merchant.example/notify");let app=api::router(service);
    let bad=Request::builder().method("POST").uri("/v1/payments").header("Authorization",format!("Bearer {}","a".repeat(40))).header("Idempotency-Key","test").header("Content-Type","application/json").body(Body::from(r#"{"merchant_order_id":"x","amount_minor":1.1,"card_number":"not allowed"}"#)).unwrap();
    assert_eq!(app.clone().oneshot(bad).await.unwrap().status(),StatusCode::BAD_REQUEST);
    let big=Request::builder().method("POST").uri("/v1/payments").body(Body::from(vec![b'x';65537])).unwrap();
    assert_eq!(app.clone().oneshot(big).await.unwrap().status(),StatusCode::PAYLOAD_TOO_LARGE);
    let noauth=Request::builder().uri("/v1/gateways").body(Body::empty()).unwrap();
    assert_eq!(app.clone().oneshot(noauth).await.unwrap().status(),StatusCode::UNAUTHORIZED);
    let mut unsupported=input();unsupported.gateway_id="other-gateway".into();
    assert_eq!(app.oneshot(create_request(&unsupported,"forbidden",false)).await.unwrap().status(),StatusCode::FORBIDDEN);
}
