use std::{collections::BTreeMap, sync::Arc, time::Duration};
use axum::http::{HeaderMap, StatusCode};
use ring::hmac;
use tokio::sync::{Semaphore, watch};
use zeroize::Zeroizing;
use crate::{config::{Config, MerchantConfig, ServerConfig}, crypto, error::{Error, Result, identifier, require}, gateway::Gateway, money, network::SafeClient, now, store::{Database, Record}, wire::{CreatePaymentRequest, GatewayList, Payment}};

pub struct Merchant { pub config:MerchantConfig, api_hash:String, webhook_key:hmac::Key }
pub struct Service {
    pub server: ServerConfig,
    pub db: Database,
    pub gateways: BTreeMap<String,Gateway>,
    pub merchants: BTreeMap<String,Merchant>,
    pub permits: Arc<Semaphore>,
    pub client: SafeClient,
}
impl Service {
    pub fn new(config: Config) -> anyhow::Result<Arc<Self>> { Self::with_secrets(config,crypto::secret) }
    // Explicit dependency injection keeps tests away from process-global environment mutations.
    pub fn with_secrets(config: Config, load: impl Fn(&str)->anyhow::Result<Zeroizing<String>>) -> anyhow::Result<Arc<Self>> {
        config.validate()?;
        let mut merchants=BTreeMap::<String,Merchant>::new();
        for m in config.merchants {
            let api=load(&m.api_key_env)?; let webhook=load(&m.webhook_secret_env)?;
            anyhow::ensure!(api.len()>=32 && webhook.len()>=32, "merchant API and webhook secrets require at least 32 bytes");
            anyhow::ensure!(api.as_bytes()!=webhook.as_bytes(), "API and webhook keys must be different");
            let api_hash=crypto::hash(api.as_bytes());
            anyhow::ensure!(!merchants.values().any(|m|m.api_hash==api_hash), "each merchant must have a different API key");
            let webhook_key=hmac::Key::new(hmac::HMAC_SHA256,webhook.as_bytes());
            merchants.insert(m.id.clone(),Merchant{config:m,api_hash,webhook_key});
        }
        let mut gateways=BTreeMap::new();
        for g in config.gateways {
            let id=g.id.clone(); gateways.insert(id,Gateway::new(g,config.server.allow_loopback_http,&load)?);
        }
        let db=Database::open(&config.server.database,config.server.db_cache_kib)?;
        let client=SafeClient::new(config.server.allow_loopback_http,config.server.max_body_bytes)?;
        let permits=Arc::new(Semaphore::new(config.server.max_concurrency));
        Ok(Arc::new(Self{server:config.server,db,gateways,merchants,permits,client}))
    }
    pub fn authenticate(&self, headers: &HeaderMap) -> Result<&Merchant> {
        let value=headers.get("authorization").and_then(|v|v.to_str().ok()).and_then(|v|v.strip_prefix("Bearer ")).filter(|v|v.len()<=512).ok_or(Error::unauthorized())?;
        let hash=crypto::hash(value.as_bytes());
        self.merchants.values().find(|m|crypto::equal(m.api_hash.as_bytes(),hash.as_bytes())).ok_or(Error::unauthorized())
    }
    pub fn list_gateways(&self, merchant: &Merchant) -> GatewayList {
        GatewayList{gateways:merchant.config.gateways.iter().filter_map(|id|self.gateways.get(id).map(Gateway::capabilities)).collect()}
    }
    pub async fn create(&self, merchant: &Merchant, key: &str, input: CreatePaymentRequest) -> Result<(Payment,bool)> {
        require(identifier(key),"invalid_idempotency_key")?;
        require(identifier(&input.merchant_order_id),"invalid_merchant_order_id")?;
        require((1..=money::MAX_AMOUNT).contains(&input.amount_minor),"invalid_amount")?;
        money::exponent(&input.currency)?;
        require(identifier(&input.method),"invalid_method")?;
        require(input.gateway_id.is_empty() || identifier(&input.gateway_id),"invalid_gateway_id")?;
        require(input.product.is_empty() || identifier(&input.product),"invalid_product")?;
        require(!input.description.is_empty() && input.description.len()<=256 && !input.description.chars().any(char::is_control),"invalid_description")?;
        let request_hash=crypto::hash(&serde_json::to_vec(&input)?);
        if let Some(existing)=self.db.existing(&merchant.config.id,key,&request_hash).await? {return Ok((existing,false));}
        let gateway_id=if input.gateway_id.is_empty(){&merchant.config.default_gateway}else{&input.gateway_id};
        if !merchant.config.gateways.contains(gateway_id){return Err(Error::new(StatusCode::FORBIDDEN,"gateway_not_allowed"));}
        let gateway=self.gateways.get(gateway_id).ok_or(Error::invalid("unknown_gateway"))?;
        require(gateway.config.currencies.contains(&input.currency),"currency_not_supported_by_gateway")?;
        require(gateway.config.methods.contains(&input.method),"method_not_supported_by_gateway")?;
        let basis=gateway.adapter.prepare(&input)?;
        let payment=Payment{id:crypto::random_id("pay_")?,merchant_order_id:input.merchant_order_id.clone(),gateway_id:gateway_id.clone(),amount_minor:input.amount_minor,currency:input.currency.clone(),method:input.method.clone(),status:"creating".into(),created_at:now(),updated_at:now(),amount_basis:basis,notification_status:"not_scheduled".into(),..Default::default()};
        let r=Record{payment,input,merchant_id:merchant.config.id.clone(),idempotency_key:key.into(),request_hash,gateway_identity:gateway.identity.clone(),notify_url:merchant.config.webhook_url.clone(),session_id:String::new(),provider_payment_id:String::new()};
        let (r,new)=self.db.reserve(r).await?;
        if !new {return Ok((self.db.get(&merchant.config.id,&r.payment.id).await?,false));}
        let result=gateway.adapter.create(&r,self.server.public_url.trim_end_matches('/'),&self.client).await;
        if let Err(e)=&result {tracing::warn!(payment_id=%r.payment.id,code=e.code,"checkout outcome requires reconciliation; no automatic resubmission");}
        Ok((self.db.finish_create(&r.payment.id,result.ok()).await?,true))
    }
    pub async fn webhook(&self, id: &str, method: &str, headers: &HeaderMap, body: &[u8], query: &str) -> Result<&'static str> {
        let gateway=self.gateways.get(id).ok_or(Error::not_found())?;
        let event=gateway.adapter.verify(method,headers,body,query)?;
        if let Some(event)=event {self.db.accept(id,&gateway.identity,event).await?;}
        Ok(if gateway.config.protocol.as_str()=="epay" {"success"} else {"OK"})
    }
    pub async fn deliver_once(&self) -> Result<bool> {
        let Some(job)=self.db.claim().await? else {return Ok(false);};
        let result=async {
            let merchant=self.merchants.get(&job.merchant_id).ok_or(Error::internal())?;
            let signature=crypto::notification_signature(&merchant.webhook_key,now(),&job.payload);
            let request=self.client.post(&job.url)?.header("Content-Type","application/json").header("X-Pay-Event-Id",&job.id).header("X-Pay-Signature",signature).body(job.payload.clone());
            let (status,_)=self.client.execute(request).await?;
            if !status.is_success(){return Err(Error::upstream());}
            Ok::<(),Error>(())
        }.await;
        if result.is_err(){tracing::warn!(event_id=%job.id,attempt=job.attempt,"merchant notification deferred");}
        self.db.finish_delivery(&job.id,job.attempt,result.is_ok()).await?;
        Ok(true)
    }
    pub async fn worker(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        loop {
            if *shutdown.borrow(){break;}
            match self.deliver_once().await {
                Ok(true)=>continue,
                Ok(false)=>{},
                Err(e)=>tracing::error!(code=e.code,"outbox worker error"),
            }
            tokio::select! {
                _=tokio::time::sleep(Duration::from_secs(1))=>{},
                changed=shutdown.changed()=>{if changed.is_err(){break;}}
            }
        }
    }
}
