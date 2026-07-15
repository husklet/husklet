//! ROBUSTNESS DEMO 1 — `oversized_buffer_rejected` (a hostile buffer geometry cannot crash the adapter).
//!
//! A hostile client asks `wl_shm_pool.create_buffer` for buffers with abusive geometry:
//!   * an ABSURDLY LARGE buffer (100000×100000, stride 400000) whose declared extent dwarfs its tiny
//!     backing pool, and
//!   * a ZERO-SIZE buffer (0×0).
//!
//! Both are rejected server-side (Smithay validates buffer geometry against the pool bounds and posts a
//! fatal `wl_shm.error`, disconnecting only the offending client) WITHOUT the compositor thread panicking
//! or wedging. The proof the adapter SURVIVED is a fresh, well-behaved [`Neighbor`] client that connects
//! afterward and composites an EXACT solid frame all the way through wl → scene → present.
//!
//! Reachability note: an oversized geometry is caught at `create_buffer` by Smithay's shm core, so it
//! never reaches the adapter's own `read_shm_rgba`. That reader is nonetheless overflow-safe by
//! construction — any geometry it sees has already been bounded to fit an shm pool whose size is an
//! `i32`, so `width*height*4` cannot exceed `i32::MAX`. This demo drives the reachable abuse (the wire
//! request) and proves the survivor.

mod common;
use common::*;

use std::time::{Duration, Instant};

use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry::WlRegistry,
    wl_shm::{self, WlShm}, wl_shm_pool::WlShmPool,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};

const NW: i32 = 120;
const NH: i32 = 90;
const NEIGHBOR: [u8; 4] = [0x20, 0xC0, 0xF0, 0xFF]; // cyan — the well-behaved survivor

/// A minimal hostile client: it only needs `wl_shm` to forge abusive `create_buffer` requests.
struct Hostile;

#[test]
fn oversized_buffer_rejected() {
    let h = Harness::start("oversized_buffer");

    // ---- abuse A: a buffer whose declared geometry vastly exceeds its backing pool ----
    {
        let conn = Connection::connect_to_env().expect("hostile connect");
        let (globals, mut queue) = registry_queue_init::<Hostile>(&conn).expect("hostile registry");
        let qh = queue.handle();
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");

        // A tiny (16-byte) real pool, then a create_buffer claiming 100000×100000 @ stride 400000 — an
        // extent of ~40 GB against 16 bytes. Smithay rejects with wl_shm.error and drops this client.
        let file = tempfile_of(16);
        let pool: WlShmPool = shm.create_pool(std::os::fd::AsFd::as_fd(&file), 16, &qh, ());
        let _huge: WlBuffer = pool.create_buffer(0, 100_000, 100_000, 400_000, wl_shm::Format::Argb8888, &qh, ());
        let _ = queue.roundtrip(&mut Hostile); // let the server process (and reject) the abuse
        // The connection is now (or about to be) killed by the protocol error; drop it.
    }

    // ---- abuse B: a zero-size buffer ----
    {
        let conn = Connection::connect_to_env().expect("hostile connect 2");
        let (globals, mut queue) = registry_queue_init::<Hostile>(&conn).expect("hostile registry 2");
        let qh = queue.handle();
        let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm 2");
        let file = tempfile_of(16);
        let pool: WlShmPool = shm.create_pool(std::os::fd::AsFd::as_fd(&file), 16, &qh, ());
        let _zero: WlBuffer = pool.create_buffer(0, 0, 0, 0, wl_shm::Format::Argb8888, &qh, ());
        let _ = queue.roundtrip(&mut Hostile);
    }

    // Give the serve loop a beat to process both disconnects.
    std::thread::sleep(Duration::from_millis(50));

    // ---- survivor: a normal client still composites an exact frame ----
    let mut neighbor = Neighbor::map(&h.runtime_dir, "survivor", NW, NH, NEIGHBOR);
    let frame = neighbor.assert_presents(&h.captures);
    save_frame("oversized_buffer-survivor", &frame);
    assert_eq!(frame.pixel(NW / 2, NH / 2).unwrap(), NEIGHBOR, "survivor is solid cyan");

    h.shutdown();
}

/// A sealed, unlinked temp file of `bytes` zero bytes — a real shm pool fd backing.
fn tempfile_of(bytes: usize) -> std::fs::File {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("hl-hostile-{}-{}.shm", std::process::id(), Instant::now().elapsed().as_nanos()));
    let mut f = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path).expect("tmp shm");
    f.write_all(&vec![0u8; bytes]).unwrap();
    f.flush().unwrap();
    let _ = std::fs::remove_file(&path);
    f
}

// ---------- Dispatch plumbing (hostile client is intentionally minimal) ----------
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
