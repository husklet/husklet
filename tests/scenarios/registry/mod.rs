//! Scenario catalog assembly: test-data groups plus the registry builder.

pub(crate) mod build;
pub(crate) mod copy;
pub(crate) mod dockernet;
pub(crate) mod dockervol;
pub(crate) mod netinstall;
pub(crate) mod networking;
pub(crate) mod toolchains;
pub(crate) mod volume;

pub(crate) fn build() -> crate::contract::Registry {
    let mut registry = crate::contract::Registry::default();
    registry.add(build::group());
    registry.add(copy::group());
    registry.add(crate::databases::group());
    registry.add(dockervol::group());
    registry.add(dockernet::group());
    registry.add(networking::group());
    registry.add(netinstall::group());
    registry.add(toolchains::group());
    registry.add(volume::group());
    registry.add(crate::distros::group());
    registry.add(crate::languages::group());
    registry.add(crate::terminal::group());
    registry.add(crate::web::group());
    registry.add(crate::weird::group());
    registry.add(crate::coherence::group());
    registry.add(crate::execcmd::group());
    registry.add(crate::lifecycle::group());
    registry.add(crate::netcontainer::group());
    registry.add(crate::process::group());
    registry.add(crate::runflags::group());
    registry.add(crate::permissions::group());
    registry.add(crate::utilities::group());
    registry
}
