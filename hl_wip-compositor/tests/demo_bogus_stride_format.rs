//! ROBUSTNESS DEMO 5 — `bogus_stride_format` (inconsistent stride / unsupported format is rejected cleanly).
//!
//! A hostile client forges two malformed `wl_shm_pool.create_buffer` requests:
//!   * an INCONSISTENT STRIDE — width 64 (needs stride ≥ 256 bytes) declared with stride 100, and
//!   * an UNSUPPORTED FORMAT — `Rgb565`, which this compositor never advertises (it offers only
//!     Argb8888 / Xrgb8888).
//!
//! Both are rejected server-side: Smithay's shm core validates buffer geometry + format at
//! `create_buffer` and posts a fatal `wl_shm.error` (`invalid_stride` / `invalid_format`), disconnecting
//! only the offending client. The compositor thread neither panics nor wedges, and a fresh well-behaved
//! [`Neighbor`] then attaches a VALID buffer and composites an EXACT frame.
//!
//! Reachability note (important): a bad stride/format is a FATAL protocol error — Smithay tears down the
//! offending client's whole connection at `create_buffer`, so the buffer never reaches the adapter's own
//! `read_shm_rgba`, and the SAME client cannot "recover" with a subsequent valid buffer (its connection
//! is gone). The reachable, meaningful proof is therefore: the adapter survives the rejection AND still
//! serves a valid buffer afterward — demonstrated here by a fresh valid client (the closest reachable
//! stand-in for "a subsequent valid buffer presents correctly"). The adapter's `read_shm_rgba` carries a
//! second, redundant line of defense (it re-checks `stride < width*4`, non-positive dims, and mapping
//! bounds, returning a benign no-content commit rather than reading out of bounds) for any malformed
//! geometry that a future non-shm buffer path might deliver un-validated.

mod common;
use common::*;

use std::io::Write;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry,
    wl_shm::{self, WlShm}, wl_shm_pool::WlShmPool,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

const NW: i32 = 128;
const NH: i32 = 96;
const NEIGHBOR: [u8; 4] = [0xC0, 0x40, 0xE0, 0xFF]; // purple survivor

struct Hostile;

#[test]
fn bogus_stride_format() {
    let h = Harness::start("bogus_stride_format");

    // ---- abuse A: inconsistent stride (width 64 needs >= 256 bytes/row, declared 100) ----
    hostile_buffer(|shm, qh| {
        // A real 4 KiB pool so the pool itself is valid; only the buffer's stride is inconsistent.
        let file = tempfile_of(4096);
        let pool: WlShmPool = shm.create_pool(file.as_fd(), 4096, qh, ());
        std::mem::forget(file);
        let _bad: WlBuffer = pool.create_buffer(0, 64, 16, 100, wl_shm::Format::Argb8888, qh, ());
    });

    // ---- abuse B: unsupported pixel format (Rgb565 is never advertised) ----
    hostile_buffer(|shm, qh| {
        let file = tempfile_of(4096);
        let pool: WlShmPool = shm.create_pool(file.as_fd(), 4096, qh, ());
        std::mem::forget(file);
        let _bad: WlBuffer = pool.create_buffer(0, 16, 16, 32, wl_shm::Format::Rgb565, qh, ());
    });

    // Let the serve loop process both fatal disconnects.
    std::thread::sleep(Duration::from_millis(50));

    // ---- survivor: a fresh valid client attaches a VALID buffer and composites an exact frame ----
    let mut neighbor = Neighbor::map(&h.runtime_dir, "survivor", NW, NH, NEIGHBOR);
    let frame = neighbor.assert_presents(&h.captures);
    save_frame("bogus_stride_format-survivor", &frame);
    assert_eq!(frame.pixel(NW / 2, NH / 2).unwrap(), NEIGHBOR, "survivor is solid purple");

    h.shutdown();
}

/// Connect a throwaway hostile client, bind `wl_shm`, run `forge` (which builds a malformed buffer), and
/// roundtrip so the server processes (and rejects) the abuse.
fn hostile_buffer(forge: impl FnOnce(&WlShm, &QueueHandle<Hostile>)) {
    let conn = Connection::connect_to_env().expect("hostile connect");
    let (globals, mut queue) = registry_queue_init::<Hostile>(&conn).expect("hostile registry");
    let qh = queue.handle();
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    forge(&shm, &qh);
    let _ = queue.roundtrip(&mut Hostile); // process + reject; the connection is then torn down
}

/// A sealed, unlinked temp file of `bytes` zero bytes — a real shm pool fd backing.
fn tempfile_of(bytes: usize) -> std::fs::File {
    let path = std::env::temp_dir().join(format!("hl-bogus-{}-{}.shm", std::process::id(), Instant::now().elapsed().as_nanos()));
    let mut f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).expect("tmp shm");
    f.write_all(&vec![0u8; bytes]).unwrap();
    f.flush().unwrap();
    let _ = std::fs::remove_file(&path);
    f
}

// ---------- dispatch plumbing ----------
impl Dispatch<WlRegistry, GlobalListContents> for Hostile {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for Hostile {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(WlCompositor, WlShm, WlShmPool, WlBuffer);
