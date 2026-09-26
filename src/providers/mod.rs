//! Hosted-checkout adapters. No card collection, settlement or provider SDK runtime.
mod creem;
mod lemon;
mod stripe;

use crate::{config::{AmountBasis, GatewayConfig, Protocol}, crypto, error::{Error, Result, identifier, require}, gateway::{Adapter, VerifiedEvent}, money, network::{self, SafeClient}, store::Record};
use axum::http::HeaderMap;
use ring::hmac;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashSet;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode { Test, Prod }
impl Mode {
    pub fn live(self) -> bool { self == Self::Prod }
    pub fn as_str(self) -> &'static str { if self.live() { "prod" } else { "test" } }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StripeConfig {
    pub api_key_env: String,
    pub webhook_secret_env: String,
    pub mode: Mode,
    pub api_version: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreemConfig {
    pub api_key_env: String,
    pub webhook_secret_env: String,
    pub mode: Mode,
    pub products: Vec<CreemProduct>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreemProduct { pub alias: String, pub id: String, pub currency: String }
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LemonConfig {
    pub api_key_env: String,
    pub webhook_secret_env: String,
    pub mode: Mode,
    pub store_id: String,
    pub products: Vec<LemonVariant>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LemonVariant { pub alias: String, pub id: String, pub amount_basis: AmountBasis }

pub fn product_aliases(c: &GatewayConfig) -> Vec<String> {
    match c.protocol {
        Protocol::WaffoPancake => c.waffo.as_ref().map(|c|c.products.iter().map(|p|p.alias.clone()).collect()).unwrap_or_default(),
        Protocol::Creem => c.creem.as_ref().map(|c|c.products.iter().map(|p|p.alias.clone()).collect()).unwrap_or_default(),
        Protocol::LemonSqueezy => c.lemon_squeezy.as_ref().map(|c|c.products.iter().map(|p|p.alias.clone()).collect()).unwrap_or_default(),
        _ => Vec::new(),
    }
}
fn unique_products<'a>(products: impl IntoIterator<Item=(&'a str, &'a str)>) -> anyhow::Result<()> {
    let mut aliases=HashSet::new(); let mut ids=HashSet::new();
    for (alias,id) in products {
        anyhow::ensure!(identifier(alias) && identifier(id), "invalid product alias or id");
        anyhow::ensure!(aliases.insert(alias) && ids.insert(id), "duplicate product alias or upstream id");
    }
    anyhow::ensure!(!aliases.is_empty() && aliases.len()<=1000, "configure 1..1000 products");
    Ok(())
}
fn numeric_id(id: &str) -> bool { id.parse::<u64>().is_ok_and(|n|n>0 && n.to_string()==id) }
fn origin(g: &GatewayConfig, expected: &str, dev: bool) -> anyhow::Result<()> {
    let u=network::safe_url(&g.base_url,dev)?;
    let local=u.host_str().is_some_and(|h|h=="localhost" || h.trim_matches(['[',']']).parse::<std::net::IpAddr>().is_ok_and(|ip|ip.is_loopback()));
    anyhow::ensure!(u.path()=="/" && u.query().is_none(), "provider base URL must be an origin");
    anyhow::ensure!(u.origin().ascii_serialization()==expected || (dev && local), "provider endpoint does not match protocol/environment");
    Ok(())
}
pub fn validate_config(g: &GatewayConfig, dev: bool) -> anyhow::Result<()> {
    let count=[g.epay.is_some(),g.waffo.is_some(),g.stripe.is_some(),g.creem.is_some(),g.lemon_squeezy.is_some()].into_iter().filter(|x|*x).count();
    anyhow::ensure!(count==1, "exactly one protocol configuration block is required");
    match g.protocol {
        Protocol::Stripe => {
            let c=g.stripe.as_ref().ok_or_else(||anyhow::anyhow!("stripe configuration required"))?;
            origin(g,"https://api.stripe.com",dev)?;
            anyhow::ensure!(identifier(&c.api_version) && c.api_version.len()<=64, "explicit Stripe API version required");
            anyhow::ensure!(g.methods.iter().all(|s|["checkout","card","alipay"].contains(&s.as_str())), "unsupported Stripe method");
        }
        Protocol::Creem => {
            let c=g.creem.as_ref().ok_or_else(||anyhow::anyhow!("creem configuration required"))?;
            origin(g,if c.mode.live(){"https://api.creem.io"}else{"https://test-api.creem.io"},dev)?;
            anyhow::ensure!(g.methods==["checkout"], "Creem selects methods in its hosted checkout");
            unique_products(c.products.iter().map(|p|(p.alias.as_str(),p.id.as_str())))?;
            for p in &c.products { anyhow::ensure!(g.currencies.contains(&p.currency) && money::exponent(&p.currency)?==2, "Creem products require configured two-decimal currencies"); }
            anyhow::ensure!(g.currencies.iter().all(|currency|c.products.iter().any(|p|&p.currency==currency)), "currency without a Creem product");
        }
        Protocol::LemonSqueezy => {
            let c=g.lemon_squeezy.as_ref().ok_or_else(||anyhow::anyhow!("lemon_squeezy configuration required"))?;
            origin(g,"https://api.lemonsqueezy.com",dev)?;
            anyhow::ensure!(g.methods==["checkout"] && g.currencies.len()==1 && money::exponent(&g.currencies[0])?==2, "Lemon Squeezy requires checkout and one two-decimal store currency");
            anyhow::ensure!(numeric_id(&c.store_id), "Lemon Squeezy store id must be a positive integer string");
            unique_products(c.products.iter().map(|p|(p.alias.as_str(),p.id.as_str())))?;
            anyhow::ensure!(c.products.iter().all(|p|numeric_id(&p.id)), "variant ids must be positive integer strings");
        }
        _ => {}
    }
    Ok(())
}
pub fn build(g: &GatewayConfig, dev: bool, load: &impl Fn(&str)->anyhow::Result<Zeroizing<String>>) -> anyhow::Result<Box<dyn Adapter>> {
    match g.protocol {
        Protocol::Stripe => {
            let c=g.stripe.as_ref().ok_or_else(||anyhow::anyhow!("missing Stripe config"))?.clone();
            let ctx=Context::new(g,dev,c.mode,&c.api_key_env,&c.webhook_secret_env,load)?;
            let prefixes=if c.mode.live(){["sk_live_","rk_live_"]}else{["sk_test_","rk_test_"]};
            anyhow::ensure!(prefixes.iter().any(|p|ctx.api_key.starts_with(*p)), "Stripe key does not match configured mode");
            Ok(Box::new(stripe::Stripe{ctx,options:c}))
        }
        Protocol::Creem => {
            let c=g.creem.as_ref().ok_or_else(||anyhow::anyhow!("missing Creem config"))?.clone();
            let ctx=Context::new(g,dev,c.mode,&c.api_key_env,&c.webhook_secret_env,load)?;
            anyhow::ensure!(ctx.api_key.starts_with("creem_") && ctx.api_key.starts_with("creem_test_") != c.mode.live(), "Creem key does not match configured mode");
            Ok(Box::new(creem::Creem{ctx,options:c}))
        }
        Protocol::LemonSqueezy => {
            let c=g.lemon_squeezy.as_ref().ok_or_else(||anyhow::anyhow!("missing Lemon config"))?.clone();
            let ctx=Context::new(g,dev,c.mode,&c.api_key_env,&c.webhook_secret_env,load)?;
            Ok(Box::new(lemon::Lemon{ctx,options:c}))
        }
        _ => anyhow::bail!("not a hosted adapter"),
    }
}
struct Context { config:GatewayConfig, api_key:Zeroizing<String>, key:hmac::Key, mode:Mode, dev:bool }
impl Context {
    fn new(g:&GatewayConfig,dev:bool,mode:Mode,api:&str,secret:&str,load:&impl Fn(&str)->anyhow::Result<Zeroizing<String>>) -> anyhow::Result<Self> {
        let api_key=load(api)?; let secret=load(secret)?;
        anyhow::ensure!(!api_key.is_empty() && secret.len()>=16 && api_key.as_bytes()!=secret.as_bytes(), "separate API and strong webhook credentials required");
        Ok(Self{config:g.clone(),api_key,key:hmac::Key::new(hmac::HMAC_SHA256,secret.as_bytes()),mode,dev})
    }
    fn url(&self,path:&str)->String { format!("{}{path}",self.config.base_url.trim_end_matches('/')) }
    fn checkout(&self,url:&str)->Result<String> { network::checkout_url(url,&self.config.checkout_origins,self.dev) }
    fn tag(&self,id:&str,hash:&str)->String {
        // A provider signing an event does not make user-editable custom data trustworthy.
        hex::encode(hmac::sign(&self.key,format!("pay-lmm:checkout:v1\0{}\0{id}\0{hash}",self.config.id).as_bytes()).as_ref())
    }
    fn reference(&self,r:&Record)->Value {
        json!({"pay_lmm_payment_id":r.payment.id,"pay_lmm_request_hash":r.request_hash,"pay_lmm_binding":self.tag(&r.payment.id,&r.request_hash)})
    }
    fn reference_parts(&self,v:&Value)->Result<(String,String)> {
        let id=id_field(v,"pay_lmm_payment_id")?; let hash=text(v,"pay_lmm_request_hash")?;
        require(hash.len()==64 && hash.bytes().all(|b|b.is_ascii_hexdigit()),"invalid_checkout_reference")?;
        let tag=text(v,"pay_lmm_binding")?;
        if !crypto::equal(tag.as_bytes(),self.tag(&id,hash).as_bytes()){return Err(Error::unauthorized());}
        Ok((id,hash.into()))
    }
    fn hmac_body(&self,headers:&HeaderMap,name:&str,body:&[u8])->Result<()> {
        let signature=single_header(headers,name)?;
        let bytes=hex::decode(signature).map_err(|_|Error::unauthorized())?;
        hmac::verify(&self.key,body,&bytes).map_err(|_|Error::unauthorized())
    }
}
fn single_header<'a>(headers:&'a HeaderMap,name:&str)->Result<&'a str> {
    require(headers.get_all(name).iter().count()==1,"duplicate_or_missing_signature_header")?;
    headers.get(name).and_then(|v|v.to_str().ok()).filter(|v|v.len()<=4096).ok_or(Error::unauthorized())
}
fn json_webhook(method:&str,headers:&HeaderMap,query:&str)->Result<()> {
    require(method=="POST" && query.is_empty(),"invalid_webhook_method")?;
    let content=single_header(headers,"content-type")?.split(';').next().unwrap_or("").trim();
    require(["application/json","application/vnd.api+json"].contains(&content),"json_content_type_required")
}
fn text<'a>(v:&'a Value,key:&str)->Result<&'a str> { v.get(key).and_then(Value::as_str).filter(|s|!s.is_empty()).ok_or(Error::invalid("missing_provider_field")) }
fn id_field(v:&Value,key:&str)->Result<String> {
    let s=text(v,key)?; require(identifier(s),"invalid_provider_id")?; Ok(s.into())
}
fn object_id(v:&Value)->Result<String> {
    let id=if let Some(s)=v.as_str(){s.to_owned()}else if let Some(n)=v.as_u64(){n.to_string()}else{return id_field(v,"id");};
    require(identifier(&id),"invalid_provider_id")?; Ok(id)
}
fn number(v:&Value,key:&str)->Result<i64> {
    let n=v.get(key).and_then(Value::as_i64).ok_or(Error::invalid("missing_amount_evidence"))?;
    require((0..=money::MAX_AMOUNT).contains(&n),"invalid_amount")?; Ok(n)
}
fn flag(v:&Value,key:&str)->Result<bool> { v.get(key).and_then(Value::as_bool).ok_or(Error::invalid("missing_provider_flag")) }
fn fingerprint(v:&Value)->Result<String> { Ok(crypto::hash(&serde_json::to_vec(v)?)) }
async fn json_response(client:&SafeClient,req:reqwest::RequestBuilder)->Result<Value> {
    let (status,body)=client.execute(req).await?;
    if !status.is_success(){return Err(Error::upstream());}
    let value:Value=serde_json::from_slice(&body).map_err(|_|Error::upstream())?;
    if value.get("error").is_some_and(|v|!v.is_null()) || value.get("errors").is_some_and(|v|!v.is_null() && v.as_array().is_none_or(|v|!v.is_empty())) {return Err(Error::upstream());}
    Ok(value)
}
