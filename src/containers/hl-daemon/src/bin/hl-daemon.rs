use std::path::PathBuf;

use hl_container::{Config, Containers};
use hl_daemon::{Daemon, Release};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("hl-daemon: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args_os().skip(1);
    let mut root = None;
    let mut socket = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--root") => root = arguments.next().map(PathBuf::from),
            Some("--socket") => socket = arguments.next().map(PathBuf::from),
            Some("--help" | "-h") => {
                println!("usage: hl-daemon --root PATH --socket PATH");
                return Ok(());
            }
            _ => return Err(format!("unknown argument {}", argument.to_string_lossy()).into()),
        }
    }
    let root = root.ok_or("--root is required")?;
    let socket = socket.ok_or("--socket is required")?;
    let containers = Containers::builder(Config::new(root)).build().await?;
    Daemon::new(containers)
        .release(Release::new(env!("CARGO_PKG_VERSION")))
        .server(socket)
        .serve()
        .await?;
    Ok(())
}
