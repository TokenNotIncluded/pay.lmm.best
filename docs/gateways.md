# 网关接入指南

本项目聚合已有支付网关。所有付款、银行卡输入、实际扣款与资金处理仍由上游完成。本次新增 Stripe Checkout、Creem、Lemon Squeezy 后，共有 **5 种上游协议适配**；不是把支付宝、银行卡等支付方式重复计作不同网关。

## 支持范围

| 协议值 | 下单方式 | 成功事件 | 当前边界 |
| --- | --- | --- | --- |
| `epay` | `submit.php` 签名跳转 | `TRADE_SUCCESS` | 旧版 CNY 协议 |
| `waffo_pancake` | RSA 签名 checkout session | `order.completed` | 一次性商品 |
| `stripe` | Checkout Sessions API | `checkout.session.completed`、`checkout.session.async_payment_succeeded` | `mode=payment`，非 Connect；必须已经 paid |
| `creem` | Checkout API + 商品预检 | `checkout.completed` | `onetime`、含税价商品，单件自定义价格 |
| `lemon_squeezy` | JSON:API checkout + 店铺/variant 预检 | `order_created` | 一次性 variant，单店铺币种，禁用优惠券 |

所有渠道共用 `POST /v1/payments`、`GET /v1/payments/{id}`、`GET /v1/gateways` 和原有商户通知机制。JSON 与 Protobuf 请求均可选渠道，无需另建一套业务接口。退款、订阅、主动上游订单查询与自动对账仍不支持，能力字段保持 `false`；商品预检不是主动支付对账。

完整独立测试配置见 [`examples/gateways.toml`](../examples/gateways.toml)。使用真实凭据前替换示例商户地址、商品 ID 与收银台域名白名单。也可把其中 `[[gateways]]` 配置块加入现有配置，并给相应商户的 `gateways` 列表增加渠道 ID。未配置的协议不会加载凭据或启动后台任务。**本次没有新增 Cargo 依赖。**

## 通用调用

```sh
curl -sS http://127.0.0.1:8080/v1/payments \
  -H "Authorization: Bearer $PAY_TEST_API_KEY" \
  -H 'Idempotency-Key: example-creem-001' \
  -H 'Content-Type: application/json' \
  --data '{
    "merchant_order_id": "example-creem-001",
    "gateway_id": "creem-test",
    "product": "credits",
    "amount_minor": 1230,
    "currency": "USD",
    "method": "checkout",
    "description": "Credits"
  }'
```

`product` 填本地商品别名，而不是随意输入上游商品 ID。Stripe 使用本次订单的内联价格，因此其 `product` 应省略或为空。`gateway_id` 省略时仍使用该商户配置的默认渠道。`method=checkout` 表示让上游托管页面选择可用支付方式，不代表本服务创建了新的支付通道。

Protobuf 使用相同 `CreatePaymentRequest` 字段；设置 `Content-Type` 与 `Accept` 为 `application/x-protobuf` 即可。消息字段编号没有变化，旧客户端不需要重新设计请求。

## Stripe Checkout

配置自己的 Stripe API key 和该回调端点专属的 webhook secret。`mode=test` 仅接受 `sk_test_` / `rk_test_` API key，`prod` 仅接受 live 前缀；回调的事件和 Session `livemode` 都会核对。选择商户自己的账户事件，不要把 Connect 多账户事件发到本适配器。

API 使用配置中的 `Stripe-Version`，示例固定为 `2025-06-30.basil` 作为兼容目标，而不是自动追随最新版。实际账户的 API 与 webhook 版本应通过沙箱验证后再升级。

每笔请求创建一个 `mode=payment` Session，数量固定 1，金额为整数最小货币单位，自动税费、自动换汇与优惠码关闭。支持 `checkout`、`card`、`alipay` 请求方式；实际可用方式及币种组合由上游账户决定，不能由本服务的配置绕过。

在 Stripe 后台注册 `https://pay.lmm.best/hooks/stripe-test`，订阅上表两个 checkout 事件。`completed` 但 `payment_status=unpaid` 只确认收件，不标记付款成功；等后续 `async_payment_succeeded` 并且为 `paid` 才确认。免费/免付款 Session 不会变成已支付订单。

签名按 `Stripe-Signature` 的时间戳与原始正文做 HMAC-SHA256 验证，支持同一个签名头里的多个 `v1`，不接受重复时间戳。接受过去 5 分钟、未来最多 1 分钟的签名时间。请求幂等键同时传给 Stripe；网络错误仍不会自动重建 Session。

Stripe 返回的收银台 URL 可能包含 `#`。现在仅浏览器跳转 URL 允许保留片段；服务端 HTTP 请求仍拒绝片段、非公共地址与重定向。

## Creem

测试 API 地址是 `https://test-api.creem.io`，生产是 `https://api.creem.io`；配置和 API key 前缀必须匹配。两个环境必须使用各自的 webhook secret。回调地址例如 `https://pay.lmm.best/hooks/creem-test`，订阅 `checkout.completed`。

创建 checkout 前，会读取配置的商品，核对 ID、环境、币种、active 状态与 `billing_type=onetime`。当前适配器**只支持 `tax_mode=inclusive`，且关闭 business net pricing 的商品**，避免税前金额与实际支付金额混为一谈。独立的商品货币代码还必须与本地请求一致。

使用官方 `custom_price` 覆盖本次一次性商品价格，`units=1`。当前支持金额 100–99,999,999，单位为配置币种的分；仅支持两位小数币种。Creem 的 `request_id` 用作订单关联，**不宣称它具备上游幂等保证**；本地持久化幂等与不自动重发策略仍然生效。

回调检查原始正文 `creem-signature` HMAC-SHA256、checkout/order 环境、onetime 类型、商品 ID、paid 状态和实际订单金额。字符串和展开对象两种商品字段形式均能解析。发生优惠折扣或其他导致实际金额不同的调整时不确认成功，请在账户侧禁用这类流程并用沙箱验证。

## Lemon Squeezy

使用自己的 store ID、一次性 variant ID 和 API key。每个渠道只配置一种与店铺一致的两位小数币种。创建前读取 store 和 variant，验证币种、测试模式、发布状态，以及非订阅、非 pay-what-you-want 属性；缺少明确的一次性商品证据时拒绝继续创建，而不是猜测。

通过 JSON:API `POST /v1/checkouts` 设置 `custom_price`、唯一允许的 variant、单件数量、禁用优惠券和 `test_mode`，并要求返回 preview。收银台返回的 store/variant、币种和金额必须先匹配，才把 URL 返回给调用者。

`amount_basis=subtotal` 用于税外价商品：订单请求金额对应小计，已验证的实际收款 `charged_minor` 包含税。例如请求 1,230、税 246，则最终收款 1,476。回调要求非含税价且 `subtotal + tax = total`。含税价商品使用 `amount_basis=total`，请求金额必须匹配最终总额。金额口径不能在下单后随意改变。

在后台注册 `https://pay.lmm.best/hooks/lemon-test`，订阅 `order_created`。使用原始正文验证 `X-Signature`，依据签名正文中的事件名路由；如还提供 `X-Event-Name`，它必须一致。订单必须 paid、未退款，并匹配 store、variant、币种与环境。

该协议没有本实现可依赖的唯一投递事件 ID 或签名时间头，因此使用 `order_created:<provider_order_id>` 和持久化收据去重。**不要套用 Stripe 的短时间窗口拒绝正常重试，也不要擅自删除去重数据。**

## 共同安全约束与迁移

新增适配器发送的订单引用含有本服务生成的、带独立用途前缀的 HMAC 绑定；即使攻击者能修改 checkout 自定义字段，并让上游为该内容签发合法 webhook，也不能仅靠替换本地订单号把付款指向任意订单。回调还需要匹配原始请求摘要、实际商品和付款金额。Stripe/Creem 在本地保存并校验 checkout Session ID；回调先于创建响应到达时也不能被另一个 Session 覆盖。

Stripe/Creem/Lemon Squeezy 的 webhook secret 同时用于带用途隔离的订单引用签名。轮换前先处理旧的未完成 checkout，或保留旧渠道及其密钥直到订单结束；当前没有自动多密钥轮换。不要在有待完成订单时直接覆盖密钥。

旧 ePay/Waffo 配置无需增加空配置块。新增可选字段在未配置时不进入渠道身份序列化，因此已有订单的 frozen gateway identity 不会仅因软件升级改变。数据库 schema 和 Protobuf 字段编号保持兼容，不要求清空数据库。

仍使用单事件循环、SQLite 和有界请求/响应。新增商品 GET 与 checkout POST 使用原来的共享 HTTP 客户端及 SSRF 防护，没有新建连接池或常驻 SDK 线程。生产容量和真实支付联调仍需在自己的环境验证；历史 ePay 内存样本不能代表三个新协议的压力测试结果。

## 验证与限制

`tests/hosted.rs` 使用本地模拟上游，检查实际发出的鉴权头、请求字段、商品预检和收银台响应。经过 HTTP 路由的 Protobuf 下单、回调验签、重复投递、错误金额/币种/环境/商品/状态、篡改引用、重复签名头、Stripe 延迟支付、时间窗与多签名，以及未知创建结果后的迟到成功通知均有测试。

没有使用真实商户凭据，没有执行真实支付或部署域名。这里是协议集成实现与模拟测试，不是各服务商的认证。退款、订阅生命周期、Connect、主动对账及新的下游 ePay 兼容入口不在本次实现范围。

## 官方协议依据

以下是实现时核对的第一方协议资料；不同账户、API 版本和商品配置须额外验收。

- Stripe：[创建 Checkout Session](https://docs.stripe.com/api/checkout/sessions/create)、[Webhook 验签与投递](https://docs.stripe.com/webhooks)、[Basil API 版本](https://docs.stripe.com/changelog/basil)。
- Creem：[创建 Checkout](https://docs.creem.io/api-reference/endpoint/create-checkout)、[读取商品](https://docs.creem.io/api-reference/endpoint/get-product)、[Webhook 签名和载荷](https://docs.creem.io/code/webhooks)、[测试环境](https://docs.creem.io/getting-started/test-mode)。
- Lemon Squeezy：[创建 Checkout](https://docs.lemonsqueezy.com/api/checkouts/create-checkout)、[订单字段](https://docs.lemonsqueezy.com/api/orders/the-order-object)、[Variant 字段](https://docs.lemonsqueezy.com/api/variants/the-variant-object)、[店铺字段](https://docs.lemonsqueezy.com/api/stores/the-store-object)、[Webhook 验签](https://docs.lemonsqueezy.com/help/webhooks/signing-requests)。
