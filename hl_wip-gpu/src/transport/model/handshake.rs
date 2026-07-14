//! The connection handshake bytes: the host executor's serialized capability descriptor.
//!
//! On connect the host advertises its [`Capabilities`] as a length-prefixed frame (`[u32 len][body]`, the
//! form `protocol::codec` already produces). The guest decodes it and negotiates its required
//! [`FeatureRequest`](crate::protocol::model::capability::FeatureRequest) against it BEFORE advertising any
//! matching API feature to the app — the "negotiate before advertise" contract ported from `hl-shim`'s
//! `negotiate_host_capabilities`. This module is a thin, transport-owned view over the protocol codec: the
//! handshake bytes themselves are 100% protocol, the transport only decides *when* they cross the wire.

use crate::protocol::model::capability::Capabilities;
use crate::protocol::model::error::Result;

/// Serialize the host's capability advertisement into the handshake frame (`[u32 len][body]`).
pub fn encode_handshake(caps: &Capabilities) -> Vec<u8> {
    caps.to_handshake()
}

/// Decode a handshake frame (`[u32 len][body]`, as produced by [`encode_handshake`]) back to the
/// advertised [`Capabilities`].
pub fn decode_handshake(bytes: &[u8]) -> Result<Capabilities> {
    Capabilities::from_handshake(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_roundtrips_through_protocol_codec() {
        let caps = Capabilities::full("transport-host");
        let bytes = encode_handshake(&caps);
        assert_eq!(decode_handshake(&bytes).unwrap(), caps);
    }
}
