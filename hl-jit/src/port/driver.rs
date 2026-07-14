//! The **driver-plugin seam**: attach one or more GPU/accelerator/display backends to a container launch
//! generically, `engine.add(Cuda::new(..))`-style, without the runtime learning what any of them is.
//!
//! [`Driver`] generalizes the single-backend [`DeviceProvider`](crate::DeviceProvider) into a plural,
//! ordered registry ([`Drivers`]): each driver still only ever hands the runtime a runtime-neutral
//! [`DeviceRequest`](crate::DeviceRequest) (mounts / env / render-node), so this stays purely about
//! **shim injection** — never GPU command semantics, which live in the backend's own ports (e.g. hl-gpu).
//!
//! This is a superset-compatible generalization: a `Driver` is a [`DeviceProvider`] that additionally
//! [names itself](Driver::name), and any existing `DeviceProvider` becomes a `Driver` unchanged via
//! [`ProviderDriver`] (or [`Drivers::add_provider`]). Today's single-provider path keeps working exactly
//! as before; this only makes the plural seam available.

use crate::runtime::{ContainerBuilder, DeviceProvider, DeviceRequest};

/// Something that can inject its device-integration needs into a container launch in runtime-neutral
/// terms. A superset of [`DeviceProvider`](crate::DeviceProvider): same
/// [`device_request`](Driver::device_request) contract, plus a [`name`](Driver::name) so a launch can
/// log/attribute which backends it attached. The implementor lives in the backend's own crate and holds
/// all backend-specific knowledge; the runtime only ever sees the [`DeviceRequest`] it returns.
///
/// `Send` so a registry of boxed drivers can move across threads with the launch it configures.
pub trait Driver: Send {
    /// Produce the mounts / env / render-node this backend needs for a launch. `guest_env` is the
    /// container's current merged guest environment (`K=V` lines), so a driver can compose against it —
    /// e.g. prepend its library dir to an existing `LD_LIBRARY_PATH`. Identical in meaning to
    /// [`DeviceProvider::device_request`](crate::DeviceProvider::device_request).
    fn device_request(&self, guest_env: &[String]) -> DeviceRequest;

    /// A short, stable identifier for this backend (e.g. `"cuda"`, `"gui"`) used only for
    /// logging/attribution — never for dispatch.
    fn name(&self) -> &str;
}

/// Adapts any existing [`DeviceProvider`](crate::DeviceProvider) into a [`Driver`] by pairing it with a
/// name, so backends that predate this seam plug into a [`Drivers`] registry unchanged.
pub struct ProviderDriver<P> {
    name: String,
    provider: P,
}

impl<P: DeviceProvider + Send> ProviderDriver<P> {
    /// Wrap `provider`, labelling it `name` for the [`Driver::name`] the registry reports.
    pub fn new(name: impl Into<String>, provider: P) -> Self {
        ProviderDriver { name: name.into(), provider }
    }
}

impl<P: DeviceProvider + Send> Driver for ProviderDriver<P> {
    fn device_request(&self, guest_env: &[String]) -> DeviceRequest {
        self.provider.device_request(guest_env)
    }
    fn name(&self) -> &str {
        &self.name
    }
}

/// An ordered registry of attached [`Driver`]s — the plural generalization of today's single provider.
///
/// Build it ergonomically (`drivers.add(Cuda::new(..))`), then either read each driver's request with
/// [`requests`](Drivers::requests) and fold them yourself, or hand the whole registry plus the evolving
/// guest env to [`apply`](Drivers::apply), which folds every driver into a [`ContainerBuilder`] exactly as
/// the launcher folds the single provider today (mounts + render-node via
/// [`apply_device`](ContainerBuilder::apply_device), then env appended and re-deduped through
/// [`guest_env`](ContainerBuilder::guest_env)). Empty = inert: zero drivers touch a launch at all.
#[derive(Default)]
pub struct Drivers(Vec<Box<dyn Driver>>);

impl Drivers {
    /// An empty registry — inert until drivers are [`add`](Drivers::add)ed.
    pub fn new() -> Self {
        Drivers(Vec::new())
    }

    /// Attach a driver, `engine.add(Cuda::new(..))`-style. Order is preserved: drivers apply in the order
    /// added, so a later driver's env can compose against an earlier one's. Returns `&mut Self` for
    /// chaining.
    pub fn add(&mut self, d: impl Driver + 'static) -> &mut Self {
        self.0.push(Box::new(d));
        self
    }

    /// Attach an existing [`DeviceProvider`](crate::DeviceProvider) under `name`, via [`ProviderDriver`],
    /// so pre-seam backends plug in without implementing [`Driver`] directly.
    pub fn add_provider(&mut self, name: impl Into<String>, p: impl DeviceProvider + Send + 'static) -> &mut Self {
        self.add(ProviderDriver::new(name, p))
    }

    /// Number of attached drivers.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// True when no drivers are attached (the launch stays device-free).
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The attached drivers' [`name`](Driver::name)s, in order — for logging/attribution.
    pub fn names(&self) -> Vec<&str> {
        self.0.iter().map(|d| d.name()).collect()
    }

    /// Each attached driver's [`DeviceRequest`] against the same `guest_env`, in registry order. The
    /// caller folds them into a launch (bind mounts, arm render-node, append env). Use [`apply`](Drivers::apply)
    /// for the ready-made fold that also lets a later driver's request compose against earlier ones' env.
    pub fn requests(&self, guest_env: &[String]) -> Vec<DeviceRequest> {
        self.0.iter().map(|d| d.device_request(guest_env)).collect()
    }

    /// Fold every attached driver into `builder`, mirroring how the launcher folds the single provider
    /// today: for each driver in order, ask it for a [`DeviceRequest`] against the current `env`, bind its
    /// mounts + arm its render-node via [`apply_device`](ContainerBuilder::apply_device), append its env to
    /// `env`, and re-apply [`guest_env`](ContainerBuilder::guest_env) so the added `K=V` lines go through
    /// the normal last-wins dedup. Because `env` accumulates across drivers, a later driver composes
    /// against earlier drivers' additions (e.g. a shared `LD_LIBRARY_PATH`). `tty` is forwarded to
    /// `guest_env`. Empty registry = `builder` and `env` returned untouched.
    pub fn apply(&self, mut builder: ContainerBuilder, env: &mut Vec<String>, tty: bool) -> ContainerBuilder {
        for d in &self.0 {
            let req = d.device_request(env);
            builder = builder.apply_device(&req);
            env.extend(req.env);
            builder = builder.guest_env(env, tty);
        }
        builder
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::DeviceMount;

    /// A minimal fake backend: contributes one mount and one env line, and can prove it saw the current
    /// guest env by echoing whether a probe key was present.
    struct FakeDriver {
        name: &'static str,
        mount: DeviceMount,
        env_line: String,
    }

    impl Driver for FakeDriver {
        fn device_request(&self, _guest_env: &[String]) -> DeviceRequest {
            DeviceRequest {
                mounts: vec![self.mount.clone()],
                env: vec![self.env_line.clone()],
                render_node: false,
            }
        }
        fn name(&self) -> &str {
            self.name
        }
    }

    #[test]
    fn registry_starts_empty_and_inert() {
        let d = Drivers::new();
        assert!(d.is_empty());
        assert_eq!(d.len(), 0);
        assert!(d.requests(&[]).is_empty());
        assert!(d.names().is_empty());
    }

    #[test]
    fn requests_returns_each_drivers_mounts_and_env_additively_in_order() {
        let mut drivers = Drivers::new();
        drivers
            .add(FakeDriver {
                name: "cuda",
                mount: DeviceMount::ro("/host/libcuda.so", "/usr/lib/libcuda.so"),
                env_line: "LD_LIBRARY_PATH=/usr/lib".into(),
            })
            .add(FakeDriver {
                name: "gui",
                mount: DeviceMount::rw("/host/wayland.sock", "/run/wayland.sock"),
                env_line: "WAYLAND_DISPLAY=wayland.sock".into(),
            });

        assert_eq!(drivers.len(), 2);
        assert_eq!(drivers.names(), vec!["cuda", "gui"]);

        let reqs = drivers.requests(&[]);
        assert_eq!(reqs.len(), 2);
        // First driver's request, unmerged.
        assert_eq!(reqs[0].mounts, vec![DeviceMount::ro("/host/libcuda.so", "/usr/lib/libcuda.so")]);
        assert_eq!(reqs[0].env, vec!["LD_LIBRARY_PATH=/usr/lib".to_string()]);
        // Second driver's request, in order.
        assert_eq!(reqs[1].mounts, vec![DeviceMount::rw("/host/wayland.sock", "/run/wayland.sock")]);
        assert_eq!(reqs[1].env, vec!["WAYLAND_DISPLAY=wayland.sock".to_string()]);
    }

    #[test]
    fn add_provider_adapts_a_bare_device_provider() {
        // A backend that predates the Driver seam: only a DeviceProvider, no name().
        struct BareProvider;
        impl DeviceProvider for BareProvider {
            fn device_request(&self, _guest_env: &[String]) -> DeviceRequest {
                DeviceRequest { env: vec!["FROM_PROVIDER=1".into()], ..Default::default() }
            }
        }

        let mut drivers = Drivers::new();
        drivers.add_provider("legacy", BareProvider);
        assert_eq!(drivers.names(), vec!["legacy"]);
        assert_eq!(drivers.requests(&[])[0].env, vec!["FROM_PROVIDER=1".to_string()]);
    }
}
