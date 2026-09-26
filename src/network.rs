use std::{io, net::IpAddr, sync::Arc, time::Duration};
use reqwest::{Client, RequestBuilder, dns::{Addrs, Name, Resolve, Resolving}};
use url::Url;
use crate::error::{Error, Result, require};

// Conservative public-address policy. Special-use and transition ranges are denied.
pub fn allowed_ip(ip: IpAddr, allow_loopback: bool) -> bool {
    if allow_loopback && ip.is_loopback() { return true; }
    match ip {
        IpAddr::V4(v) => {
            let [a,b,c,_] = v.octets();
            !(a == 0 || a == 10 || a == 127 || a >= 224 || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254) || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || (b == 0 && (c == 0 || c == 2)) || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19 || (b == 51 && c == 100))) || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() { return allowed_ip(IpAddr::V4(v4), allow_loopback); }
            let s = v.segments();
            (s[0] & 0xe000) == 0x2000 && s[0] != 0x2002
                && !(s[0] == 0x2001 && (s[1] < 0x200 || s[1] == 0xdb8))
                && !(s[0] == 0x3fff && s[1] < 0x1000)
        }
    }
}
pub fn safe_url(value: &str, dev: bool) -> Result<Url> {
    require(value.len() <= 8192, "url_too_long")?;
    let u = Url::parse(value).map_err(|_| Error::invalid("invalid_url"))?;
    require(u.username().is_empty() && u.password().is_none() && u.fragment().is_none(), "url_credentials_or_fragment_forbidden")?;
    let host = u.host_str().ok_or(Error::invalid("url_host_required"))?;
    let host = host.trim_matches(['[',']']);
    let ip = host.parse::<IpAddr>().ok();
    let loopback = host == "localhost" || ip.is_some_and(|v|v.is_loopback());
    require(u.scheme() == "https" || (dev && loopback && u.scheme() == "http"), "https_required")?;
    if let Some(ip) = ip { require(allowed_ip(ip,dev), "private_address_forbidden")?; }
    if host == "localhost" || host.ends_with(".localhost") { require(dev && host == "localhost", "private_address_forbidden")?; }
    Ok(u)
}
pub fn checkout_url(value: &str, origins: &[String], dev: bool) -> Result<String> {
    let u = safe_url(value, dev)?;
    let origin = u.origin().ascii_serialization();
    require(origins.iter().any(|s|Url::parse(s).is_ok_and(|a|a.origin().ascii_serialization()==origin)), "checkout_origin_not_allowed")?;
    Ok(u.into())
}
#[derive(Debug)]
struct PublicDns { allow_loopback: bool }
impl Resolve for PublicDns {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_owned(); let dev = self.allow_loopback;
        Box::pin(async move {
            let addresses: Vec<_> = tokio::net::lookup_host((host.as_str(), 0)).await?.take(32).collect();
            if addresses.is_empty() || addresses.iter().any(|a| !allowed_ip(a.ip(), dev)) {
                return Err(Box::new(io::Error::new(io::ErrorKind::PermissionDenied, "non-public DNS answer denied")) as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}
#[derive(Clone)]
pub struct SafeClient { client: Client, dev: bool, max_response: usize }
impl SafeClient {
    pub fn new(dev: bool, max_response: usize) -> anyhow::Result<Self> {
        let client = Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .dns_resolver(Arc::new(PublicDns{allow_loopback:dev}))
            .connect_timeout(Duration::from_secs(3)).timeout(Duration::from_secs(15))
            .pool_max_idle_per_host(1).pool_idle_timeout(Duration::from_secs(30))
            .min_tls_version(reqwest::tls::Version::TLS_1_2).build()?;
        Ok(Self{client,dev,max_response})
    }
    pub fn post(&self, url: &str) -> Result<RequestBuilder> { Ok(self.client.post(safe_url(url,self.dev)?)) }
    pub async fn execute(&self, request: RequestBuilder) -> Result<(axum::http::StatusCode,Vec<u8>)> {
        let mut response = request.send().await.map_err(|_|Error::upstream())?;
        let status = response.status();
        if response.content_length().is_some_and(|n|n > self.max_response as u64) { return Err(Error::upstream()); }
        let mut data = Vec::with_capacity(1024.min(self.max_response));
        while let Some(chunk) = response.chunk().await.map_err(|_|Error::upstream())? {
            if data.len().saturating_add(chunk.len()) > self.max_response { return Err(Error::upstream()); }
            data.extend_from_slice(&chunk);
        }
        Ok((status,data))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ssrf_boundaries() {
        for ip in ["127.0.0.1","10.0.0.1","169.254.169.254","100.64.0.1","192.168.1.1","0.0.0.0","::1","fc00::1","fe80::1","::ffff:127.0.0.1","2002:7f00:1::1"] {
            assert!(!allowed_ip(ip.parse().unwrap(),false), "{ip}");
        }
        assert!(allowed_ip("8.8.8.8".parse().unwrap(),false));
        assert!(allowed_ip("2606:4700:4700::1111".parse().unwrap(),false));
        assert!(safe_url("https://127.0.0.1/",false).is_err());
        assert!(safe_url("http://localhost/",true).is_ok());
        assert!(safe_url("http://10.0.0.1/",true).is_err());
        assert!(safe_url("https://user:secret@example.com",false).is_err());
        assert!(checkout_url("https://evil.example/", &["https://good.example".into()],false).is_err());
    }
}
