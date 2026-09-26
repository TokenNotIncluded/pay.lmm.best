# pay.lmm.best

### 一个接口，连接已有支付网关。

**这是支付网关的聚合服务，不是支付网关。**

`pay.lmm.best` 是用 Rust 编写的协议适配与订单通知服务：业务系统调用统一接口，聚合层把订单交给已配置的上游网关，用户在上游收银台付款，聚合层验证结果后通知业务系统。

**本项目不提供支付通道、收单、资金托管、钱包余额、清算结算、提现或代付功能。银行卡信息应只在上游收银台输入。本服务不接收银行卡专用字段、不处理真实扣款。商户须使用自己已获授权的上游账户。** 这是软件功能边界，不是关于运营资质或监管义务的法律结论。

```text
业务系统 ── JSON / Protobuf ──► pay.lmm.best ──► 已有支付网关
                                  │                    │
                                  │              上游托管收银台
                                  │                    │
业务系统 ◄── 签名通知 + 重试 ──────┴──── 验签后的结果 ◄──┘
```

## 当前支持

| 接口 / 适配器 | 已实现 | 明确不做 |
| --- | --- | --- |
| 统一 HTTP API | 创建订单、查询本地状态、商户渠道列表、死信重投 | 不宣称兼容上游的所有接口 |
| Protocol Buffers | 同一路由上的二进制请求与响应，schema 在 `proto/pay/v1/pay.proto` | 当前不是 gRPC |
| ePay v1 | `submit.php` 签名跳转、GET / POST 回调、MD5 兼容验签、CNY | 不自动换汇、不冒充支付宝或微信直连 |
| Waffo Pancake | RSA-SHA256 创建一次性 checkout、显式商品映射、验签、店铺及环境绑定 | 不自动发布商品、不把订阅事件当一次性付款 |
| 通知交付 | SQLite 事务 outbox、HMAC-SHA256、持久化去重、16 次投递、死信重投 | 不保证网络交付 exactly-once；接收方仍需按事件 ID 去重 |

退款发起、订阅续费、上游主动查单、自动对账尚未实现，渠道能力接口明确返回 `false`，没有“成功返回”的占位实现。`GET /v1/payments/{id}` 返回的是**本地已验证状态**，不调用上游查单。

## 运行

需要 Rust stable、C 编译器和 `protoc`。构建期的 Protobuf 工具不进入运行时。

```sh
# Debian / Ubuntu 构建依赖
sudo apt-get install build-essential protobuf-compiler
cargo build --release

cp examples/config.toml config.toml
# 编辑 config.toml：填入真实上游地址、自己的商户号及回调地址。
export PAY_APP_API_KEY="$(openssl rand -hex 32)"
export PAY_APP_WEBHOOK_SECRET="$(openssl rand -hex 32)"
export EPAY_KEY='上游提供的密钥'

./target/release/pay-lmm --config ./config.toml --check-config
./target/release/pay-lmm --config ./config.toml
```

默认监听 `127.0.0.1:8080`。生产环境在前面配置 HTTPS 反向代理。默认配置路径为 `/etc/pay.lmm.best/config.toml`。`--check-config` 也会校验凭据格式、打开数据库并应用必要迁移；请使用独立测试数据库检查新配置。

`examples/config.toml` 使用保留示例域名，**不能直接产生真实付款**。Waffo 的配置示例见 [`examples/waffo.toml`](examples/waffo.toml)。不要把真实凭据写进 TOML、Git、浏览器代码或日志。

## 创建订单

```sh
curl -sS http://127.0.0.1:8080/v1/payments \
  -H "Authorization: Bearer $PAY_APP_API_KEY" \
  -H 'Idempotency-Key: order-20260927-001' \
  -H 'Content-Type: application/json' \
  --data '{
    "merchant_order_id": "order-20260927-001",
    "amount_minor": 1230,
    "currency": "CNY",
    "method": "alipay",
    "description": "示例商品"
  }'
```

`1230 CNY` 表示 `12.30 CNY`，金额不经过浮点数。将返回的 `checkout_url` 交给用户打开，不在本服务收集付款凭据。上游成功跳转**不代表已付款**；必须以已验签通知或本地订单状态为准。

同一商户的同一 `Idempotency-Key`、同一请求始终返回同一订单；改变请求内容返回 `409`。商户业务订单号也有唯一约束。接口鉴权由商户 API key 决定，请求不能通过伪造 `merchant_id` 访问别人的订单。

### Protobuf

```sh
printf '%s\n' 'merchant_order_id: "protobuf-001" amount_minor: 1230 currency: "CNY" method: "alipay" description: "Example"' \
  | protoc -I proto --encode=pay.v1.CreatePaymentRequest proto/pay/v1/pay.proto \
  | curl -sS http://127.0.0.1:8080/v1/payments \
      -H "Authorization: Bearer $PAY_APP_API_KEY" \
      -H 'Idempotency-Key: protobuf-001' \
      -H 'Content-Type: application/x-protobuf' \
      -H 'Accept: application/x-protobuf' --data-binary @- \
  | protoc -I proto --decode=pay.v1.Payment proto/pay/v1/pay.proto
```

根据 HTTP 状态选择 `Payment` 或 `ErrorResponse` 解码错误响应。JSON 和 Protobuf 共享校验、幂等、鉴权与业务逻辑。上游仍使用各自原生协议；商户异步通知目前使用 JSON。

## 接口

| 方法 | 路径 | 说明 |
| --- | --- | --- |
| POST | `/v1/payments` | 统一下单；必须携带幂等键 |
| GET | `/v1/payments/{id}` | 查询当前商户自己的本地订单 |
| GET | `/v1/gateways` | 仅返回当前商户获准使用的渠道及能力 |
| POST | `/v1/payments/{id}/notifications/retry` | 重投当前商户的 `dead` 通知 |
| GET / POST | `/hooks/{gateway_id}` | 上游回调，按适配器验签 |
| GET | `/return/{id}` | 静态提示页，不改变订单状态 |
| GET | `/healthz` / `/readyz` | 存活 / 数据库就绪检查 |
| GET | `/proto/pay/v1/pay.proto` | 公共消息定义 |

## 一致性与安全

订单状态为 `creating → pending → succeeded`。创建 checkout 时若网络失败、响应无法确认，状态为 `unknown`；崩溃遗留的 `creating` 在启动时也转为 `unknown`。**不会自动切换渠道或重复下单**。后续有效回调仍可确认这笔订单；缺少回调时需人工在上游核对，当前无自动恢复创建或主动查单接口。

回调必须通过签名、上游账户 / 渠道身份、订单号、币种、金额核对。ePay 还核对支付方式；Waffo 显式绑定 `test` / `prod` 公钥与店铺，校验实际扣款和税额口径。签名时间窗、事件去重、上游付款唯一绑定各自独立，不能用其中一项替代其他项。

状态变更、回调收据、商户通知在同一 SQLite 事务提交后才确认上游。商户通知签名是 `HMAC-SHA256(timestamp + "." + raw_body)`，通过 `X-Pay-Signature: t=...,v1=...` 传递；接收端先验签、检查时间窗口，再按 `X-Pay-Event-Id` / body `id` 去重并持久化，最后返回 2xx。详见 [`docs/security.md`](docs/security.md)。

## 面向小服务器

使用单个 Tokio 事件循环、最多两个阻塞工作线程、单 SQLite 连接和单通知投递协程。默认并发上限 32，请求 / 上游响应上限 64 KiB，SQLite 页缓存 2 MiB，禁用 mmap，订单与通知保存在磁盘，不维护无限增长的内存订单列表。HTTP 客户端复用连接，不启用压缩或 HTTP/2 功能。

这些是资源约束设计，不是未经测试的 RSS 承诺；部署时仍需限制连接数、配置反向代理限流，并测量真实负载。SQLite 版本只允许一个进程使用同一个数据库文件，不支持多副本共享数据库。后续存储适配可扩展为 PostgreSQL，但本版没有该实现。

## 开发

```sh
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
```

领域模型与 HTTP 编码分开，`gateway::Adapter` 负责 `prepare / create / verify`，不允许适配器直接写业务数据库。扩展协议时同时补上签名、金额、失败行为和回调测试，再更新能力表，不用一套通用签名函数猜所有网关协议。

协议依据：Waffo 官方 [Go SDK](https://github.com/waffo-com/waffo-pancake-sdk-go) 的签名、checkout 与 webhook 定义；ePay 兼容接口参考 [go-epay](https://github.com/Calcium-Ion/go-epay)。不同 ePay 部署可能有方言差异，上线前必须用自己的上游沙箱验收。本仓库的模拟网关测试不能代替真实商户联调。

MIT License · [Security](SECURITY.md) · [Architecture](docs/architecture.md)
