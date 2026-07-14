//! Host-side shader translation via `naga`.
//!
//! The hl-GPU IR's declared shader ABI is SPIR-V (`Cmd::CreateShader{ spirv: Vec<u32> }`). This module
//! lowers it to WGSL, which the wgpu backend feeds to `create_shader_module` (feature `wgsl`). Doing the
//! whole translation in naga on the host is the thin-guest win: the guest shim forwards SPIR-V (or GLSL)
//! bytes and never packs MSL itself — unlike the bespoke Metal replay, which ships MSL-as-bytes in this
//! same field. We deliberately produce WGSL text (rather than hand wgpu a `naga::Module`) so this crate's
//! own `naga` dependency does the lowering end to end, without coupling to wgpu's internal naga version.

const SPIRV_MAGIC: u32 = 0x0723_0203;

/// Translate an IR `CreateShader` payload to WGSL. Returns `None` when the payload is not real SPIR-V
/// (e.g. the legacy MSL-as-bytes shape), so the backend can select a builtin pipeline instead.
pub fn spirv_to_wgsl(words: &[u32]) -> Result<Option<String>, String> {
    if words.first().copied() != Some(SPIRV_MAGIC) {
        return Ok(None);
    }
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(words.as_ptr() as *const u8, std::mem::size_of_val(words)) };
    let module = naga::front::spv::parse_u8_slice(bytes, &naga::front::spv::Options::default())
        .map_err(|e| format!("spirv-in: {e:?}"))?;
    module_to_wgsl(&module)
}

/// Translate a raw GLSL source string (the guest GLES path) to WGSL for the given stage.
#[allow(dead_code)] // the forward GLES path (shim forwards GLSL); exercised by tests today
pub fn glsl_to_wgsl(src: &str, stage: naga::ShaderStage) -> Result<String, String> {
    let mut frontend = naga::front::glsl::Frontend::default();
    let module = frontend
        .parse(&naga::front::glsl::Options::from(stage), src)
        .map_err(|e| format!("glsl-in: {e:?}"))?;
    module_to_wgsl(&module)?.ok_or_else(|| "glsl-in: empty module".to_string())
}

fn module_to_wgsl(module: &naga::Module) -> Result<Option<String>, String> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("validate: {e:?}"))?;
    let wgsl = naga::back::wgsl::write_string(module, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| format!("wgsl-out: {e}"))?;
    Ok(Some(wgsl))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mint valid SPIR-V from a WGSL seed (WGSL front -> SPIR-V back), then prove SPIR-V -> WGSL works —
    // the round trip the guest's SPIR-V ABI relies on. Mirrors the spike's naga_demo, but through the
    // actual `spirv_to_wgsl` entry point the backend calls.
    #[test]
    fn spirv_roundtrips_to_wgsl() {
        let seed = "@fragment fn fs() -> @location(0) vec4<f32> { return vec4<f32>(0.0,1.0,0.0,1.0); }";
        let module = naga::front::wgsl::parse_str(seed).expect("seed wgsl");
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("seed validate");
        let words = naga::back::spv::write_vec(
            &module,
            &info,
            &naga::back::spv::Options::default(),
            None,
        )
        .expect("seed spv");
        assert_eq!(words.first().copied(), Some(SPIRV_MAGIC), "minted a real SPIR-V header");
        let wgsl = spirv_to_wgsl(&words).expect("translate ok").expect("is spirv");
        assert!(wgsl.contains("fn "), "produced WGSL: {wgsl}");
    }

    #[test]
    fn glsl_lowers_to_wgsl() {
        let src = "#version 450\nlayout(location=0) in vec4 c; layout(location=0) out vec4 o; void main(){ o=c; }";
        let wgsl = glsl_to_wgsl(src, naga::ShaderStage::Fragment).expect("glsl ok");
        assert!(wgsl.contains("fn "), "produced WGSL: {wgsl}");
    }

    #[test]
    fn legacy_msl_is_not_spirv() {
        // The current shim's MSL-as-bytes payload must be recognised as legacy (builtin fallback), not
        // mis-parsed as SPIR-V.
        let msl = "#include <metal_stdlib>\nusing namespace metal;\nfragment float4 f(){return 0;}";
        let mut words = vec![msl.len() as u32];
        for ch in msl.as_bytes().chunks(4) {
            let mut w = [0u8; 4];
            w[..ch.len()].copy_from_slice(ch);
            words.push(u32::from_le_bytes(w));
        }
        assert!(spirv_to_wgsl(&words).unwrap().is_none(), "not spirv");
    }

    #[test]
    fn malformed_spirv_is_a_translation_error() {
        // Correct magic means this is explicitly on the SPIR-V path; the truncated instruction
        // stream must fail translation instead of being classified as a legacy/builtin payload.
        let words = [SPIRV_MAGIC, 0x0001_0000, 0, 2, 0, 0xffff_ffff];
        assert!(spirv_to_wgsl(&words).is_err());
    }
}
