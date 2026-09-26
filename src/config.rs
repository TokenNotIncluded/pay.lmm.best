use std::{collections::HashSet, path::Path};
use serde::{Deserialize, Serialize};
use crate::{error::identifier, money, network};

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    pub merchants: Vec<MerchantConfig>,
    pub gateways: Vec<GatewayConfig>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub listen: String,
    pub public_url: String,
    pub database: String,
    pub max_concurrency: usize,
    pub max_body_bytes: usize,
    pub db_cache_kib: u32,
    pub allow_loopback_http: bool,
}
impl Default for ServerConfig {
    fn default() -> Self {
        Self { listen: "127.0.0.1:8080".into(), public_url: "https://pay.lmm.best".into(), database: "data/pay.sqlite3".into(), max_concurrency: 32, max_body_bytes: 65536, db_cache_kib: 2048, allow_loopback_http: false }
    }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MerchantConfig {
    pub id: String,
    pub api_key_env: String,
    pub webhook_secret_env: String,
    pub webhook_url: String,
    pub default_gateway: String,
    pub gateways: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    pub id: String,
    pub protocol: Protocol,
    pub base_url: String,
    pub checkout_origins: Vec<String>,
    pub currencies: Vec<String>,
    pub methods: Vec<String>,
    pub epay: Option<EpayConfig>,
    pub waffo: Option<WaffoConfig>,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Protocol { Epay, WaffoPancake }
impl Protocol {
    pub fn as_str(self) -> &'static str { match self { Self::Epay => "epay", Self::WaffoPancake => "waffo_pancake" } }
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EpayConfig { pub pid: String, pub key_env: String }
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WaffoConfig {
    pub merchant_id: String,
    pub store_id: String,
    pub mode: String,
    pub private_key_env: String,
    pub public_key_env: String,
    pub products: Vec<ProductConfig>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProductConfig {
    pub alias: String,
    pub id: String,
    pub tax_category: String,
    pub amount_basis: AmountBasis,
}
#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AmountBasis { Total, Subtotal }
impl AmountBasis {
    pub fn as_str(self) -> &'static str { match self { Self::Total => "total", Self::Subtotal => "subtotal" } }
}
fn unique(items: impl IntoIterator<Item = String>) -> bool {
    let mut seen = HashSet::new(); items.into_iter().all(|s| seen.insert(s))
}
impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = std::fs::read(path)?;
        anyhow::ensure!(bytes.len() <= 1024 * 1024, "configuration is too large");
        let config: Self = toml::from_str(std::str::from_utf8(&bytes)?)?;
        config.validate()?;
        Ok(config)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        let dev = self.server.allow_loopback_http;
        let public = network::safe_url(&self.server.public_url, dev)?;
        anyhow::ensure!(public.path() == "/" && public.query().is_none(), "server.public_url must be an origin without a path or query");
        self.server.listen.parse::<std::net::SocketAddr>()?;
        anyhow::ensure!((1..=256).contains(&self.server.max_concurrency), "max_concurrency must be 1..256");
        anyhow::ensure!((1024..=262144).contains(&self.server.max_body_bytes), "max_body_bytes must be 1024..262144");
        anyhow::ensure!((256..=65536).contains(&self.server.db_cache_kib), "db_cache_kib must be 256..65536");
        anyhow::ensure!(!self.merchants.is_empty() && self.merchants.len() <= 1000, "configure 1..1000 merchants");
        anyhow::ensure!(!self.gateways.is_empty() && self.gateways.len() <= 100, "configure 1..100 gateways");
        anyhow::ensure!(unique(self.merchants.iter().map(|m|m.id.clone())), "duplicate merchant id");
        anyhow::ensure!(unique(self.gateways.iter().map(|g|g.id.clone())), "duplicate gateway id");
        for g in &self.gateways {
            anyhow::ensure!(identifier(&g.id), "invalid gateway id");
            let base = network::safe_url(&g.base_url, dev)?;
            anyhow::ensure!(base.query().is_none(), "gateway base URL cannot contain a query");
            anyhow::ensure!(!g.checkout_origins.is_empty(), "checkout_origins is required");
            for origin in &g.checkout_origins {
                let u = network::safe_url(origin, dev)?;
                anyhow::ensure!(u.path() == "/" && u.query().is_none(), "checkout allowlist entries must be origins");
            }
            anyhow::ensure!(!g.currencies.is_empty() && unique(g.currencies.clone()), "currencies must be nonempty and unique");
            for c in &g.currencies { money::exponent(c)?; }
            anyhow::ensure!(!g.methods.is_empty() && unique(g.methods.clone()), "methods must be nonempty and unique");
            match g.protocol {
                Protocol::Epay => {
                    anyhow::ensure!(g.waffo.is_none(), "unexpected waffo configuration for epay");
                    let c = g.epay.as_ref().ok_or_else(||anyhow::anyhow!("epay configuration required"))?;
                    anyhow::ensure!(identifier(&c.pid), "invalid epay pid");
                    anyhow::ensure!(g.currencies == ["CNY"], "legacy ePay v1 only supports CNY; currency conversion is not implemented");
                    anyhow::ensure!(g.methods.iter().all(|m|["alipay","wxpay","qqpay"].contains(&m.as_str())), "unsupported epay method");
                    network::checkout_url(&g.base_url, &g.checkout_origins, dev)?;
                }
                Protocol::WaffoPancake => {
                    anyhow::ensure!(g.epay.is_none(), "unexpected epay configuration for waffo");
                    anyhow::ensure!(base.path() == "/", "Waffo base URL must be an origin");
                    let c = g.waffo.as_ref().ok_or_else(||anyhow::anyhow!("waffo configuration required"))?;
                    anyhow::ensure!(["test","prod"].contains(&c.mode.as_str()), "explicit Waffo mode test/prod is required");
                    anyhow::ensure!(identifier(&c.merchant_id) && identifier(&c.store_id), "invalid Waffo merchant/store identifiers");
                    anyhow::ensure!(!c.products.is_empty() && c.products.len() <= 1000 && unique(c.products.iter().map(|p|p.alias.clone())), "configure unique Waffo product aliases");
                    anyhow::ensure!(g.methods.iter().all(|m|["checkout","card","applepay","googlepay","wechat"].contains(&m.as_str())), "unsupported Waffo method");
                    for p in &c.products { anyhow::ensure!(identifier(&p.alias) && identifier(&p.id) && identifier(&p.tax_category), "invalid Waffo product mapping"); }
                }
            }
        }
        for m in &self.merchants {
            anyhow::ensure!(identifier(&m.id), "invalid merchant id");
            network::safe_url(&m.webhook_url, dev)?;
            anyhow::ensure!(unique(m.gateways.clone()) && m.gateways.contains(&m.default_gateway), "default gateway must be in the merchant allowlist");
            anyhow::ensure!(m.gateways.iter().all(|id|self.gateways.iter().any(|g| &g.id==id)), "merchant references unknown gateway");
        }
        Ok(())
    }
}
