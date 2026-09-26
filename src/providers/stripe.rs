use super::*;
use crate::{
    gateway::{Checkout, FutureResult},
    wire::CreatePaymentRequest,
};

pub(super) struct Stripe {
    pub(super) ctx: Context,
    pub(super) options: StripeConfig,
}
impl Stripe {
    fn signature(&self, headers: &HeaderMap, body: &[u8]) -> Result<()> {
        let header = single_header(headers, "stripe-signature")?;
        let mut timestamp = None;
        let mut signatures = Vec::new();
        for part in header.split(',') {
            let (name, value) = part.trim().split_once('=').ok_or(Error::unauthorized())?;
            match name {
                "t" if timestamp.is_none() => timestamp = Some(value),
                "t" => return Err(Error::unauthorized()),
                "v1" => {
                    require(signatures.len() < 8, "too_many_signatures")?;
                    signatures.push(value);
                }
                _ => {} // Other version schemes may coexist; never verify against them.
            }
        }
        let t = timestamp.ok_or(Error::unauthorized())?;
        require(
            !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()),
            "invalid_timestamp",
        )?;
        let seconds = t.parse::<i64>().map_err(|_| Error::unauthorized())?;
        let age = crate::now()
            .checked_sub(seconds)
            .ok_or(Error::unauthorized())?;
        if !(-60..=300).contains(&age) {
            return Err(Error::unauthorized());
        }
        let mut input = t.as_bytes().to_vec();
        input.push(b'.');
        input.extend_from_slice(body);
        if signatures.iter().any(|s| {
            hex::decode(s).is_ok_and(|sig| hmac::verify(&self.ctx.key, &input, &sig).is_ok())
        }) {
            Ok(())
        } else {
            Err(Error::unauthorized())
        }
    }
}
impl Adapter for Stripe {
    fn prepare(&self, input: &CreatePaymentRequest) -> Result<String> {
        require(
            input.product.is_empty(),
            "stripe_uses_inline_price_not_product_alias",
        )?;
        require(input.amount_minor <= 99_999_999, "stripe_amount_too_large")?;
        Ok("total".into())
    }
    fn create<'a>(
        &'a self,
        r: &'a Record,
        public_url: &'a str,
        client: &'a SafeClient,
    ) -> FutureResult<'a, Checkout> {
        Box::pin(async move {
            let mut fields = vec![
                ("mode".to_string(), "payment".to_string()),
                ("client_reference_id".into(), r.payment.id.clone()),
                (
                    "success_url".into(),
                    format!("{public_url}/return/{}", r.payment.id),
                ),
                (
                    "cancel_url".into(),
                    format!("{public_url}/return/{}", r.payment.id),
                ),
                (
                    "line_items[0][price_data][currency]".into(),
                    r.input.currency.to_ascii_lowercase(),
                ),
                (
                    "line_items[0][price_data][unit_amount]".into(),
                    r.input.amount_minor.to_string(),
                ),
                (
                    "line_items[0][price_data][product_data][name]".into(),
                    r.input.description.clone(),
                ),
                ("line_items[0][quantity]".into(), "1".into()),
                ("automatic_tax[enabled]".into(), "false".into()),
                ("adaptive_pricing[enabled]".into(), "false".into()),
                ("allow_promotion_codes".into(), "false".into()),
            ];
            if r.input.method != "checkout" {
                fields.push(("payment_method_types[0]".into(), r.input.method.clone()));
            }
            for (key, value) in self.ctx.reference(r).as_object().ok_or(Error::internal())? {
                fields.push((
                    format!("metadata[{key}]"),
                    value.as_str().ok_or(Error::internal())?.into(),
                ));
            }
            let body = url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(fields)
                .finish();
            let data = json_response(
                client,
                client
                    .post(&self.ctx.url("/v1/checkout/sessions"))?
                    .bearer_auth(self.ctx.api_key.as_str())
                    .header("Stripe-Version", &self.options.api_version)
                    .header("Idempotency-Key", &r.payment.id)
                    .header("Content-Type", "application/x-www-form-urlencoded")
                    .body(body),
            )
            .await?;
            require(
                text(&data, "object")? == "checkout.session" && text(&data, "mode")? == "payment",
                "invalid_checkout_response",
            )?;
            require(
                flag(&data, "livemode")? == self.ctx.mode.live(),
                "provider_mode_mismatch",
            )?;
            require(
                text(&data, "client_reference_id")? == r.payment.id
                    && number(&data, "amount_total")? == r.input.amount_minor
                    && text(&data, "currency")? == r.input.currency.to_ascii_lowercase(),
                "checkout_response_mismatch",
            )?;
            Ok(Checkout {
                url: self.ctx.checkout(text(&data, "url")?)?,
                session_id: id_field(&data, "id")?,
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
        self.signature(headers, body)?;
        let event: Value =
            serde_json::from_slice(body).map_err(|_| Error::invalid("invalid_webhook"))?;
        require(
            flag(&event, "livemode")? == self.ctx.mode.live(),
            "provider_mode_mismatch",
        )?;
        // This adapter uses direct merchant accounts, not Connect event multiplexing.
        require(
            event.get("account").is_none_or(Value::is_null),
            "stripe_connect_not_supported",
        )?;
        let kind = text(&event, "type")?;
        if ![
            "checkout.session.completed",
            "checkout.session.async_payment_succeeded",
        ]
        .contains(&kind)
        {
            return Ok(None);
        }
        let data = &event["data"]["object"];
        require(
            text(data, "object")? == "checkout.session",
            "invalid_checkout_object",
        )?;
        if text(data, "mode")? != "payment" {
            return Ok(None);
        }
        require(
            flag(data, "livemode")? == self.ctx.mode.live(),
            "provider_mode_mismatch",
        )?;
        match text(data, "payment_status")? {
            "unpaid" if kind == "checkout.session.completed" => return Ok(None),
            "no_payment_required" => return Ok(None),
            "paid" => {}
            _ => return Err(Error::invalid("contradictory_payment_status")),
        }
        require(
            text(data, "status")? == "complete",
            "contradictory_payment_status",
        )?;
        let (payment_id, request_hash) = self.ctx.reference_parts(&data["metadata"])?;
        require(
            text(data, "client_reference_id")? == payment_id,
            "checkout_reference_mismatch",
        )?;
        let total = number(data, "amount_total")?;
        require(
            total > 0 && number(data, "amount_subtotal")? == total,
            "unexpected_checkout_adjustment",
        )?;
        for field in ["amount_tax", "amount_discount", "amount_shipping"] {
            require(
                number(&data["total_details"], field)? == 0,
                "unexpected_checkout_adjustment",
            )?;
        }
        let id = id_field(data, "id")?;
        Ok(Some(VerifiedEvent {
            id: id_field(&event, "id")?,
            fingerprint: fingerprint(&event)?,
            payment_id,
            provider_order_id: id.clone(),
            provider_payment_id: object_id(&data["payment_intent"])?,
            currency: text(data, "currency")?.to_ascii_uppercase(),
            method: None,
            total_minor: total,
            subtotal_minor: Some(total),
            charged_minor: total,
            checkout_id: Some(id),
            product: None,
            request_hash: Some(request_hash),
        }))
    }
}
