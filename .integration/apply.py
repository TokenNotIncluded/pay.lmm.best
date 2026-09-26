from pathlib import Path
root=Path.cwd()
def change(path, old, new, count=1):
    p=root/path
    s=p.read_text()
    assert s.count(old)==count,(path,old[:80],s.count(old))
    p.write_text(s.replace(old,new))
change('src/lib.rs','pub mod network;','pub mod network;\npub mod providers;')
change('src/config.rs','    pub waffo: Option<WaffoConfig>,','''    pub waffo: Option<WaffoConfig>,
    // Omitting absent NEW fields preserves the frozen identity of existing gateways.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stripe: Option<crate::providers::StripeConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creem: Option<crate::providers::CreemConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lemon_squeezy: Option<crate::providers::LemonConfig>,''')
change('src/config.rs','    WaffoPancake,','    WaffoPancake,\n    Stripe,\n    Creem,\n    LemonSqueezy,')
change('src/config.rs','            Self::WaffoPancake => "waffo_pancake",','''            Self::WaffoPancake => "waffo_pancake",
            Self::Stripe => "stripe",
            Self::Creem => "creem",
            Self::LemonSqueezy => "lemon_squeezy",''')
change('src/config.rs','            match g.protocol {','''            crate::providers::validate_config(g, dev)?;
            match g.protocol {
                Protocol::Stripe | Protocol::Creem | Protocol::LemonSqueezy => {},''')
change('src/gateway.rs',"type FutureResult<'a, T>","pub(crate) type FutureResult<'a, T>")
change('src/gateway.rs','    pub charged_minor: i64,','''    pub charged_minor: i64,
    pub checkout_id: Option<String>,
    pub product: Option<String>,
    pub request_hash: Option<String>,''')
change('src/gateway.rs','        let adapter: Box<dyn Adapter> = match config.protocol {','''        let adapter: Box<dyn Adapter> = match config.protocol {
            Protocol::Stripe | Protocol::Creem | Protocol::LemonSqueezy =>
                crate::providers::build(&config, dev, load)?,''')
change('src/gateway.rs','''            products: self
                .config
                .waffo
                .as_ref()
                .map(|c| c.products.iter().map(|p| p.alias.clone()).collect())
                .unwrap_or_default(),''','''            products: crate::providers::product_aliases(&self.config),''')
change('src/gateway.rs','            charged_minor: amount,','''            charged_minor: amount,
            checkout_id: None, product: None, request_hash: None,''')
change('src/gateway.rs','            charged_minor: charged,','''            charged_minor: charged,
            checkout_id: None, product: None, request_hash: None,''')
change('src/store.rs','''                Some(v) => {
                    r.payment.checkout_url = v.url;''','''                Some(v) => {
                    // A verified callback may have arrived before this API response.
                    require(r.session_id.is_empty() || r.session_id == v.session_id,
                        "checkout_session_mismatch")?;
                    r.payment.checkout_url = v.url;''')
change('src/store.rs','''            require(r.payment.currency==e.currency, "currency_mismatch")?;''','''            require(r.payment.currency==e.currency, "currency_mismatch")?;
            require(e.product.as_ref().is_none_or(|p|p==&r.input.product), "product_mismatch")?;
            require(e.request_hash.as_ref().is_none_or(|h|h==&r.request_hash), "checkout_reference_mismatch")?;
            if let Some(checkout_id)=&e.checkout_id {
                require(r.session_id.is_empty() || &r.session_id==checkout_id, "checkout_session_mismatch")?;
                r.session_id=checkout_id.clone();
            }''')
change('tests/storage.rs','        charged_minor: 100,','''        charged_minor: 100,
        checkout_id: None, product: None, request_hash: None,''')
change('src/network.rs','''    let u = safe_url(value, dev)?;
    let origin = u.origin().ascii_serialization();''','''    // Fragments belong to the browser (Stripe includes one); never sent by our HTTP client.
    require(value.len() <= 8192, "url_too_long")?;
    let mut u = Url::parse(value).map_err(|_| Error::invalid("invalid_url"))?;
    let fragment = u.fragment().map(str::to_owned);
    u.set_fragment(None);
    let mut u = safe_url(u.as_str(), dev)?;
    let origin = u.origin().ascii_serialization();''')
change('src/network.rs','    Ok(u.into())','    u.set_fragment(fragment.as_deref());\n    Ok(u.into())')
change('src/network.rs','''    pub fn post(&self, url: &str) -> Result<RequestBuilder> {''','''    pub fn get(&self, url: &str) -> Result<RequestBuilder> {
        Ok(self.client.get(safe_url(url, self.dev)?))
    }
    pub fn post(&self, url: &str) -> Result<RequestBuilder> {''')
with (root/'tests/storage.rs').open('a') as out:
    out.write('''
#[tokio::test]
async fn hosted_checkout_references_products_and_early_sessions_are_bound() {
    let db = Database::open(":memory:", 512).unwrap();
    let mut r = record("pay_bound");
    r.input.product = "credits".into();
    db.reserve(r).await.unwrap();
    let mut e = event("pay_bound", "evt_bound");
    e.checkout_id = Some("cs_1".into());
    e.product = Some("wrong".into());
    e.request_hash = Some("hash-pay_bound".into());
    assert!(db.accept("gateway", "identity", e.clone()).await.is_err());
    e.product = Some("credits".into());
    e.request_hash = Some("wrong".into());
    assert!(db.accept("gateway", "identity", e.clone()).await.is_err());
    e.request_hash = Some("hash-pay_bound".into());
    db.accept("gateway", "identity", e).await.unwrap();
    assert!(db.finish_create("pay_bound", Some(Checkout { url: "https://checkout.example".into(), session_id: "cs_other".into() })).await.is_err());
    let p = db.finish_create("pay_bound", Some(Checkout { url: "https://checkout.example".into(), session_id: "cs_1".into() })).await.unwrap();
    assert_eq!(p.status, "succeeded");
}
''')
change('proto/pay/v1/pay.proto','Required Waffo product alias; unused by ePay.','Product alias for Waffo/Creem/Lemon Squeezy; empty for ePay/Stripe.')
change('README.md','## 当前支持\n','## 当前支持\n\n已接入 **5 种上游协议**：ePay、Waffo Pancake、Stripe Checkout、Creem、Lemon Squeezy。新渠道共用现有 JSON / Protobuf API，没有新增运行时依赖。详细配置、支持边界和官方协议来源见 [网关接入指南](docs/gateways.md)。\n')
change('README.md','| 通知交付 |','| Stripe Checkout | 一次性收银台、签名验真、延迟支付完成事件 | 不含 Connect、订阅、自动换汇或折扣 |\n| Creem | 一次性商品检查、自定义价格、HMAC 回调、测试/生产绑定 | 当前只接受含税价商品，不含订阅 |\n| Lemon Squeezy | JSON:API checkout、店铺/商品检查、预览金额验证、签名订单回调 | 单店铺币种、单商品，不含订阅/优惠券 |\n| 通知交付 |')
change('web/index.html','ePay · Waffo Pancake','ePay · Waffo Pancake · Stripe · Creem · Lemon Squeezy')
