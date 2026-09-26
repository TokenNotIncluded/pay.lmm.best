from pathlib import Path
import re
p=Path('src/providers/creem.rs')
s=p.read_text()
old='''        let amount = number(order, "amount")?;
        require(amount > 0, "invalid_amount")?;'''
new='''        // List price is not proof of collection: discounts and tax adjustments can differ.
        // A legacy payload without explicit paid evidence must not fulfill an order.
        let amount = number(order, "amount_paid")?;
        require(amount > 0 && number(order, "amount")? == amount,
            "charged_total_mismatch")?;
        if order.get("amount_due").is_some() {
            require(number(order, "amount_due")? == amount, "unpaid_order_balance")?;
        }
        if order.get("discount_amount").is_some() {
            require(number(order, "discount_amount")? == 0, "discount_not_supported")?;
        }
        if order.get("refunded_amount").is_some() {
            require(number(order, "refunded_amount")? == 0, "refunded_order")?;
        }'''
assert s.count(old)==1
p.write_text(s.replace(old,new))
p=Path('tests/hosted.rs');s=p.read_text()
s,n=re.subn(r'"amount"\s*:\s*1230\s*,', '"amount":1230,"amount_paid":1230,"amount_due":1230,"discount_amount":0,',s)
assert n==1,n
s+='''
#[tokio::test]
async fn creem_requires_collected_money_not_only_a_list_price() {
    let h = harness("creem").await;
    let p = create(&h, "creem", "paid-evidence").await;
    let e = event("creem", h.upstream.captured.lock().await.clone(), 1);
    for (field, value) in [
        ("amount_paid", json!(0)),
        ("amount_paid", json!(1229)),
        ("amount_paid", json!(1231)),
        ("amount_paid", Value::Null),
        ("amount_due", json!(1231)),
        ("discount_amount", json!(1)),
        ("refunded_amount", json!(1)),
    ] {
        let mut bad = e.clone();
        bad["object"]["order"][field] = value;
        assert!(send(&h, "creem", &bad).await.is_client_error(), "accepted {field}");
        assert_eq!(h.service.db.get("app", &p.id).await.unwrap().status, "pending");
    }
    let mut old = e.clone();
    old["object"]["order"].as_object_mut().unwrap().remove("amount_paid");
    assert!(send(&h, "creem", &old).await.is_client_error());
    assert_eq!(send(&h, "creem", &e).await, StatusCode::OK);
    assert_eq!(h.service.db.get("app", &p.id).await.unwrap().charged_minor, 1230);
}

#[test]
fn documented_configuration_examples_validate_without_loading_secrets() {
    for source in [
        include_str!("../examples/config.toml"),
        include_str!("../examples/waffo.toml"),
        include_str!("../examples/gateways.toml"),
    ] {
        let c: Config = toml::from_str(source).unwrap();
        c.validate().unwrap();
    }
}
'''
p.write_text(s)
p=Path('docs/gateways.md');s=p.read_text()
s=s.replace('paid 状态和实际订单金额。', 'paid 状态和 `order.amount_paid` 实付金额。实付必须与含税标价及本地订单金额一致；存在 `amount_due` 时也必须一致，非零折扣或已退款金额会被拒绝。缺少 `amount_paid` 的旧载荷不会退回使用标价 `amount`，而是保留待核对状态。')
s += '\n### Creem 金额证据兼容要求\n\nCreem 接入必须提供明确的 `order.amount_paid`。官方旧 webhook 示例只有 `amount`，不能据此证明最终实付金额；本实现对这类旧载荷拒绝确认成功。部署前请从实际沙箱事件验证金额字段，不要删掉此校验来迁就示例。当前没有通过交易查询补全旧载荷的自动流程。专项测试覆盖少付、多付、零支付、缺字段、折扣和退款，但不替代真实上游联调。\n'
p.write_text(s)
p=Path('examples/gateways.toml');s=p.read_text();s=s.replace('# The upstream product must be onetime, active and tax inclusive, without B2B net pricing.', '# The upstream product must be onetime, active and tax inclusive, without B2B net pricing.\n# Callback order.amount_paid is REQUIRED; legacy amount-only payloads fail closed.')
p.write_text(s)
