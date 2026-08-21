use serde::{Deserialize, Serialize};

/// Credentials accepted by Docker's registry authentication probe.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Credentials {
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub serveraddress: String,
    #[serde(default)]
    #[serde(rename = "identitytoken")]
    pub identity_token: String,
}

#[cfg(feature = "runtime")]
impl Credentials {
    pub(crate) fn decode(value: &str) -> Result<Self, String> {
        use base64::Engine as _;
        let value = value.trim();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(value)
            .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(value))
            .map_err(|_| "invalid X-Registry-Auth encoding".to_owned())?;
        serde_json::from_slice(&bytes).map_err(|_| "invalid X-Registry-Auth JSON".to_owned())
    }

    pub(crate) fn auth(self) -> Result<hl_images::remote::Auth, String> {
        use hl_images::remote::Auth;
        if !self.identity_token.is_empty() {
            if !self.username.is_empty() || !self.password.is_empty() {
                return Err("X-Registry-Auth cannot combine identitytoken with username/password".into());
            }
            Ok(Auth::Bearer(self.identity_token))
        } else if !self.username.is_empty() || !self.password.is_empty() {
            if self.username.is_empty() {
                return Err("X-Registry-Auth password requires username".into());
            }
            Ok(Auth::Basic {
                username: self.username,
                password: self.password,
            })
        } else {
            Ok(Auth::Anonymous)
        }
    }
}

/// Result of validating registry credentials.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct Authentication {
    pub status: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub identity_token: String,
}

// These exercise `Credentials::decode`, which the `runtime` feature gates, so the
// module needs the same gate -- the shape `api/model/filesystem.rs` already uses.
// Without it `cargo check -p hl-daemon --no-default-features --all-targets` fails in
// the LIB TEST, which no `required-features` key can express.
#[cfg(all(test, feature = "runtime"))]
mod tests {
    use super::Credentials;
    use base64::Engine as _;

    fn registry_auth(value: &serde_json::Value) -> String {
        base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&value).unwrap())
    }

    #[test]
    fn decodes_username_and_password() {
        let auth = registry_auth(&serde_json::json!({"username":"alice","password":"s3cret"}));
        let credentials = Credentials::decode(&auth).unwrap();
        assert_eq!(credentials.username, "alice");
        assert_eq!(credentials.password, "s3cret");
    }

    #[test]
    fn missing_keys_fall_back_to_empty_strings() {
        let credentials = Credentials::decode(&registry_auth(&serde_json::json!({"password":"p"}))).unwrap();
        assert!(credentials.username.is_empty());
        assert_eq!(credentials.password, "p");
        assert_eq!(
            Credentials::decode(&registry_auth(&serde_json::json!({}))).unwrap(),
            Credentials::default()
        );
    }

    #[test]
    fn leading_trailing_whitespace_is_trimmed() {
        let auth = registry_auth(&serde_json::json!({"username":"alice"}));
        assert_eq!(Credentials::decode(&format!("  {auth}\n")).unwrap().username, "alice");
    }

    #[test]
    fn invalid_base64_is_none() {
        assert!(Credentials::decode("not valid base64 !!!").is_err());
    }

    #[test]
    fn accepts_unpadded_url_safe_base64() {
        let bytes = serde_json::to_vec(&serde_json::json!({"username":"alice"})).unwrap();
        let auth = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        assert_eq!(Credentials::decode(&auth).unwrap().username, "alice");
    }

    #[test]
    fn valid_base64_but_non_json_is_none() {
        let auth = base64::engine::general_purpose::STANDARD.encode("not json at all");
        assert!(Credentials::decode(&auth).is_err());
    }
}
