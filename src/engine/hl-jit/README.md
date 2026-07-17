# hl-jit

A clean, platform-agnostic Rust API for **configuring and running Linux containers directly from
code** — no Docker daemon, no VM, no shelling out. `hl-jit` picks a host backend at compile time
(`hl-jit-darwin` today; `hl-jit-linux` / `hl-jit-win` in future — all the same API) and runs the
container through the VM-less dd JIT engine.

## Configure and launch a container from Rust

```rust
use hl_jit::{Runtime, Container, Image};

let rt = Runtime::new()?;                                   // the host backend (darwin today)

let container = Container::builder(Image::from_rootfs("/var/lib/dd/alpine"))
    .cmd(["/bin/sh", "-c", "echo hi"])
    .env("TERM", "xterm")
    .cpus(2)
    .memory_mb(512)
    .read_only(true)
    .publish(8080, 80)                                      // -p 8080:80
    .bind("/host/data", "/data", /* read_only = */ false)  // -v
    .hostname("web")
    .build()?;

let mut handle = rt.run(&container)?;                       // launch
println!("pid {}", handle.pid());
let status = handle.wait()?;                                // or handle.signal(libc::SIGTERM)
println!("exited {}", status.code());
# Ok::<(), hl_jit::Error>(())
```

Runnable version: [`examples/run_container.rs`](examples/run_container.rs)
(`cargo run -p hl-jit --example run_container -- /path/to/rootfs`).

## What the builder covers

`cmd` · `env` · `cwd` · `guest_env` (docker env semantics) · `user`/`user_spec` · `cpus` ·
`memory_mb`/`memory_bytes` · `pids` · `read_only` · `ulimit` · `hostname` · `publish` · `bind` ·
`private_network` · `net_isolate` · `bridge` · `persistent_cache` · `sandbox` — every knob is typed,
with sensible defaults (unlimited resources, shared network, root user); set only what you need.

## The two-crate model

- **`hl-jit`** — this crate: the public API (`Runtime`, `Image`, `Container` + builder, `RunHandle`,
  `Error`). Platform-agnostic; depends only on the backend for the current host.
- **`hl-jit-darwin`** — the macOS-host backend: the C DBT engine (x86-64 + aarch64 Linux guests → ARM64)
  and the darwinjail for native macOS containers.

[`hl-daemon`](../hl-daemon) is a thin Docker-Engine-API polyfill layered on top of this crate: it
translates Docker HTTP requests into `hl_jit` calls and owns no runtime logic of its own.
