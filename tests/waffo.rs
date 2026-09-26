use std::{process::Command, sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}}};
use axum::{Router, http::{HeaderMap, StatusCode}, response::IntoResponse, routing::post};
use base64::{Engine, engine::general_purpose::STANDARD};
use pay_lmm::{config::Config, crypto, service::Service, wire::CreatePaymentRequest};
use ring::{digest, signature};
use serde_json::{Value, json};
use zeroize::Zeroizing;

fn generate_keys(dir: &std::path::Path) -> (String,String) {
    let private=dir.join("private.pem");let public=dir.join("public.pem");
    assert!(Command::new("openssl").args(["genpkey","-algorithm","RSA","-pkeyopt","rsa_keygen_bits:2048","-out"]).arg(&private).output().expect("OpenSSL is required for interoperability tests").status.success());
    assert!(Command::new("openssl").args(["pkey","-in"]).arg(&private).args(["-pubout","-out"]).arg(&public).output().unwrap().status.success());
    (std::fs::read_to_string(private).unwrap(),std::fs::read_to_string(public).unwrap())
}
fn signed(event: &Value, key: &ring::rsa::KeyPair, timestamp: i64) -> (HeaderMap,Vec<u8>) {
    let body=serde_json::to_vec(event).unwrap();
    let mut input=timestamp.to_string().into_bytes();input.push(b'.');input.extend_from_slice(&body);
    let signature=crypto::rsa_sign(key,&input).unwrap();
    let mut headers=HeaderMap::new();headers.insert("content-type","application/json".parse().unwrap());
    headers.insert("x-waffo-signature",format!("t={timestamp},v1={signature}").parse().unwrap());
    (headers,body)
}
#[tokio::test]
async fn real_rsa_interop_checkout_environment_amounts_and_unknown_outcomes() {
    let dir=tempfile::tempdir().unwrap();let (private,public)=generate_keys(dir.path());
    let key=crypto::private_key(&private).unwrap();let public_der=Arc::new(crypto::public_key(&public).unwrap());
    // Independent OpenSSL output must equal the SDK-compatible request signature.
    let payload=b"{\"currency\":\"USD\"}";
    let hash=STANDARD.encode(digest::digest(&digest::SHA256,payload).as_ref());
    let canonical=format!("POST\n/v1/actions/checkout/create-session\n123456\n{hash}");
    std::fs::write(dir.path().join("canonical"),canonical).unwrap();
    let signed_file=dir.path().join("signature.bin");
    assert!(Command::new("openssl").args(["dgst","-sha256","-sign"]).arg(dir.path().join("private.pem")).arg("-out").arg(&signed_file).arg(dir.path().join("canonical")).output().unwrap().status.success());
    assert_eq!(crypto::waffo_request_signature(&key,"/v1/actions/checkout/create-session","123456",payload).unwrap(),STANDARD.encode(std::fs::read(signed_file).unwrap()));

    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin=format!("http://{}",listener.local_addr().unwrap());
    let calls=Arc::new(AtomicUsize::new(0));let reject=Arc::new(AtomicBool::new(false));
    let (counter,fail,verify_key,checkout_origin)=(calls.clone(),reject.clone(),public_der.clone(),origin.clone());
    let upstream=Router::new().route("/v1/actions/checkout/create-session",post(move |headers:HeaderMap,body:axum::body::Bytes|{
        let (counter,fail,key,origin)=(counter.clone(),fail.clone(),verify_key.clone(),checkout_origin.clone());
        async move {
            counter.fetch_add(1,Ordering::SeqCst);
            assert_eq!(headers["x-merchant-id"],"MER_TEST");
            let timestamp=headers["x-timestamp"].to_str().unwrap();
            let body_hash=STANDARD.encode(digest::digest(&digest::SHA256,&body).as_ref());
            let canonical=format!("POST\n/v1/actions/checkout/create-session\n{timestamp}\n{body_hash}");
            let signature=STANDARD.decode(headers["x-signature"].as_bytes()).unwrap();
            signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256,key.as_slice()).verify(canonical.as_bytes(),&signature).unwrap();
            let request:Value=serde_json::from_slice(&body).unwrap();
            assert_eq!(request["productId"],"PROD_TEST");assert_eq!(request["priceSnapshot"]["amount"],"12.30");
            assert_eq!(request["withTrial"],false);assert_eq!(request["orderMerchantExternalId"].as_str().unwrap(),headers["x-idempotency-key"].to_str().unwrap());
            if fail.load(Ordering::SeqCst) {return (StatusCode::BAD_GATEWAY,axum::Json(json!({"errors":[{"message":"upstream uncertainty"}]}))).into_response();}
            axum::Json(json!({"data":{"sessionId":"SESSION_TEST","checkoutUrl":format!("{origin}/checkout/SESSION_TEST")}})).into_response()
        }
    }));
    let server=tokio::spawn(async move {axum::serve(listener,upstream).await.unwrap();});
    let config:Config=toml::from_str(&format!(r#"
[server]
database=":memory:"
allow_loopback_http=true
[[merchants]]
id="test-app"
api_key_env="API"
webhook_secret_env="WEBHOOK"
webhook_url="https://merchant.example/notify"
default_gateway="pancake-test"
gateways=["pancake-test"]
[[gateways]]
id="pancake-test"
protocol="waffo_pancake"
base_url="{origin}"
checkout_origins=["{origin}"]
currencies=["USD"]
methods=["checkout"]
[gateways.waffo]
merchant_id="MER_TEST"
store_id="STO_TEST"
mode="test"
private_key_env="PRIVATE"
public_key_env="PUBLIC"
[[gateways.waffo.products]]
alias="credits"
id="PROD_TEST"
tax_category="digital_goods"
amount_basis="total"
"#)).unwrap();
    let service=Service::with_secrets(config,|name|Ok(Zeroizing::new(match name {"API"=>"a".repeat(40),"WEBHOOK"=>"c".repeat(40),"PRIVATE"=>private.clone(),"PUBLIC"=>public.clone(),_=>panic!("unexpected secret")}))).unwrap();
    let request=CreatePaymentRequest{merchant_order_id:"business-1".into(),amount_minor:1230,currency:"USD".into(),method:"checkout".into(),product:"credits".into(),description:"Credits".into(),..Default::default()};
    let (p,created)=service.create(&service.merchants["test-app"],"idem-1",request.clone()).await.unwrap();assert!(created);assert_eq!(p.status,"pending");
    service.create(&service.merchants["test-app"],"idem-1",request.clone()).await.unwrap();assert_eq!(calls.load(Ordering::SeqCst),1);
    let event=json!({"id":"EV_TEST","eventType":"order.completed","storeId":"STO_TEST","mode":"test","data":{"orderId":"ORD_TEST","orderMerchantExternalId":p.id,"orderStatus":"completed","paymentStatus":"succeeded","paymentId":"PAYMENT_TEST","currency":"USD","chargedAmount":"12.30","listPrice":{"total":"12.30","subtotal":"12.00","taxAmount":"0.30"}}});
    for bad in ["mode","store","amount","state","tax"] {
        let mut e=event.clone();
        match bad {"mode"=>e["mode"]=json!("prod"),"store"=>e["storeId"]=json!("STO_OTHER"),"amount"=>e["data"]["chargedAmount"]=json!("12.29"),"state"=>e["data"]["paymentStatus"]=json!("failed"),"tax"=>e["data"]["listPrice"]["taxAmount"]=json!("0.31"),_=>unreachable!()}
        let (headers,body)=signed(&e,&key,pay_lmm::now()*1000);
        assert!(service.webhook("pancake-test","POST",&headers,&body,"").await.is_err(),"{bad}");
    }
    let (headers,body)=signed(&event,&key,(pay_lmm::now()-3600)*1000);
    assert!(service.webhook("pancake-test","POST",&headers,&body,"").await.is_err());
    let mut lifecycle=event.clone();lifecycle["eventType"]=json!("subscription.activated");
    let (headers,body)=signed(&lifecycle,&key,pay_lmm::now()*1000);
    service.webhook("pancake-test","POST",&headers,&body,"").await.unwrap();
    assert_eq!(service.db.get("test-app",&p.id).await.unwrap().status,"pending");
    let (headers,body)=signed(&event,&key,pay_lmm::now()*1000);
    let mut tampered=body.clone();tampered.push(b' ');
    assert!(service.webhook("pancake-test","POST",&headers,&tampered,"").await.is_err());
    service.webhook("pancake-test","POST",&headers,&body,"").await.unwrap();
    service.webhook("pancake-test","POST",&headers,&body,"").await.unwrap();
    assert_eq!(service.db.get("test-app",&p.id).await.unwrap().status,"succeeded");
    reject.store(true,Ordering::SeqCst);
    let mut next=request;next.merchant_order_id="business-2".into();
    let (unknown,_)=service.create(&service.merchants["test-app"],"idem-2",next.clone()).await.unwrap();assert_eq!(unknown.status,"unknown");
    let (repeated,_)=service.create(&service.merchants["test-app"],"idem-2",next).await.unwrap();assert_eq!(repeated.id,unknown.id);assert_eq!(calls.load(Ordering::SeqCst),2);
    let mut late=event;late["id"]=json!("EV_LATE");late["data"]["orderId"]=json!("ORD_LATE");late["data"]["paymentId"]=json!("PAY_LATE");late["data"]["orderMerchantExternalId"]=json!(unknown.id);
    let (headers,body)=signed(&late,&key,pay_lmm::now()*1000);
    service.webhook("pancake-test","POST",&headers,&body,"").await.unwrap();
    assert_eq!(service.db.get("test-app",&unknown.id).await.unwrap().status,"succeeded");
    server.abort();
}
