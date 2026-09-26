# pay.lmm.best

**支付网关的聚合服务，不是支付网关。**

A lightweight Rust protocol aggregation service for existing payment gateways.

This project only translates requests, routes orders to configured upstream providers, verifies their callbacks, and delivers normalized notifications. It does not provide acquiring, payment channels, funds custody, balances, settlement, payouts, or card-data collection. Actual checkout and payment processing belong to the upstream provider and the merchant's own account.

Initial implementation: Rust, SQLite, ePay, Waffo Pancake, JSON and Protocol Buffers over HTTP.
