use super::*;
use crate::{gateway::{Checkout, FutureResult}, wire::CreatePaymentRequest};

pub(super) struct Creem { pub(super) ctx:Context, pub(super) options:CreemConfig }
impl Creem {
    fn product(&self,alias:&str)->Result<&CreemProduct> { self.options.products.iter().find(|p|p.alias==alias).ok_or(Error::invalid("unknown_product")) }
    fn check_product(&self,data:&Value,p:&CreemProduct)->Result<()> {
        require(text(data,"id")?==p.id && text(data,"mode")?==self.ctx.mode.as_str(),"product_identity_mismatch")?;
        require(text(data,"currency")?==p.currency && text(data,"billing_type")?=="onetime" && text(data,"status")?=="active","unsupported_product")?;
        // Only a fully inclusive price has an unambiguous gross amount in order.amount.
        require(text(data,"tax_mode")?=="inclusive" && data.get("business_net_pricing").is_none_or(|v|v==false),"creem_requires_tax_inclusive_price")
    }
}
impl Adapter for Creem {
    fn prepare(&self,input:&CreatePaymentRequest)->Result<String> {
        let p=self.product(&input.product)?;
        require(input.currency==p.currency && (100..=99_999_999).contains(&input.amount_minor),"invalid_creem_amount_or_currency")?;
        Ok("total".into())
    }
    fn create<'a>(&'a self,r:&'a Record,public_url:&'a str,client:&'a SafeClient)->FutureResult<'a,Checkout> {
        Box::pin(async move {
            let p=self.product(&r.input.product)?;
            let product=json_response(client,client.get(&self.ctx.url(&format!("/v1/products/{}",p.id)))?.header("x-api-key",self.ctx.api_key.as_str())).await?;
            self.check_product(&product,p)?;
            let body=serde_json::to_vec(&json!({"product_id":p.id,"request_id":r.payment.id,"units":1,"custom_price":r.input.amount_minor,"success_url":format!("{public_url}/return/{}",r.payment.id),"metadata":self.ctx.reference(r)}))?;
            let data=json_response(client,client.post(&self.ctx.url("/v1/checkouts"))?.header("x-api-key",self.ctx.api_key.as_str()).header("Content-Type","application/json").body(body)).await?;
            require(text(&data,"mode")?==self.ctx.mode.as_str() && text(&data,"request_id")?==r.payment.id && object_id(&data["product"])?==p.id,"checkout_response_mismatch")?;
            require(number(&data,"custom_price")?==r.input.amount_minor,"checkout_price_mismatch")?;
            Ok(Checkout{url:self.ctx.checkout(text(&data,"checkout_url")?)?,session_id:id_field(&data,"id")?})
        })
    }
    fn verify(&self,method:&str,headers:&HeaderMap,body:&[u8],query:&str)->Result<Option<VerifiedEvent>> {
        json_webhook(method,headers,query)?;self.ctx.hmac_body(headers,"creem-signature",body)?;
        let event:Value=serde_json::from_slice(body).map_err(|_|Error::invalid("invalid_webhook"))?;
        if text(&event,"eventType")?!="checkout.completed"{return Ok(None);}
        let data=&event["object"];let order=&data["order"];
        require(text(data,"object")?=="checkout" && text(data,"status")?=="completed" && text(order,"status")?=="paid","contradictory_payment_status")?;
        require(text(data,"mode")?==self.ctx.mode.as_str() && text(order,"mode")?==self.ctx.mode.as_str(),"provider_mode_mismatch")?;
        require(text(order,"type")?=="onetime" && data.get("subscription").is_none_or(Value::is_null),"subscription_not_supported")?;
        let product_id=object_id(&data["product"])?;
        let p=self.options.products.iter().find(|p|p.id==product_id).ok_or(Error::invalid("unknown_product"))?;
        require(object_id(&order["product"])?==p.id && text(order,"currency")?==p.currency,"product_identity_mismatch")?;
        if data["product"].is_object(){self.check_product(&data["product"],p)?;}
        require(data.get("units").is_none_or(|v|v.as_i64()==Some(1)),"unexpected_quantity")?;
        let (payment_id,request_hash)=self.ctx.reference_parts(&data["metadata"])?;
        require(text(data,"request_id")?==payment_id,"checkout_reference_mismatch")?;
        let amount=number(order,"amount")?;require(amount>0,"invalid_amount")?;
        let order_id=id_field(order,"id")?;
        Ok(Some(VerifiedEvent{id:id_field(&event,"id")?,fingerprint:fingerprint(&event)?,payment_id,provider_order_id:order_id.clone(),provider_payment_id:order_id,currency:p.currency.clone(),method:None,total_minor:amount,subtotal_minor:None,charged_minor:amount,checkout_id:Some(id_field(data,"id")?),product:Some(p.alias.clone()),request_hash:Some(request_hash)}))
    }
}
