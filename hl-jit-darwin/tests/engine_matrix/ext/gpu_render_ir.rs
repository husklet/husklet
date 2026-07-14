//! Direct hl-gpu compositor replay probes. Owner: gpu-render-ir agent.
//! These cases require a GUI/GPU-enabled hl launch with `/dev/dri/renderD128`
//! and `HL_GPU_EXEC` wired to the host executor socket.

use crate::support::{group, src, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![group(
        "gpu-render-ir",
        vec![
            src("gpu-compositor-multipass", "gpu_compositor_multipass_ir.c")
                .only(&[Engine::LinuxAarch64])
                .has("gpu_compositor_multipass: ok offscreen_rgba=1 load_store=1 sample_to_bgra=1")
                .xfail(&[Engine::LinuxAarch64]),
        ],
    )]
}
