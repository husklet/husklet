//! Generated client and server bindings for Husklet's private surface identity protocol.

pub mod buffer;

pub const VERSION: u32 = 1;

/// C ABI metadata for marshalling the protocol through an application-owned `libwayland-client`.
///
/// Generated Rust proxies cannot adopt an arbitrary existing `wl_display` connection. Dynamic clients
/// therefore pass these descriptors to `wl_proxy_marshal_flags`, exactly as generated Wayland C code
/// would. All strings and descriptor storage remain valid for the lifetime of [`ClientInterfaces`].
pub mod raw {
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    pub struct Message {
        pub name: *const c_char,
        pub signature: *const c_char,
        pub types: *const *const Interface,
    }

    #[repr(C)]
    pub struct Interface {
        pub name: *const c_char,
        pub version: c_int,
        pub method_count: c_int,
        pub methods: *const Message,
        pub event_count: c_int,
        pub events: *const Message,
    }

    /// Owned, pointer-stable protocol descriptors.
    ///
    /// This value is intentionally not `Send` or `Sync`: it contains raw pointers consumed only while
    /// marshalling on the Wayland connection that supplied `wl_surface_interface`.
    #[allow(
        dead_code,
        reason = "boxes own storage referenced by the exported C descriptors"
    )]
    pub struct ClientInterfaces {
        identity_requests: Box<[Message; 2]>,
        identity_events: Box<[Message; 1]>,
        identity: Box<Interface>,
        manager_types: Box<[*const Interface; 2]>,
        manager_requests: Box<[Message; 1]>,
        manager: Box<Interface>,
    }

    impl ClientInterfaces {
        /// Build metadata using the `wl_surface_interface` exported by the exact libwayland instance
        /// whose proxies will be marshalled.
        ///
        /// `wl_surface_interface` must point to a process-lifetime `wl_interface` with the C layout
        /// represented by [`Interface`]. `libwayland-client` exports such storage statically.
        ///
        /// # Safety
        ///
        /// The caller must keep the pointed-to static libwayland descriptor loaded for this value's
        /// lifetime and marshal the returned descriptors through that same libwayland ABI.
        pub unsafe fn new(wl_surface_interface: *const c_void) -> Option<Self> {
            let wl_surface_interface = wl_surface_interface.cast::<Interface>();
            if wl_surface_interface.is_null() {
                return None;
            }

            let identity_requests = Box::new([
                Message {
                    name: c"associate".as_ptr(),
                    signature: c"uu".as_ptr(),
                    types: std::ptr::null(),
                },
                Message {
                    name: c"destroy".as_ptr(),
                    signature: c"".as_ptr(),
                    types: std::ptr::null(),
                },
            ]);
            let identity_events = Box::new([Message {
                name: c"token".as_ptr(),
                signature: c"uu".as_ptr(),
                types: std::ptr::null(),
            }]);
            let identity = Box::new(Interface {
                name: c"hl_surface_identity_v1".as_ptr(),
                version: 1,
                method_count: identity_requests.len() as c_int,
                methods: identity_requests.as_ptr(),
                event_count: identity_events.len() as c_int,
                events: identity_events.as_ptr(),
            });
            let manager_types =
                Box::new([std::ptr::from_ref(identity.as_ref()), wl_surface_interface]);
            let manager_requests = Box::new([Message {
                name: c"get_surface".as_ptr(),
                signature: c"no".as_ptr(),
                types: manager_types.as_ptr(),
            }]);
            let manager = Box::new(Interface {
                name: c"hl_surface_manager_v1".as_ptr(),
                version: 1,
                method_count: manager_requests.len() as c_int,
                methods: manager_requests.as_ptr(),
                event_count: 0,
                events: std::ptr::null(),
            });

            Some(Self {
                identity_requests,
                identity_events,
                identity,
                manager_types,
                manager_requests,
                manager,
            })
        }

        pub fn manager(&self) -> *const c_void {
            std::ptr::from_ref(self.manager.as_ref()).cast()
        }

        pub fn identity(&self) -> *const c_void {
            std::ptr::from_ref(self.identity.as_ref()).cast()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::ffi::CStr;

        fn text(value: *const c_char) -> &'static [u8] {
            // SAFETY: every tested pointer is owned by the live descriptor under test.
            unsafe { CStr::from_ptr(value) }.to_bytes()
        }

        #[test]
        fn descriptors_follow_xml_opcode_order_and_signatures() {
            let wl_surface = Interface {
                name: c"wl_surface".as_ptr(),
                version: 6,
                method_count: 0,
                methods: std::ptr::null(),
                event_count: 0,
                events: std::ptr::null(),
            };
            // SAFETY: the stack descriptor outlives `interfaces`.
            let interfaces =
                unsafe { ClientInterfaces::new(std::ptr::from_ref(&wl_surface).cast()) }
                    .expect("metadata");

            assert_eq!(text(interfaces.manager.name), b"hl_surface_manager_v1");
            assert_eq!(interfaces.manager.version, 1);
            assert_eq!(interfaces.manager.method_count, 1);
            assert_eq!(text(interfaces.manager_requests[0].name), b"get_surface");
            assert_eq!(text(interfaces.manager_requests[0].signature), b"no");
            assert_eq!(
                interfaces.manager_types[0],
                std::ptr::from_ref(interfaces.identity.as_ref())
            );
            assert_eq!(interfaces.manager_types[1], std::ptr::from_ref(&wl_surface));

            assert_eq!(text(interfaces.identity.name), b"hl_surface_identity_v1");
            assert_eq!(interfaces.identity.method_count, 2);
            assert_eq!(text(interfaces.identity_requests[0].name), b"associate");
            assert_eq!(text(interfaces.identity_requests[0].signature), b"uu");
            assert_eq!(text(interfaces.identity_requests[1].name), b"destroy");
            assert_eq!(text(interfaces.identity_requests[1].signature), b"");
            assert_eq!(interfaces.identity.event_count, 1);
            assert_eq!(text(interfaces.identity_events[0].name), b"token");
            assert_eq!(text(interfaces.identity_events[0].signature), b"uu");
        }
    }
}

#[cfg(feature = "bindings")]
#[allow(
    clippy::wildcard_imports,
    reason = "wayland-scanner requires protocol and generated interface glob imports"
)]
pub mod client {
    use wayland_client;
    use wayland_client::protocol::*;

    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocol/hl-surface-v1.xml");
    }

    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("protocol/hl-surface-v1.xml");
}

#[cfg(feature = "bindings")]
#[allow(
    clippy::wildcard_imports,
    reason = "wayland-scanner requires protocol and generated interface glob imports"
)]
pub mod server {
    use wayland_server;
    use wayland_server::protocol::*;

    pub mod __interfaces {
        use wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocol/hl-surface-v1.xml");
    }

    use self::__interfaces::*;
    wayland_scanner::generate_server_code!("protocol/hl-surface-v1.xml");
}
