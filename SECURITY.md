# Security policy

This repository is a payment-gateway **aggregation service**, not a payment gateway or a funds-handling service. It is an initial implementation and has not received an independent security audit.

Read [the security contract](docs/security.md) before deployment. Never send real merchant private keys, API keys, card data or unsanitized production callbacks in public issues.

For a suspected vulnerability, use GitHub private vulnerability reporting when enabled, or contact the organization maintainers privately. Public issues are appropriate only for non-sensitive bugs with synthetic, redacted reproductions. Do not include exploitable production details before maintainers have had an opportunity to assess them.

The initial 0.1 series is the only implementation line maintained here. Deploy only a commit whose CI you have reviewed and run sandbox acceptance tests for your exact upstream gateway configuration.
