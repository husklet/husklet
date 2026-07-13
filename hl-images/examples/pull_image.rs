//! Pull an OCI image into a local store with the `dd-images` API, then read what a runtime needs.
//!
//!   cargo run -p hl-images --example pull_image -- alpine latest ./store
//!
//! `dd-images` is runtime-agnostic: it fetches + unpacks the image and hands you the `rootfs`, the
//! detected `arch`, and the image's default command/env — you feed those to whatever runtime you use
//! (e.g. `dd-jit`). No Docker daemon involved.

use hl_images::{Credentials, PullEvent, Store};

fn main() -> Result<(), hl_images::Error> {
    let mut args = std::env::args().skip(1);
    let from = args.next().unwrap_or_else(|| "alpine".into());
    let tag = args.next().unwrap_or_else(|| "latest".into());
    let dir = args.next().unwrap_or_else(|| "./store".into());

    let store = Store::new(dir);

    // Progress callback: the pull reports layer download/extract events as it goes.
    let mut on_event = |ev: PullEvent| {
        if let PullEvent::PullComplete { id } = ev {
            println!("layer {id} ready");
        }
    };

    // Anonymous pull (public registry); pass real `Credentials` for a private one.
    let image = store.pull(&from, &tag, Credentials::none(), &mut on_event)?;

    println!("pulled {from}:{tag}");
    println!("  rootfs:     {}", image.rootfs.display());
    println!("  arch:       {:?}", image.arch);
    println!("  entrypoint: {:?}", image.entrypoint_cmd(Vec::<String>::new()));
    println!("  workdir:    {}", image.workdir());
    println!("  user:       {}", image.user());
    println!("  env:        {} vars", image.env().len());
    Ok(())
}
