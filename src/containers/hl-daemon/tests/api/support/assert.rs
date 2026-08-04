//! Assertion helper shared by daemon integration tests.

pub(crate) fn require(value: bool, message: &str) -> Result<(), Box<dyn std::error::Error>> {
    if value { Ok(()) } else { Err(message.into()) }
}
