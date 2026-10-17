//! Account login: cookie capture (embedded WebKitGTK), persistence (keyring with a file
//! fallback), and building an authenticated or anonymous `Session`.
use std::sync::Arc;

use innertube::{ClientConfig, Credentials, Session};

pub mod login;
pub mod store;

/// True if `header` can authenticate (parses + has a `SAPISID`). Used to decide
/// authenticated vs anonymous and to reject a corrupted stored value.
pub fn cookie_is_usable(header: &str) -> bool {
    Credentials::from_cookie_header(header).is_ok()
}

/// Build a session from an optional cookie header: authenticated when the cookie is
/// usable, anonymous otherwise. The error is the HTTP client failing to build.
pub fn build_session(cookie: Option<&str>) -> Result<Arc<Session>, innertube::Error> {
    if let Some(header) = cookie {
        if let Ok(creds) = Credentials::from_cookie_header(header) {
            if let Ok(session) = Session::authenticated(ClientConfig::web(), creds) {
                return Ok(Arc::new(session));
            }
        }
    }
    Ok(Arc::new(Session::new(ClientConfig::web())?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unusable_cookie_builds_an_anonymous_session() {
        assert!(!build_session(None).unwrap().is_authenticated());
        assert!(!build_session(Some("garbage")).unwrap().is_authenticated());
        assert!(build_session(Some("SAPISID=abc123")).unwrap().is_authenticated());
    }
}
