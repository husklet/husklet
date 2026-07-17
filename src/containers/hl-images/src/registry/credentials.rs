//! Registry credentials as sent by the CLI in the `X-Registry-Auth` header.

use super::*;
use serde_json::Value;

/// Credentials for a registry, as sent by the CLI in the `X-Registry-Auth` header.
#[derive(Clone, Default)]
pub struct Credentials {
    /// Registry account username (empty for anonymous access).
    pub username: String,
    /// Registry account password or access token (empty for anonymous access).
    pub password: String,
}
impl Credentials {
    /// Anonymous credentials (public registries).
    pub fn none() -> Credentials {
        Credentials::default()
    }

    /// Decode docker's base64-encoded `X-Registry-Auth` JSON (`{username,password,...}`).
    pub fn from_x_registry_auth(b64: &str) -> Option<Credentials> {
        let json = base64_decode(b64.trim())?;
        let v: Value = serde_json::from_slice(&json).ok()?;
        Some(Credentials {
            username: v["username"].as_str().unwrap_or_default().to_string(),
            password: v["password"].as_str().unwrap_or_default().to_string(),
        })
    }
    pub(super) fn is_empty(&self) -> bool {
        self.username.is_empty() && self.password.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // base64 of {"username":"alice","password":"s3cret"}
    const VALID: &str = "eyJ1c2VybmFtZSI6ICJhbGljZSIsICJwYXNzd29yZCI6ICJzM2NyZXQifQ==";
    // base64 of {"password":"p"}  (no username key)
    const NO_USER: &str = "eyJwYXNzd29yZCI6ICJwIn0=";
    // base64 of {}  (neither key)
    const EMPTY_OBJ: &str = "e30=";
    // base64 of the bytes "not json at all" (valid base64, invalid JSON)
    const NON_JSON: &str = "bm90IGpzb24gYXQgYWxs";

    #[test]
    fn decodes_username_and_password() {
        let c = Credentials::from_x_registry_auth(VALID).expect("valid auth header decodes");
        assert_eq!(c.username, "alice");
        assert_eq!(c.password, "s3cret");
        assert!(!c.is_empty());
    }

    #[test]
    fn missing_keys_fall_back_to_empty_strings() {
        // Missing `username` key -> empty string (unwrap_or_default), password preserved.
        let c = Credentials::from_x_registry_auth(NO_USER).expect("valid JSON decodes");
        assert_eq!(c.username, "");
        assert_eq!(c.password, "p");
        // Both keys absent -> a fully-empty (anonymous-equivalent) credential, still Some.
        let e = Credentials::from_x_registry_auth(EMPTY_OBJ).expect("empty object decodes");
        assert!(e.is_empty());
    }

    #[test]
    fn leading_trailing_whitespace_is_trimmed() {
        let c = Credentials::from_x_registry_auth(&format!("  {VALID}\n")).expect("trims ws");
        assert_eq!(c.username, "alice");
    }

    #[test]
    fn invalid_base64_is_none() {
        // `!` is outside the base64 alphabet.
        assert!(Credentials::from_x_registry_auth("not valid base64 !!!").is_none());
    }

    #[test]
    fn valid_base64_but_non_json_is_none() {
        assert!(Credentials::from_x_registry_auth(NON_JSON).is_none());
    }
}
