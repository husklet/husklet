//! The connection handshake bytes: the host executor's serialized capability descriptor.
//!
//! On connect the host advertises its [`Capabilities`] as a length-prefixed frame (`[u32 len][body]`, the
//! form `protocol::codec` already produces). The guest decodes it and negotiates its required
//! [`FeatureRequest`](crate::protocol::model::capability::FeatureRequest) against it BEFORE advertising any
//! matching API feature to the app — the "negotiate before advertise" contract ported from `hl-shim`'s
//! `negotiate_host_capabilities`. This module is a thin, transport-owned view over the protocol codec: the
//! handshake bytes themselves are 100% protocol, the transport only decides *when* they cross the wire.

#[cfg(test)]
use crate::protocol::model::capability::Capabilities;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_roundtrips_through_protocol_codec() {
        let caps = Capabilities::permissive_fixture("transport-host");
        let bytes = caps.to_handshake();
        assert_eq!(Capabilities::from_handshake(&bytes).unwrap(), caps);
    }
}
