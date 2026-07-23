use hl_client::model::CreateContainer;
use hl_client::{Client, Config};

#[test]
fn intended_surface_is_simple_and_lazy() {
    let config = Config::unix("/tmp/hl.sock").api_version("v1.43");
    let client = Client::with_config(config).expect("valid configuration");
    let _request = CreateContainer {
        image: "alpine:latest".into(),
        cmd: Some(vec!["echo".into(), "hello".into()]),
        ..Default::default()
    };
    let _containers = client.containers();
    assert_eq!(client.config().socket().to_string_lossy(), "/tmp/hl.sock");
}

#[test]
fn invalid_configuration_is_rejected_before_io() {
    assert!(Client::with_config(Config::unix("").api_version("nonsense")).is_err());
}
