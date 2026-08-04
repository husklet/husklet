//! Real API/daemon integration tests, one purpose per file.

pub(crate) mod support;

pub(crate) mod test_concurrent_clients;
pub(crate) mod test_container_copy;
pub(crate) mod test_daemon_runtime;
pub(crate) mod test_descendant_cleanup;
pub(crate) mod test_headless_lifecycle;
pub(crate) mod test_headless_runtime;
pub(crate) mod test_http_errors;
pub(crate) mod test_image_archive;
pub(crate) mod test_malformed_image_archive;
pub(crate) mod test_named_volume;
pub(crate) mod test_network_bridge;
pub(crate) mod test_persistence_restart;
pub(crate) mod test_port_publishing;
pub(crate) mod test_removal_wait_race;
pub(crate) mod test_resources;
pub(crate) mod test_server_process;
pub(crate) mod test_server_restart_persistence;
