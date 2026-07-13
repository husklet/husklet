//! Flag-gated content-tile tracer (`DD_TILE_TRACE`) — INSTRUMENTATION ONLY, no behavior change.
//!
//! Purpose: pin, on a live multi-process Chrome run, WHERE the renderer's rastered web content is lost
//! before the viz/GPU process composites it. The GL shim's frame lowering (`frame.rs`) only binds a
//! sampler texture when it has CPU-side pixel `data` (uploaded via `glTexImage2D`/`glTexSubImage2D`) or
//! is an in-frame offscreen render target; a texture whose pixels live in an *external / cross-process*
//! buffer (a SharedImage / GpuMemoryBuffer / EGLImage the renderer filled elsewhere) has empty `data`
//! and is silently DROPPED from the bind group — the content quad then samples nothing → white content.
//!
//! This tracer replays `frame.rs`'s exact tile-selection logic against the same `GlState` at swap time
//! and reports, per frame:
//!   * how many render passes target the default surface vs an offscreen FBO (does the shim see any
//!     renderer raster-into-tile at all?),
//!   * for every sampler-bound texture the compositor draws, whether it carries pixel `data` or is
//!     EMPTY (would be dropped → the "sampled empty tile"), with its GL id + dimensions.
//!
//! It never mutates state and emits nothing unless `DD_TILE_TRACE` is set, so it is safe to leave in.
//! Correlate its "EMPTY tile" lines with `DD_SHIM_DEBUG` "unimplemented entry point: eglCreateImage /
//! eglBindTexImage" lines: an EMPTY tile plus a stubbed image-import verb is the buffer-bridge gap.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::state::{GlState, MAXTEX};

static FRAME: AtomicU64 = AtomicU64::new(0);

#[inline]
fn enabled() -> bool {
    std::env::var_os("DD_TILE_TRACE").is_some()
}

/// The program a draw renders with (mirrors `frame.rs::dpr_idx` without changing its visibility).
fn dpr_idx(s: &GlState, d: &crate::state::DrawCall) -> usize {
    if (d.prog as usize) < s.prog.len() && s.prog[d.prog as usize].used {
        d.prog as usize
    } else {
        s.cur_prog as usize
    }
}

fn tex_dims(s: &GlState, id: u32) -> (i32, i32, usize, bool) {
    match s.tex.get(id as usize) {
        Some(t) if t.used => (t.w, t.h, t.data.len(), t.data.is_empty()),
        _ => (0, 0, 0, true),
    }
}

/// Emit one trace block for the frame being lowered. Call at the top of `build_frame_ir` (post-guard).
pub fn trace_frame(s: &GlState) {
    if !enabled() {
        return;
    }
    let f = FRAME.fetch_add(1, Ordering::Relaxed);

    let ndraw = s.draws.len();
    let nclear = s.draws.iter().filter(|d| d.is_clear).count();

    // Render-pass targets: 0 == default window surface, else an offscreen FBO color texture (a tile the
    // GPU process rasters into IN THIS PROCESS — the single-process content signature `Begin target=N`).
    let mut offscreen_targets: Vec<u32> = Vec::new();
    let mut default_pass = false;
    for d in s.draws.iter().filter(|d| !d.is_clear) {
        let t = d.target_tex;
        if t != 0 && (t as usize) < MAXTEX && s.tex.get(t as usize).map(|x| x.used).unwrap_or(false) {
            if !offscreen_targets.contains(&t) {
                offscreen_targets.push(t);
            }
        } else {
            default_pass = true;
        }
    }

    // Sampler-bound textures the compositor samples for content/UI quads, split by whether the shim has
    // their pixels. An EMPTY one is exactly what `frame.rs` drops from `texlist` (`!data.is_empty()`
    // gate) → the content quad samples an unbacked texture → white. This is the "sampled empty tile".
    let mut sampled_with_data: Vec<(u32, i32, i32, usize)> = Vec::new();
    let mut sampled_empty: Vec<(u32, i32, i32)> = Vec::new();
    for d in s.draws.iter().filter(|d| !d.is_clear) {
        let dpr = &s.prog[dpr_idx(s, d)];
        for i in 0..dpr.samp_names.len().min(4) {
            let unit = if (0..8).contains(&d.samp_units[i]) { d.samp_units[i] as usize } else { i };
            let tu = d.tex_units[unit];
            if tu == 0 {
                continue;
            }
            let (w, h, len, empty) = tex_dims(s, tu);
            if empty {
                if !sampled_empty.iter().any(|&(id, ..)| id == tu) {
                    sampled_empty.push((tu, w, h));
                }
            } else if !sampled_with_data.iter().any(|&(id, ..)| id == tu) {
                sampled_with_data.push((tu, w, h, len));
            }
        }
    }

    eprintln!(
        "[tiletrace] frame={f} draws={ndraw} clears={nclear} \
         default_pass={default_pass} offscreen_fbo_passes={} \
         sampled_with_data={} sampled_EMPTY={}",
        offscreen_targets.len(),
        sampled_with_data.len(),
        sampled_empty.len(),
    );
    for (id, w, h) in &sampled_empty {
        eprintln!("[tiletrace]   SAMPLED-EMPTY tile: glTex={id} {w}x{h} data=0  (DROPPED from bind group → samples zero → WHITE)");
    }
    for (id, w, h, len) in &sampled_with_data {
        eprintln!("[tiletrace]   sampled tile: glTex={id} {w}x{h} data={len}B (has pixels → composited)");
    }
    for t in &offscreen_targets {
        let (w, h, len, _) = tex_dims(s, *t);
        eprintln!("[tiletrace]   offscreen FBO render-target: glTex={t} {w}x{h} data={len}B (in-process raster the shim sees)");
    }

    // Optional per-frame dump to $DD_TEXTURE_DUMP_DIR: a manifest of every live texture + its data len,
    // and the raw RGBA of any non-empty sampler tile, so a live run yields inspectable evidence.
    if let Some(dir) = std::env::var_os("DD_TEXTURE_DUMP_DIR") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let mut manifest = format!(
            "frame {f}: draws={ndraw} clears={nclear} default_pass={default_pass} \
             offscreen_fbo={} sampled_with_data={} sampled_EMPTY={}\n",
            offscreen_targets.len(),
            sampled_with_data.len(),
            sampled_empty.len(),
        );
        for (i, t) in s.tex.iter().enumerate() {
            if t.used {
                manifest.push_str(&format!("  glTex={i} {}x{} data={}B\n", t.w, t.h, t.data.len()));
            }
        }
        let _ = std::fs::write(dir.join(format!("tiletrace-{f:05}.txt")), manifest);
        for (id, ..) in &sampled_with_data {
            if let Some(t) = s.tex.get(*id as usize) {
                let _ = std::fs::write(dir.join(format!("tile-f{f:05}-tex{id}.rgba")), &t.data);
            }
        }
    }
}
