use super::support::*;

#[tokio::test]
async fn server_refuses_to_delete_regular_file_at_socket_path() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("occupied");
    std::fs::write(&path, b"keep").unwrap();
    let error = Daemon::new(containers(&root).await)
        .server(&path)
        .serve_with_shutdown(async {})
        .await
        .unwrap_err();
    assert!(matches!(error, Error::OccupiedSocket(value) if value == path));
    assert_eq!(std::fs::read(&path).unwrap(), b"keep");
}
