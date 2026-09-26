use crate::error::{Error, Result, require};

pub const MAX_AMOUNT: i64 = 1_000_000_000_000;
pub fn exponent(currency: &str) -> Result<usize> {
    match currency {
        "JPY" | "KRW" => Ok(0),
        "USD" | "CNY" | "EUR" | "GBP" | "HKD" | "SGD" | "CAD" | "AUD" | "CHF" | "TWD" => Ok(2),
        _ => Err(Error::invalid("unsupported_currency")),
    }
}
pub fn decimal(amount: i64, currency: &str) -> Result<String> {
    require((0..=MAX_AMOUNT).contains(&amount), "invalid_amount")?;
    let digits = exponent(currency)?;
    let unit = 10_i64.pow(digits as u32);
    if digits == 0 {
        Ok(amount.to_string())
    } else {
        Ok(format!(
            "{}.{:0width$}",
            amount / unit,
            amount % unit,
            width = digits
        ))
    }
}
pub fn minor(s: &str, currency: &str) -> Result<i64> {
    let digits = exponent(currency)?;
    require(!s.is_empty() && s.len() <= 24, "invalid_amount")?;
    let mut parts = s.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next();
    require(
        parts.next().is_none() && !whole.is_empty() && whole.bytes().all(|b| b.is_ascii_digit()),
        "invalid_amount",
    )?;
    let f = fraction.unwrap_or("");
    require(
        f.len() <= digits
            && f.bytes().all(|b| b.is_ascii_digit())
            && (fraction.is_none() || !f.is_empty()),
        "invalid_precision",
    )?;
    let unit = 10_i64.pow(digits as u32);
    let w: i64 = whole
        .parse()
        .map_err(|_| Error::invalid("invalid_amount"))?;
    let tail: i64 = if f.is_empty() {
        0
    } else {
        f.parse().map_err(|_| Error::invalid("invalid_amount"))?
    };
    let amount = w
        .checked_mul(unit)
        .and_then(|v| v.checked_add(tail * 10_i64.pow((digits - f.len()) as u32)))
        .ok_or(Error::invalid("amount_overflow"))?;
    require(amount <= MAX_AMOUNT, "invalid_amount")?;
    Ok(amount)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_money() {
        assert_eq!(minor("0.29", "USD").unwrap(), 29);
        assert_eq!(minor("12.3", "USD").unwrap(), 1230);
        assert_eq!(decimal(1230, "USD").unwrap(), "12.30");
        assert_eq!(minor("150", "JPY").unwrap(), 150);
        assert_eq!(decimal(150, "JPY").unwrap(), "150");
    }
    #[test]
    fn rejects_lossy_and_malformed_amounts() {
        for s in [
            "-1",
            "+1",
            "NaN",
            "1e2",
            "1.001",
            "1.",
            ".1",
            " 1",
            "1.2.3",
            "99999999999999999999999",
        ] {
            assert!(minor(s, "USD").is_err(), "{s}");
        }
        assert!(minor("1.0", "JPY").is_err());
        assert!(minor("1", "XYZ").is_err());
    }
}
