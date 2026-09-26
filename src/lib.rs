pub mod api;
pub mod config;
pub mod crypto;
pub mod error;
pub mod gateway;
pub mod money;
pub mod network;
pub mod providers;
pub mod service;
pub mod store;

pub mod wire {
    include!(concat!(env!("OUT_DIR"), "/pay.v1.rs"));
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
