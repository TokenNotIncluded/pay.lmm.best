use std::{sync::Arc, time::Duration};
use axum::{Router, body::{Body, Bytes, to_bytes}, extract::{Path, Request, State}, http::{HeaderMap, Method, StatusCode, Uri, header}, middleware::{self, Next}, response::{Html, IntoResponse, Response}, routing::{get, post}};
use prost::Message;
use serde::Serialize;
use crate::{error::{Error, Result}, service::Service, wire::{CreatePaymentRequest, ErrorResponse}};

#[derive(Clone, Copy)]
enum Encoding { Json, Protobuf }
impl Encoding {
    fn response(headers: &HeaderMap) -> Self {
        let accept=headers.get(header::ACCEPT).and_then(|v|v.to_str().ok()).unwrap_or("");
        if accept.split(',').any(|v|matches!(v.trim().split(';').next(),Some("application/x-protobuf"|"application/protobuf"))) {Self::Protobuf} else {Self::Json}
    }
    fn decode(self, headers: &HeaderMap, body: &[u8]) -> Result<CreatePaymentRequest> {
        let _=self;
        let content_type=headers.get(header::CONTENT_TYPE).and_then(|v|v.to_str().ok()).unwrap_or("").split(';').next().unwrap_or("").trim();
        match content_type {
            "application/json"=>serde_json::from_slice(body).map_err(|_|Error::new(StatusCode::BAD_REQUEST,"invalid_json")),
            "application/x-protobuf"|"application/protobuf"=>CreatePaymentRequest::decode(body).map_err(|_|Error::new(StatusCode::BAD_REQUEST,"invalid_protobuf")),
            _=>Err(Error::new(StatusCode::UNSUPPORTED_MEDIA_TYPE,"unsupported_content_type")),
        }
    }
}
fn response<T:Serialize+Message>(encoding: Encoding, status: StatusCode, value: &T) -> Response {
    match encoding {
        Encoding::Json=>match serde_json::to_vec(value) {
            Ok(body)=>(status,[(header::CONTENT_TYPE,"application/json")],body).into_response(),
            Err(_)=>(StatusCode::INTERNAL_SERVER_ERROR,"internal_error").into_response(),
        },
        Encoding::Protobuf=>(status,[(header::CONTENT_TYPE,"application/x-protobuf")],value.encode_to_vec()).into_response(),
    }
}
fn error(encoding: Encoding, e: Error) -> Response {response(encoding,e.status,&ErrorResponse{code:e.code.into()})}
async fn guard(State(service): State<Arc<Service>>, request: Request, next: Next) -> Response {
    let encoding=Encoding::response(request.headers());
    let Ok(_permit)=service.permits.clone().try_acquire_owned() else {return error(encoding,Error::new(StatusCode::SERVICE_UNAVAILABLE,"busy"));};
    if request.uri().to_string().len()>16384 || request.headers().len()>64 {return error(encoding,Error::new(StatusCode::REQUEST_HEADER_FIELDS_TOO_LARGE,"request_headers_too_large"));}
    for name in ["authorization","idempotency-key","x-waffo-signature","content-type"] {
        if request.headers().get_all(name).iter().count()>1 {return error(encoding,Error::new(StatusCode::BAD_REQUEST,"duplicate_header"));}
    }
    if request.headers().get(header::CONTENT_ENCODING).is_some_and(|v|v!="identity") {return error(encoding,Error::new(StatusCode::UNSUPPORTED_MEDIA_TYPE,"content_encoding_not_supported"));}
    let (parts,body)=request.into_parts();
    let bytes=match tokio::time::timeout(Duration::from_secs(10),to_bytes(body,service.server.max_body_bytes)).await {
        Ok(Ok(bytes))=>bytes,
        Ok(Err(_))=>return error(encoding,Error::new(StatusCode::PAYLOAD_TOO_LARGE,"body_too_large")),
        Err(_)=>return error(encoding,Error::new(StatusCode::REQUEST_TIMEOUT,"body_timeout")),
    };
    let mut result=next.run(Request::from_parts(parts,Body::from(bytes))).await;
    let h=result.headers_mut();
    h.insert(header::CACHE_CONTROL,header::HeaderValue::from_static("no-store"));
    h.insert("x-content-type-options",header::HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy",header::HeaderValue::from_static("no-referrer"));
    h.insert("content-security-policy",header::HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'"));
    result
}
pub fn router(service: Arc<Service>) -> Router {
    Router::new()
        .route("/",get(||async {Html(include_str!("../web/index.html"))}))
        .route("/healthz",get(||async {axum::Json(serde_json::json!({"status":"ok"}))}))
        .route("/readyz",get(ready))
        .route("/proto/pay/v1/pay.proto",get(||async {([(header::CONTENT_TYPE,"text/plain; charset=utf-8")],include_str!("../proto/pay/v1/pay.proto"))}))
        .route("/v1/gateways",get(gateways))
        .route("/v1/payments",post(create))
        .route("/v1/payments/{id}",get(payment))
        .route("/v1/payments/{id}/notifications/retry",post(retry))
        .route("/hooks/{id}",get(webhook).post(webhook))
        .route("/return/{id}",get(||async {Html("<!doctype html><html lang=\"zh-CN\"><meta charset=\"utf-8\"><title>Payment status</title><h1>请回到商户页面查看订单</h1><p>此跳转不代表付款成功。商户须通过已验证的服务端通知或订单查询确认结果。</p></html>")}))
        .fallback(|headers:HeaderMap|async move {error(Encoding::response(&headers),Error::not_found())})
        .layer(middleware::from_fn_with_state(service.clone(),guard))
        .with_state(service)
}
async fn ready(State(service):State<Arc<Service>>, headers:HeaderMap) -> Response {
    match service.db.call(|c|{c.query_row("SELECT 1",[],|r|r.get::<_,i32>(0))?;Ok(())}).await {
        Ok(())=>axum::Json(serde_json::json!({"status":"ready"})).into_response(),
        Err(e)=>error(Encoding::response(&headers),e),
    }
}
async fn gateways(State(service):State<Arc<Service>>, headers:HeaderMap) -> Response {
    let encoding=Encoding::response(&headers);
    match service.authenticate(&headers) {
        Ok(m)=>response(encoding,StatusCode::OK,&service.list_gateways(m)),
        Err(e)=>error(encoding,e),
    }
}
async fn create(State(service):State<Arc<Service>>, headers:HeaderMap, body:Bytes) -> Response {
    let encoding=Encoding::response(&headers);
    let result=async {
        let m=service.authenticate(&headers)?;
        let key=headers.get("idempotency-key").and_then(|v|v.to_str().ok()).ok_or(Error::new(StatusCode::BAD_REQUEST,"idempotency_key_required"))?;
        let input=encoding.decode(&headers,&body)?;
        service.create(m,key,input).await
    }.await;
    match result {
        Ok((p,new))=>{
            let status=if p.status=="unknown" || p.status=="creating" {StatusCode::ACCEPTED} else if new {StatusCode::CREATED} else {StatusCode::OK};
            response(encoding,status,&p)
        }
        Err(e)=>error(encoding,e),
    }
}
async fn payment(State(service):State<Arc<Service>>, Path(id):Path<String>, headers:HeaderMap) -> Response {
    let encoding=Encoding::response(&headers);
    let result=async {let m=service.authenticate(&headers)?; service.db.get(&m.config.id,&id).await}.await;
    match result {Ok(p)=>response(encoding,StatusCode::OK,&p),Err(e)=>error(encoding,e)}
}
async fn retry(State(service):State<Arc<Service>>, Path(id):Path<String>, headers:HeaderMap) -> Response {
    let encoding=Encoding::response(&headers);
    let result=async {let m=service.authenticate(&headers)?; service.db.retry_dead(&m.config.id,&id).await}.await;
    match result {Ok(p)=>response(encoding,StatusCode::OK,&p),Err(e)=>error(encoding,e)}
}
async fn webhook(State(service):State<Arc<Service>>, Path(id):Path<String>, method:Method, uri:Uri, headers:HeaderMap, body:Bytes) -> Response {
    match service.webhook(&id,method.as_str(),&headers,&body,uri.query().unwrap_or("")).await {
        Ok(ack)=>(StatusCode::OK,[(header::CONTENT_TYPE,"text/plain")],ack).into_response(),
        Err(e)=>error(Encoding::Json,e),
    }
}
