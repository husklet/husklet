fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    let value = |name: &str| {
        arguments
            .windows(2)
            .find(|pair| pair[0] == name)
            .and_then(|pair| pair[1].parse::<i32>().ok())
    };
    let result = value("--session-fd")
        .zip(value("--bootstrap-fd"))
        .zip(value("--health-fd"))
        .zip(value("--transfer-fd"))
        .ok_or(())
        .and_then(|(((session, bootstrap), health), transfer)| {
            let file = value("--project-fd");
            let root = value("--root-fd");
            let writable = arguments.iter().any(|argument| argument == "--root-write");
            hl_engine::native::Child::run_projected(session, bootstrap, health, transfer, file, root, writable)
                .map_err(|_| ())
        });
    if result.is_err() {
        std::process::exit(70);
    }
}
