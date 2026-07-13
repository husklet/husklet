//! Configure and launch a container from Rust with the `dd-jit` API.
//!
//!   cargo run -p hl-jit --example run_container -- /var/lib/dd/alpine
//!
//! `dd-jit` selects the host backend at compile time (`dd-jit-darwin` today) and runs the container
//! directly — no Docker daemon, no shelling out.

use hl_jit::{Container, Image, Runtime};

fn main() -> Result<(), hl_jit::Error> {
    let rootfs = std::env::args().nth(1).unwrap_or_else(|| "/var/lib/dd/alpine".into());

    let rt = Runtime::new()?;

    let container = Container::builder(Image::from_rootfs(rootfs))
        .cmd(["/bin/sh", "-c", "echo hello from dd-jit; id; nproc"])
        .env("TERM", "xterm")
        .cpus(2)
        .memory_mb(512)
        .read_only(true)
        .publish(8080, 80)
        .bind("/host/data", "/data", /* read_only = */ false)
        .hostname("web")
        .build()?;

    if !rt.supports(container.guest()) {
        eprintln!("no dd-jit backend built for this guest on this host");
        return Ok(());
    }

    let mut handle = rt.run(&container)?;
    println!("container pid {}", handle.pid());
    let status = handle.wait()?;
    println!("exited {}", status.code());
    Ok(())
}
