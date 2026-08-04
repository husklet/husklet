fn main() {
    if hl_engine::native::HostConfinement::apply().is_err() {
        std::process::exit(70);
    }
    let file_denied = std::fs::File::open("/etc/passwd").is_err_and(|error| error.raw_os_error() == Some(libc::EPERM));
    let network_denied =
        std::net::TcpStream::connect("127.0.0.1:9").is_err_and(|error| error.raw_os_error() == Some(libc::EPERM));
    let request = hl_engine::native::SpawnRequest {
        program: std::ffi::CString::new("/bin/true").unwrap(),
        arguments: vec![std::ffi::CString::new("true").unwrap()],
        environment: Vec::new(),
        process_group: hl_engine::native::ProcessGroup::Inherit,
        file_actions: Vec::new(),
    };
    let process_denied =
        hl_engine::native::ProcessHandle::spawn(std::sync::Arc::new(hl_engine::native::LinuxHost), &request)
            .is_err_and(|error| error == hl_engine::native::HostError::Denied);
    let thread_allowed = std::thread::Builder::new()
        .name("confined-test".into())
        .spawn(|| 7)
        .is_ok_and(|thread| matches!(thread.join(), Ok(7)));
    let variants_denied = hl_engine::native::HostConfinement::variants_denied();
    std::process::exit(
        if file_denied && network_denied && process_denied && thread_allowed && variants_denied {
            0
        } else {
            71
        },
    );
}
