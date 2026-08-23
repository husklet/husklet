fn main() {
    let arguments = std::env::args().collect::<Vec<_>>();
    if arguments.get(1).map(String::as_str) == Some("--backend-receipt") {
        match engine::backend_receipt(&arguments, None) {
            Ok(receipt) => {
                println!("{receipt}");
                std::process::exit(0);
            }
            Err(reason) => {
                eprintln!("hl-engine: {reason}");
                std::process::exit(125);
            }
        }
    }
    let guest = arguments
        .windows(2)
        .find(|pair| pair[0] == "--guest-isa")
        .and_then(|pair| match pair[1].as_str() {
            "aarch64" | "arm64" => Some(engine::Guest::Aarch64),
            "x86_64" | "amd64" => Some(engine::Guest::X86_64),
            _ => None,
        })
        .unwrap_or(if cfg!(target_arch = "aarch64") {
            engine::Guest::Aarch64
        } else {
            engine::Guest::X86_64
        });
    engine::Worker::run(guest)
}
