use std::collections::BTreeMap;
use base64::{Engine, engine::general_purpose::STANDARD};
use md5::{Digest, Md5};
use ring::{digest, hmac, rand::{SecureRandom, SystemRandom}, rsa, signature};
use spki::der::Decode;
use subtle::ConstantTimeEq;
use zeroize::Zeroizing;
use crate::error::{Error, Result, require};

pub fn hash(data: &[u8]) -> String { hex::encode(digest::digest(&digest::SHA256, data).as_ref()) }
pub fn equal(a: &[u8], b: &[u8]) -> bool { bool::from(a.ct_eq(b)) }
pub fn random_id(prefix: &str) -> Result<String> {
    let mut bytes = [0u8; 16];
    SystemRandom::new().fill(&mut bytes).map_err(|_| Error::internal())?;
    Ok(format!("{prefix}{}", hex::encode(bytes)))
}
pub fn secret(name: &str) -> anyhow::Result<Zeroizing<String>> {
    anyhow::ensure!(!name.is_empty(), "secret environment variable name is required");
    let value = std::env::var(name).map_err(|_| anyhow::anyhow!("required secret environment variable is missing: {name}"))?;
    anyhow::ensure!(!value.trim().is_empty(), "secret environment variable is empty: {name}");
    Ok(Zeroizing::new(value))
}
// Legacy MD5 is confined to ePay compatibility. Never used for our authentication.
pub fn epay_signature(values: &BTreeMap<String, String>, key: &str) -> String {
    let joined = values.iter().filter(|(k,v)| k.as_str() != "sign" && k.as_str() != "sign_type" && !v.is_empty())
        .map(|(k,v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let mut h = Md5::new(); h.update(joined.as_bytes()); h.update(key.as_bytes());
    hex::encode(h.finalize())
}
pub fn private_key(value: &str) -> anyhow::Result<rsa::KeyPair> {
    let normalized = Zeroizing::new(value.replace("\\n", "\n"));
    let parsed = pem::parse(normalized.as_bytes()).map_err(|_| anyhow::anyhow!("invalid private key PEM"))?;
    let key = match parsed.tag() {
        "PRIVATE KEY" => rsa::KeyPair::from_pkcs8(parsed.contents()),
        "RSA PRIVATE KEY" => rsa::KeyPair::from_der(parsed.contents()),
        _ => anyhow::bail!("expected PKCS#8 or PKCS#1 RSA private key"),
    }.map_err(|_| anyhow::anyhow!("unsupported RSA private key; require at least 2048 bits"))?;
    Ok(key)
}
pub fn public_key(value: &str) -> anyhow::Result<Vec<u8>> {
    let parsed = pem::parse(value.replace("\\n", "\n")).map_err(|_| anyhow::anyhow!("invalid public key PEM"))?;
    match parsed.tag() {
        "RSA PUBLIC KEY" => Ok(parsed.contents().to_vec()),
        "PUBLIC KEY" => {
            let info = spki::SubjectPublicKeyInfoRef::from_der(parsed.contents()).map_err(|_| anyhow::anyhow!("invalid SPKI key"))?;
            anyhow::ensure!(info.algorithm.oid.to_string() == "1.2.840.113549.1.1.1", "expected RSA public key");
            Ok(info.subject_public_key.as_bytes().ok_or_else(|| anyhow::anyhow!("invalid RSA bit string"))?.to_vec())
        }
        _ => anyhow::bail!("expected RSA public key PEM"),
    }
}
pub fn rsa_sign(key: &rsa::KeyPair, data: &[u8]) -> Result<String> {
    let mut sig = vec![0; key.public().modulus_len()];
    key.sign(&signature::RSA_PKCS1_SHA256, &SystemRandom::new(), data, &mut sig).map_err(|_| Error::internal())?;
    Ok(STANDARD.encode(sig))
}
pub fn waffo_request_signature(key: &rsa::KeyPair, path: &str, timestamp: &str, body: &[u8]) -> Result<String> {
    let body_hash = STANDARD.encode(digest::digest(&digest::SHA256, body).as_ref());
    rsa_sign(key, format!("POST\n{path}\n{timestamp}\n{body_hash}").as_bytes())
}
pub fn verify_waffo(key: &[u8], header: &str, body: &[u8], now_ms: i64) -> Result<()> {
    require(header.len() <= 4096, "invalid_signature_header")?;
    let mut timestamp = None; let mut signature_b64 = None;
    for item in header.split(',') {
        let (k, v) = item.trim().split_once('=').ok_or(Error::unauthorized())?;
        match k {
            "t" if timestamp.is_none() => timestamp = Some(v),
            "v1" if signature_b64.is_none() => signature_b64 = Some(v),
            _ => return Err(Error::unauthorized()),
        }
    }
    let t = timestamp.ok_or(Error::unauthorized())?;
    require(t.bytes().all(|b| b.is_ascii_digit()), "invalid_timestamp")?;
    let ts = t.parse::<i64>().map_err(|_| Error::unauthorized())?;
    let age = now_ms.checked_sub(ts).ok_or(Error::unauthorized())?;
    if !(-60_000..=2_700_000).contains(&age) { return Err(Error::unauthorized()); }
    let sig = STANDARD.decode(signature_b64.ok_or(Error::unauthorized())?).map_err(|_| Error::unauthorized())?;
    let mut input = Vec::with_capacity(t.len() + 1 + body.len());
    input.extend_from_slice(t.as_bytes()); input.push(b'.'); input.extend_from_slice(body);
    signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, key)
        .verify(&input, &sig).map_err(|_| Error::unauthorized())
}
pub fn notification_signature(key: &hmac::Key, timestamp: i64, body: &[u8]) -> String {
    let mut context = hmac::Context::with_key(key);
    context.update(timestamp.to_string().as_bytes()); context.update(b"."); context.update(body);
    format!("t={timestamp},v1={}", hex::encode(context.sign().as_ref()))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn epay_known_vector() {
        let v = BTreeMap::from([("b".into(), "2".into()), ("a".into(), "1".into()), ("sign_type".into(), "MD5".into())]);
        assert_eq!(epay_signature(&v, "secret"), hex::encode(Md5::digest(b"a=1&b=2secret")));
        let mut different = v.clone(); different.insert("a".into(), "3".into());
        assert_ne!(epay_signature(&v,"secret"), epay_signature(&different,"secret"));
    }
    #[test]
    fn duplicate_or_old_waffo_signatures_rejected() {
        assert!(verify_waffo(&[], "t=1,t=1,v1=AA==", b"{}", 1).is_err());
        assert!(verify_waffo(&[], "t=1,v1=AA==", b"{}", 3_000_000).is_err());
    }
    #[test]
    fn hmac_matches_independent_input() {
        let key = hmac::Key::new(hmac::HMAC_SHA256, b"test-secret");
        let expected = hmac::sign(&key, b"123.{}");
        assert_eq!(notification_signature(&key, 123, b"{}"), format!("t=123,v1={}",hex::encode(expected.as_ref())));
    }
}
