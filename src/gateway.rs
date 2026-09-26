use std::{collections::BTreeMap, future::Future, pin::Pin};
use axum::http::HeaderMap;
use ring::rsa;
use serde::Deserialize;
use serde_json::json;
use zeroize::Zeroizing;
use crate::{config::{GatewayConfig, Protocol, WaffoConfig}, crypto, error::{Error, Result, identifier, require}, money, network::{self, SafeClient}, store::Record, wire};

type FutureResult<'a,T> = Pin<Box<dyn Future<Output=Result<T>> + Send + 'a>>;
pub struct Checkout { pub url: String, pub session_id: String }
#[derive(Clone, Debug)]
pub struct VerifiedEvent {
    pub id: String,
    pub fingerprint: String,
    pub payment_id: String,
    pub provider_order_id: String,
    pub provider_payment_id: String,
    pub currency: String,
    pub method: Option<String>,
    pub total_minor: i64,
    pub subtotal_minor: Option<i64>,
    pub charged_minor: i64,
}
// Adapters cannot write storage or grant business entitlements.
pub trait Adapter: Send + Sync {
    fn prepare(&self, input: &wire::CreatePaymentRequest) -> Result<String>;
    fn create<'a>(&'a self, record: &'a Record, public_url: &'a str, client: &'a SafeClient) -> FutureResult<'a,Checkout>;
    fn verify(&self, method: &str, headers: &HeaderMap, body: &[u8], query: &str) -> Result<Option<VerifiedEvent>>;
}
pub struct Gateway {
    pub config: GatewayConfig,
    pub identity: String,
    pub adapter: Box<dyn Adapter>,
}
impl Gateway {
    pub fn new(config: GatewayConfig, dev: bool, load: &impl Fn(&str)->anyhow::Result<Zeroizing<String>>) -> anyhow::Result<Self> {
        let identity = crypto::hash(&serde_json::to_vec(&config)?);
        let adapter: Box<dyn Adapter> = match config.protocol {
            Protocol::Epay => {
                let c = config.epay.as_ref().ok_or_else(||anyhow::anyhow!("epay config missing"))?;
                Box::new(Epay { config:config.clone(), pid:c.pid.clone(), key:load(&c.key_env)?, dev })
            }
            Protocol::WaffoPancake => {
                let c = config.waffo.clone().ok_or_else(||anyhow::anyhow!("waffo config missing"))?;
                let key = crypto::private_key(&load(&c.private_key_env)?)?;
                let public_key = crypto::public_key(&load(&c.public_key_env)?)?;
                Box::new(Waffo { config:config.clone(), options:c, key, public_key, dev })
            }
        };
        Ok(Self{config,identity,adapter})
    }
    pub fn capabilities(&self) -> wire::Gateway {
        wire::Gateway { id:self.config.id.clone(), protocol:self.config.protocol.as_str().into(), currencies:self.config.currencies.clone(), methods:self.config.methods.clone(), products:self.config.waffo.as_ref().map(|c|c.products.iter().map(|p|p.alias.clone()).collect()).unwrap_or_default(), checkout:true, signed_webhook:true, refunds:false, subscriptions:false, upstream_query:false }
    }
}
struct Epay { config: GatewayConfig, pid: String, key: Zeroizing<String>, dev: bool }
pub fn form(raw: &str) -> Result<BTreeMap<String,String>> {
    require(raw.len() <= 65536, "form_too_large")?;
    let bytes = raw.as_bytes();
    for (i,b) in bytes.iter().enumerate() {
        if *b == b'%' { require(i+2 < bytes.len() && bytes[i+1].is_ascii_hexdigit() && bytes[i+2].is_ascii_hexdigit(), "invalid_form_encoding")?; }
    }
    let mut values = BTreeMap::new();
    for (k,v) in url::form_urlencoded::parse(bytes) {
        require(identifier(&k) && v.len() <= 8192 && !v.contains('\u{fffd}') && !v.chars().any(char::is_control), "invalid_form_field")?;
        require(values.insert(k.into_owned(),v.into_owned()).is_none(), "duplicate_form_field")?;
        require(values.len() <= 32, "too_many_form_fields")?;
    }
    Ok(values)
}
fn field<'a>(v: &'a BTreeMap<String,String>, key: &str) -> Result<&'a str> {
    v.get(key).map(String::as_str).filter(|s|!s.is_empty()).ok_or(Error::invalid("missing_provider_field"))
}
impl Adapter for Epay {
    fn prepare(&self, input: &wire::CreatePaymentRequest) -> Result<String> {
        require(input.product.is_empty(), "epay_product_not_supported")?;
        Ok("total".into())
    }
    fn create<'a>(&'a self, r: &'a Record, public_url: &'a str, _: &'a SafeClient) -> FutureResult<'a,Checkout> {
        Box::pin(async move {
            let mut values = BTreeMap::from([
                ("pid".into(),self.pid.clone()), ("type".into(),r.input.method.clone()),
                ("out_trade_no".into(),r.payment.id.clone()),
                ("notify_url".into(),format!("{public_url}/hooks/{}",self.config.id)),
                ("return_url".into(),format!("{public_url}/return/{}",r.payment.id)),
                ("name".into(),r.input.description.clone()),
                ("money".into(),money::decimal(r.input.amount_minor,&r.input.currency)?),
                ("sign_type".into(),"MD5".into()),
            ]);
            values.insert("sign".into(),crypto::epay_signature(&values,&self.key));
            let base = network::safe_url(&format!("{}/",self.config.base_url.trim_end_matches('/')),self.dev)?;
            let mut url = base.join("submit.php").map_err(|_|Error::internal())?;
            url.query_pairs_mut().extend_pairs(values);
            Ok(Checkout{url:network::checkout_url(url.as_str(),&self.config.checkout_origins,self.dev)?,session_id:String::new()})
        })
    }
    fn verify(&self, method: &str, headers: &HeaderMap, body: &[u8], query: &str) -> Result<Option<VerifiedEvent>> {
        let values = if method == "GET" { form(query)? } else {
            require(method == "POST" && query.is_empty(), "invalid_webhook_method")?;
            require(headers.get("content-type").and_then(|v|v.to_str().ok()).unwrap_or("").split(';').next() == Some("application/x-www-form-urlencoded"), "form_content_type_required")?;
            form(std::str::from_utf8(body).map_err(|_|Error::invalid("invalid_utf8"))?)?
        };
        if field(&values,"pid")? != self.pid || !crypto::equal(field(&values,"sign")?.to_ascii_lowercase().as_bytes(),crypto::epay_signature(&values,&self.key).as_bytes()) {
            return Err(Error::unauthorized());
        }
        require(values.get("sign_type").is_none_or(|s|s == "MD5"), "unsupported_signature_type")?;
        if field(&values,"trade_status")? != "TRADE_SUCCESS" { return Ok(None); }
        let order = field(&values,"trade_no")?; let payment = field(&values,"out_trade_no")?;
        require(identifier(order) && identifier(payment), "invalid_provider_order_id")?;
        let amount = money::minor(field(&values,"money")?,"CNY")?;
        require(amount > 0, "invalid_amount")?;
        Ok(Some(VerifiedEvent { id:format!("{order}:TRADE_SUCCESS"), fingerprint:crypto::hash(&serde_json::to_vec(&values)?), payment_id:payment.into(), provider_order_id:order.into(), provider_payment_id:order.into(), currency:"CNY".into(), method:Some(field(&values,"type")?.into()), total_minor:amount, subtotal_minor:Some(amount), charged_minor:amount }))
    }
}
struct Waffo { config: GatewayConfig, options: WaffoConfig, key: rsa::KeyPair, public_key: Vec<u8>, dev: bool }
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct WaffoEnvelope { id:String, event_type:String, store_id:String, mode:String, data:serde_json::Value }
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct WaffoData {
    order_id:String,
    order_merchant_external_id:String,
    order_status:String,
    payment_status:String,
    payment_id:Option<String>,
    currency:String,
    charged_amount:Option<String>,
    amount:Option<String>,
    list_price:Option<PriceBreakdown>,
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase")]
struct PriceBreakdown { total:String, subtotal:Option<String>, tax_amount:Option<String> }
impl Adapter for Waffo {
    fn prepare(&self, input: &wire::CreatePaymentRequest) -> Result<String> {
        let p = self.options.products.iter().find(|p|p.alias == input.product).ok_or(Error::invalid("unknown_product"))?;
        Ok(p.amount_basis.as_str().into())
    }
    fn create<'a>(&'a self, r: &'a Record, public_url: &'a str, client: &'a SafeClient) -> FutureResult<'a,Checkout> {
        Box::pin(async move {
            let product = self.options.products.iter().find(|p|p.alias == r.input.product).ok_or(Error::invalid("unknown_product"))?;
            let path = "/v1/actions/checkout/create-session";
            let mut payload = json!({"productId":product.id,"currency":r.input.currency,"priceSnapshot":{"amount":money::decimal(r.input.amount_minor,&r.input.currency)?,"taxCategory":product.tax_category},"withTrial":false,"orderMerchantExternalId":r.payment.id,"successUrl":format!("{public_url}/return/{}",r.payment.id),"metadata":{"pay_lmm_payment_id":r.payment.id}});
            if r.input.method != "checkout" { payload["includePaymentMethods"] = json!([r.input.method]); }
            let body = serde_json::to_vec(&payload)?; let timestamp = crate::now().to_string();
            let signature = crypto::waffo_request_signature(&self.key,path,&timestamp,&body)?;
            let req = client.post(&format!("{}{path}",self.config.base_url.trim_end_matches('/')))?
                .header("Content-Type","application/json").header("X-Merchant-Id",&self.options.merchant_id)
                .header("X-Timestamp",timestamp).header("X-Signature",signature).header("X-Idempotency-Key",&r.payment.id).body(body);
            let (status,body) = client.execute(req).await?;
            if !status.is_success() { return Err(Error::upstream()); }
            let env: serde_json::Value = serde_json::from_slice(&body).map_err(|_|Error::upstream())?;
            if env.get("errors").is_some_and(|e|!e.is_null() && e.as_array().is_none_or(|a|!a.is_empty())) { return Err(Error::upstream()); }
            let data = env.get("data").ok_or(Error::upstream())?;
            let url = data.get("checkoutUrl").and_then(|s|s.as_str()).ok_or(Error::upstream())?;
            let session = data.get("sessionId").and_then(|s|s.as_str()).filter(|s|identifier(s)).ok_or(Error::upstream())?;
            Ok(Checkout{url:network::checkout_url(url,&self.config.checkout_origins,self.dev)?,session_id:session.into()})
        })
    }
    fn verify(&self, method: &str, headers: &HeaderMap, body: &[u8], query: &str) -> Result<Option<VerifiedEvent>> {
        require(method == "POST" && query.is_empty(), "invalid_webhook_method")?;
        require(headers.get("content-type").and_then(|v|v.to_str().ok()).unwrap_or("").split(';').next() == Some("application/json"), "json_content_type_required")?;
        let sig = headers.get("x-waffo-signature").and_then(|s|s.to_str().ok()).ok_or(Error::unauthorized())?;
        crypto::verify_waffo(&self.public_key,sig,body,crate::now()*1000)?;
        let envelope: WaffoEnvelope = serde_json::from_slice(body).map_err(|_|Error::invalid("invalid_webhook"))?;
        if envelope.mode != self.options.mode || envelope.store_id != self.options.store_id { return Err(Error::unauthorized()); }
        require(identifier(&envelope.id), "invalid_event_id")?;
        if envelope.event_type != "order.completed" { return Ok(None); }
        let data: WaffoData = serde_json::from_value(envelope.data).map_err(|_|Error::invalid("incomplete_payment_event"))?;
        require(data.order_status == "completed" && data.payment_status == "succeeded", "contradictory_payment_status")?;
        require(identifier(&data.order_id) && identifier(&data.order_merchant_external_id), "invalid_provider_order_id")?;
        let charged = money::minor(data.charged_amount.as_deref().or(data.amount.as_deref()).ok_or(Error::invalid("missing_charged_amount"))?,&data.currency)?;
        let total = match &data.list_price { Some(p)=>money::minor(&p.total,&data.currency)?,None=>charged };
        let subtotal = data.list_price.as_ref().and_then(|p|p.subtotal.as_ref()).map(|s|money::minor(s,&data.currency)).transpose()?;
        require(charged > 0 && charged == total, "charged_total_mismatch")?;
        if let (Some(subtotal),Some(tax)) = (subtotal,data.list_price.as_ref().and_then(|p|p.tax_amount.as_ref())) {
            require(subtotal.checked_add(money::minor(tax,&data.currency)?) == Some(total), "tax_total_mismatch")?;
        }
        let provider_payment_id = data.payment_id.unwrap_or_else(||data.order_id.clone());
        require(identifier(&provider_payment_id), "invalid_provider_payment_id")?;
        Ok(Some(VerifiedEvent{id:envelope.id,fingerprint:crypto::hash(body),payment_id:data.order_merchant_external_id,provider_order_id:data.order_id,provider_payment_id,currency:data.currency,method:None,total_minor:total,subtotal_minor:subtotal,charged_minor:charged}))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_form_parameters_are_rejected() {
        assert!(form("pid=1&pid=2").is_err());
        assert!(form("pid=%GG").is_err());
        assert!(form("pid=%").is_err());
        assert!(form("pid=%FF").is_err());
        assert_eq!(form("name=hello+world").unwrap()["name"],"hello world");
    }
}
