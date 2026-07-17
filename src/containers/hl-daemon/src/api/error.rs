//! Error-response DTO.
//!
//! Docker's error bodies are a single lowercase `{"message": …}` object. This
//! `#[derive(Serialize)]` struct replaces the inline `json!({"message": …})`
//! builders in `util::http`; the field name is already lowercase, so no
//! `#[serde(rename)]` is needed and the serialized bytes are unchanged.

use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct ErrorMessage {
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_message_is_lowercase_message_key() {
        assert_eq!(
            serde_json::to_value(ErrorMessage {
                message: "x".into()
            })
            .unwrap(),
            serde_json::json!({"message": "x"})
        );
    }
}
