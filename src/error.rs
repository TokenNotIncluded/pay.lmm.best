use axum::http::StatusCode;

#[derive(Debug, Clone, Copy)]
pub struct Error {
    pub status: StatusCode,
    pub code: &'static str,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub const fn new(status: StatusCode, code: &'static str) -> Self { Self { status, code } }
    pub const fn invalid(code: &'static str) -> Self { Self::new(StatusCode::UNPROCESSABLE_ENTITY, code) }
    pub const fn conflict(code: &'static str) -> Self { Self::new(StatusCode::CONFLICT, code) }
    pub const fn unauthorized() -> Self { Self::new(StatusCode::UNAUTHORIZED, "unauthorized") }
    pub const fn upstream() -> Self { Self::new(StatusCode::BAD_GATEWAY, "upstream_result_unknown") }
    pub const fn internal() -> Self { Self::new(StatusCode::INTERNAL_SERVER_ERROR, "internal_error") }
    pub const fn not_found() -> Self { Self::new(StatusCode::NOT_FOUND, "not_found") }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code) }
}
impl std::error::Error for Error {}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self { Self::new(StatusCode::SERVICE_UNAVAILABLE, "database_unavailable") }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self { Self::internal() }
}
pub fn require(ok: bool, code: &'static str) -> Result<()> {
    if ok { Ok(()) } else { Err(Error::invalid(code)) }
}
pub fn identifier(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && s.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-.:".contains(&c))
}
