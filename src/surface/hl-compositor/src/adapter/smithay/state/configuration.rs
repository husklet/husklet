use super::*;

/// Reads the advertised output transform from host configuration.
fn env_output_transform() -> BufferTransform {
    let raw = match std::env::var("HL_OUTPUT_TRANSFORM") {
        Ok(v) => v,
        Err(_) => return BufferTransform::Normal,
    };
    match raw.trim().to_ascii_lowercase().replace('_', "-").as_str() {
        "" | "normal" | "0" => BufferTransform::Normal,
        "90" => BufferTransform::_90,
        "180" => BufferTransform::_180,
        "270" => BufferTransform::_270,
        "flipped" | "flipped-0" => BufferTransform::Flipped,
        "flipped-90" => BufferTransform::Flipped90,
        "flipped-180" => BufferTransform::Flipped180,
        "flipped-270" => BufferTransform::Flipped270,
        other => {
            eprintln!("hl-compositor: unknown HL_OUTPUT_TRANSFORM {other:?}, using Normal");
            BufferTransform::Normal
        }
    }
}

/// The scene's output layout, from `$HL_OUTPUTS` (default: one output).
///
/// Unset (the default): a single `1920×1080@60` output "HL-0" at `(0, 0)`, carrying the advertised
/// `wl_output.transform` from `$HL_OUTPUT_TRANSFORM` — byte-for-byte the pre-multi-output behaviour, so
/// every existing single-output demo is unaffected.
///
/// Set: a `;`-separated list of output specs, each `WxH@X,Y[*S]` — pixel mode `W×H`, layout position
/// `(X, Y)`, optional integer scale `S` (default 1). Refresh comes from
/// `$HL_OUTPUT_REFRESH_MHZ` and defaults to 60 Hz. Outputs are numbered
/// `HL-0`, `HL-1`, … with ids `1, 2, …`; the FIRST is the primary (new surfaces enter it). Example:
/// `HL_OUTPUTS="1920x1080@0,0;2560x1440@1920,0*2"` — a scale-1 1080p output beside a scale-2 1440p one.
/// A malformed spec is skipped with a warning; if nothing parses, the single default is used.
pub(super) fn env_outputs() -> Vec<Output> {
    let refresh_mhz = std::env::var("HL_OUTPUT_REFRESH_MHZ")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(60_000);
    let raw = match std::env::var("HL_OUTPUTS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => {
            return vec![Output::new(OutputId(1), "HL-0", 1920, 1080, refresh_mhz)
                .with_transform(env_output_transform())];
        }
    };

    let mut outputs = Vec::new();
    for (i, spec) in raw
        .split(';')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        match parse_output_spec(spec, i as u32, refresh_mhz) {
            Some(o) => outputs.push(o),
            None => eprintln!("hl-compositor: ignoring malformed HL_OUTPUTS spec {spec:?}"),
        }
    }
    if outputs.is_empty() {
        eprintln!("hl-compositor: HL_OUTPUTS parsed no outputs, using the single default");
        return vec![Output::new(OutputId(1), "HL-0", 1920, 1080, refresh_mhz)
            .with_transform(env_output_transform())];
    }
    outputs
}

/// Parse one `$HL_OUTPUTS` spec `WxH@X,Y[*S]` into an [`Output`] with id/name index `i` (0 → `HL-0`,
/// id `1`). Returns `None` on any malformed field.
fn parse_output_spec(spec: &str, i: u32, refresh_mhz: i64) -> Option<Output> {
    // Split off an optional `*scale` suffix first.
    let (geom, scale) = match spec.split_once('*') {
        Some((g, s)) => (g, s.trim().parse::<i32>().ok().filter(|&s| s > 0)?),
        None => (spec, 1),
    };
    // `WxH@X,Y` — the `@X,Y` position is optional (defaults to origin).
    let (mode, pos) = match geom.split_once('@') {
        Some((m, p)) => (m, Some(p)),
        None => (geom, None),
    };
    let (w, h) = mode.trim().split_once('x')?;
    let (w, h) = (w.trim().parse::<i32>().ok()?, h.trim().parse::<i32>().ok()?);
    if w <= 0 || h <= 0 {
        return None;
    }
    let (x, y) = match pos {
        Some(p) => {
            let (x, y) = p.trim().split_once(',')?;
            (x.trim().parse::<i32>().ok()?, y.trim().parse::<i32>().ok()?)
        }
        None => (0, 0),
    };
    Some(
        Output::new(OutputId(i + 1), format!("HL-{i}"), w, h, refresh_mhz)
            .with_position(x, y)
            .with_scale(scale),
    )
}

/// Map the neutral [`BufferTransform`] onto Smithay's `utils::Transform` (what a `wl_output` advertises).
/// The inverse of [`map_buffer_transform`], used to drive the output's advertised `wl_output.transform`.
impl From<BufferTransform> for Transform {
    fn from(t: BufferTransform) -> Self {
        match t {
            BufferTransform::Normal => Transform::Normal,
            BufferTransform::_90 => Transform::_90,
            BufferTransform::_180 => Transform::_180,
            BufferTransform::_270 => Transform::_270,
            BufferTransform::Flipped => Transform::Flipped,
            BufferTransform::Flipped90 => Transform::Flipped90,
            BufferTransform::Flipped180 => Transform::Flipped180,
            BufferTransform::Flipped270 => Transform::Flipped270,
        }
    }
}
