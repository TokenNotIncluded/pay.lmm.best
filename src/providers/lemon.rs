use super::*;
use crate::{
    gateway::{Checkout, FutureResult},
    wire::CreatePaymentRequest,
};

pub(super) struct Lemon {
    pub(super) ctx: Context,
    pub(super) options: LemonConfig,
}
impl Lemon {
    fn product(&self, alias: &str) -> Result<&LemonVariant> {
        self.options
            .products
            .iter()
            .find(|p| p.alias == alias)
            .ok_or(Error::invalid("unknown_product"))
    }
    fn request(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .bearer_auth(self.ctx.api_key.as_str())
            .header("Accept", "application/vnd.api+json")
            .header("Content-Type", "application/vnd.api+json")
    }
}
impl Adapter for Lemon {
    fn prepare(&self, input: &CreatePaymentRequest) -> Result<String> {
        require(
            input.currency == self.ctx.config.currencies[0],
            "store_currency_mismatch",
        )?;
        Ok(self.product(&input.product)?.amount_basis.as_str().into())
    }
    fn create<'a>(
        &'a self,
        r: &'a Record,
        public_url: &'a str,
        client: &'a SafeClient,
    ) -> FutureResult<'a, Checkout> {
        Box::pin(async move {
            let p = self.product(&r.input.product)?;
            let store = json_response(
                client,
                self.request(
                    client.get(
                        &self
                            .ctx
                            .url(&format!("/v1/stores/{}", self.options.store_id)),
                    )?,
                ),
            )
            .await?;
            require(
                text(&store["data"], "type")? == "stores"
                    && text(&store["data"], "id")? == self.options.store_id
                    && text(&store["data"]["attributes"], "currency")? == r.input.currency,
                "store_currency_mismatch",
            )?;
            let variant = json_response(
                client,
                self.request(client.get(&self.ctx.url(&format!("/v1/variants/{}", p.id)))?),
            )
            .await?;
            let a = &variant["data"]["attributes"];
            require(
                text(&variant["data"], "type")? == "variants"
                    && text(&variant["data"], "id")? == p.id
                    && flag(a, "test_mode")? != self.ctx.mode.live(),
                "product_identity_mismatch",
            )?;
            // Missing legacy compatibility flags fail closed rather than assuming one-time.
            require(
                !flag(a, "is_subscription")?
                    && !flag(a, "pay_what_you_want")?
                    && text(a, "status")? == "published",
                "unsupported_product",
            )?;
            let variant_id =
                p.id.parse::<u64>()
                    .map_err(|_| Error::invalid("invalid_variant_id"))?;
            let payload = json!({"data":{"type":"checkouts","attributes":{"custom_price":r.input.amount_minor,"test_mode":!self.ctx.mode.live(),"preview":true,"product_options":{"enabled_variants":[variant_id],"redirect_url":format!("{public_url}/return/{}",r.payment.id)},"checkout_options":{"discount":false,"skip_trial":true},"checkout_data":{"custom":self.ctx.reference(r),"variant_quantities":[{"variant_id":variant_id,"quantity":1}]}},"relationships":{"store":{"data":{"type":"stores","id":self.options.store_id}},"variant":{"data":{"type":"variants","id":p.id}}}}});
            let result = json_response(
                client,
                self.request(client.post(&self.ctx.url("/v1/checkouts"))?)
                    .body(serde_json::to_vec(&payload)?),
            )
            .await?;
            let data = &result["data"];
            let a = &data["attributes"];
            require(
                text(data, "type")? == "checkouts"
                    && flag(a, "test_mode")? != self.ctx.mode.live()
                    && object_id(&a["store_id"])? == self.options.store_id
                    && object_id(&a["variant_id"])? == p.id,
                "checkout_response_mismatch",
            )?;
            require(
                number(a, "custom_price")? == r.input.amount_minor
                    && text(&a["preview"], "currency")? == r.input.currency
                    && number(&a["preview"], p.amount_basis.as_str())? == r.input.amount_minor
                    && number(&a["preview"], "discount_total")? == 0,
                "checkout_price_mismatch",
            )?;
            Ok(Checkout {
                url: self.ctx.checkout(text(a, "url")?)?,
                session_id: id_field(data, "id")?,
            })
        })
    }
    fn verify(
        &self,
        method: &str,
        headers: &HeaderMap,
        body: &[u8],
        query: &str,
    ) -> Result<Option<VerifiedEvent>> {
        json_webhook(method, headers, query)?;
        self.ctx.hmac_body(headers, "x-signature", body)?;
        let event: Value =
            serde_json::from_slice(body).map_err(|_| Error::invalid("invalid_webhook"))?;
        let kind = text(&event["meta"], "event_name")?;
        if headers.contains_key("x-event-name") {
            require(
                single_header(headers, "x-event-name")? == kind,
                "event_name_mismatch",
            )?;
        }
        if kind != "order_created" {
            return Ok(None);
        }
        let data = &event["data"];
        let a = &data["attributes"];
        require(
            text(data, "type")? == "orders"
                && text(a, "status")? == "paid"
                && !flag(a, "refunded")?,
            "contradictory_payment_status",
        )?;
        require(
            a.get("refunded_amount")
                .is_none_or(|v| v.as_i64() == Some(0)),
            "refunded_order",
        )?;
        require(
            flag(a, "test_mode")? != self.ctx.mode.live()
                && object_id(&a["store_id"])? == self.options.store_id,
            "provider_mode_or_store_mismatch",
        )?;
        let item = &a["first_order_item"];
        let variant = object_id(&item["variant_id"])?;
        let p = self
            .options
            .products
            .iter()
            .find(|p| p.id == variant)
            .ok_or(Error::invalid("unknown_product"))?;
        let id = id_field(data, "id")?;
        require(object_id(&item["order_id"])? == id, "order_item_mismatch")?;
        require(
            text(a, "currency")? == self.ctx.config.currencies[0],
            "store_currency_mismatch",
        )?;
        let (payment_id, request_hash) = self.ctx.reference_parts(&event["meta"]["custom_data"])?;
        let total = number(a, "total")?;
        let subtotal = number(a, "subtotal")?;
        let tax = number(a, "tax")?;
        require(
            total > 0 && tax <= total && number(a, "discount_total")? == 0,
            "unexpected_checkout_adjustment",
        )?;
        let inclusive = flag(a, "tax_inclusive")?;
        if !inclusive {
            require(
                subtotal.checked_add(tax) == Some(total),
                "tax_total_mismatch",
            )?;
        }
        if p.amount_basis == AmountBasis::Subtotal {
            require(!inclusive, "subtotal_requires_tax_exclusive_product")?;
        }
        // Lemon Squeezy has no delivery event ID/timestamp header. Bind stable order ID.
        Ok(Some(VerifiedEvent {
            id: format!("order_created:{id}"),
            fingerprint: fingerprint(&event)?,
            payment_id,
            provider_order_id: id.clone(),
            provider_payment_id: id,
            currency: text(a, "currency")?.into(),
            method: None,
            total_minor: total,
            subtotal_minor: Some(subtotal),
            charged_minor: total,
            checkout_id: None,
            product: Some(p.alias.clone()),
            request_hash: Some(request_hash),
        }))
    }
}
