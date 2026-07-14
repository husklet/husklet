//! The Vulkan → hl-gpu-IR mapping seam (sketch + anti-drift round-trip).
//!
//! This increment establishes the ICD surface and object model; the sibling host-execution agent
//! turns these IR streams into real Metal work. What lives here is the *encode seam*: the mapping
//! from the Vulkan object/command model onto the shared [`hl_shim::ir`] vocabulary (which is
//! `hl_gpu::ir` re-exported verbatim — the guest producer and host executor share ONE Rust type and
//! ONE encode/decode, so they cannot drift). The mapping mirrors hl-shim-gl's `lower.rs` and
//! hl-shim-cuda's `CudaContext` seam.
//!
//! ## Vulkan → IR correspondence
//! | Vulkan                                   | hl-gpu IR                                       |
//! |------------------------------------------|-------------------------------------------------|
//! | `VkDeviceMemory` + `VkBuffer`            | [`Cmd::CreateBuffer`] (`BufferDesc`)            |
//! | `vkMapMemory` write / `vkCmdUpdateBuffer`| [`Cmd::WriteBuffer`]                            |
//! | `VkImage` / `VkImageView`                | [`Cmd::CreateTexture`] (`TextureDesc`)          |
//! | `VkSampler`                              | [`Cmd::CreateSampler`]                          |
//! | `VkShaderModule` (**SPIR-V**)            | [`Cmd::CreateShader`] `{ spirv }` — *direct*    |
//! | `VkPipeline` (graphics)                  | [`Cmd::CreateRenderPipeline`]                   |
//! | `VkPipeline` (compute)                   | [`Cmd::CreateComputePipeline`]                  |
//! | `VkDescriptorSet`                        | [`Cmd::CreateBindGroup`]                        |
//! | `vkCmdBindPipeline` / `vkCmdDispatch`    | `Enc::SetPipeline` / `Enc::Dispatch`            |
//! | `vkCmdBindPipeline` / `vkCmdDraw*`       | `Enc::SetPipeline` / `Enc::Draw*`               |
//! | `vkCmdBeginRenderPass` / `End`           | `Enc::BeginRenderPass` / `EndRenderPass`        |
//! | `vkQueueSubmit` (`VkCommandBuffer`)      | [`Cmd::Submit`] (`CommandBuffer{ encoder }`)    |
//! | `VkFence` / `VkSemaphore` (timeline)     | [`Cmd::CreateFence`] / [`Cmd::WaitFence`]       |
//!
//! The single most important row: a `VkShaderModule` **is** SPIR-V, and the IR's shader ABI is ALSO
//! SPIR-V (`Cmd::CreateShader{ spirv: Vec<u32> }`, lowered host-side to MSL by naga in hl-gpu-wgpu).
//! So Vulkan shaders forward with **zero translation** — the thinnest possible guest seam, and the
//! reason Vulkan is a natural fit for this IR.

use hl_shim::ir::{
    BufferDesc, Cmd, CommandBuffer, ComputePipelineDesc, Enc, ShaderRef, TextureDesc, TextureDim,
    TextureFormat,
};

/// Map a `VkBuffer`/`VkDeviceMemory` pair to the IR buffer-create command. `usage` is the opaque
/// WebGPU-style usage bitset the host backend understands (Vulkan `VkBufferUsageFlags` translate to
/// it in a later increment; for the seam we pass it through).
pub fn create_buffer(id: u32, size: u64, usage: u32, label: &str) -> Cmd {
    Cmd::CreateBuffer(
        id,
        BufferDesc {
            size,
            usage,
            label: label.to_string(),
        },
    )
}

/// Map a `VkImage` to the IR texture-create command.
pub fn create_image(id: u32, width: u32, height: u32, format: TextureFormat, usage: u32) -> Cmd {
    Cmd::CreateTexture(
        id,
        TextureDesc {
            width,
            height,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format,
            usage,
            label: String::new(),
        },
    )
}

/// Map a `VkShaderModule` — whose `pCode` is SPIR-V words — to the IR shader-create command with NO
/// translation (SPIR-V is the IR shader ABI). This is the seam's keystone.
pub fn create_shader_module(id: u32, spirv: Vec<u32>) -> Cmd {
    Cmd::CreateShader { id, kind: hl_gpu::ir::ShaderPayloadKind::SpirV, spirv }
}

/// Map a compute `VkPipeline` (its `VkShaderModule` + entry point) to the IR compute pipeline.
pub fn create_compute_pipeline(id: u32, shader_id: u32, entry: &str, label: &str) -> Cmd {
    Cmd::CreateComputePipeline(
        id,
        ComputePipelineDesc {
            compute: ShaderRef {
                module: shader_id,
                entry: entry.to_string(),
            },
            label: label.to_string(),
        },
    )
}

/// Map a recorded compute command buffer (`vkCmdBindPipeline` + `vkCmdDispatch`) submitted via
/// `vkQueueSubmit` to an IR [`Cmd::Submit`], optionally signalling a fence (`VkFence`).
pub fn submit_compute_dispatch(
    pipeline_id: u32,
    groups: (u32, u32, u32),
    signal_fence: Option<(u32, u64)>,
) -> Cmd {
    let encoder = vec![
        Enc::BeginComputePass,
        Enc::SetPipeline(pipeline_id),
        Enc::Dispatch {
            x: groups.0,
            y: groups.1,
            z: groups.2,
        },
        Enc::EndComputePass,
    ];
    Cmd::Submit(CommandBuffer {
        encoder,
        signal: signal_fence,
    })
}

/// Build a representative end-to-end compute stream — a SPIR-V module, its compute pipeline, a
/// dispatch, and a fence signal — exactly as the future `vkCreateShaderModule` →
/// `vkCreateComputePipelines` → `vkCmdDispatch` → `vkQueueSubmit` path will. Used by the anti-drift
/// round-trip test (and available to integration tests).
pub fn demo_compute_stream(spirv: Vec<u32>) -> Vec<Cmd> {
    const SHADER: u32 = 1;
    const PIPELINE: u32 = 2;
    const IN_BUF: u32 = 10;
    const OUT_BUF: u32 = 11;
    const FENCE: u32 = 100;
    vec![
        create_buffer(IN_BUF, 1024, 0, "in"),
        create_buffer(OUT_BUF, 1024, 0, "out"),
        create_shader_module(SHADER, spirv),
        create_compute_pipeline(PIPELINE, SHADER, "main", "hl-vk-compute"),
        Cmd::CreateFence(FENCE),
        submit_compute_dispatch(PIPELINE, (64, 1, 1), Some((FENCE, 1))),
        Cmd::WaitFence {
            id: FENCE,
            value: 1,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_shim::ir::{decode_stream, encode_stream};

    /// Anti-drift round-trip (mirrors hl-shim-gl's `framebuilder_encodes_the_shared_contract` and
    /// hl-shim-cuda's `launch_path_encodes_the_shared_ir_contract`): encode a representative Vulkan
    /// compute stream with the guest producer, then decode it with the HOST's own `hl_gpu::ir`
    /// decoder. Same bytes, same code path — guest and host cannot drift.
    #[test]
    fn vk_compute_seam_encodes_the_shared_ir_contract() {
        // A tiny but valid SPIR-V header + a couple words (the payload the seam forwards verbatim).
        let spirv: Vec<u32> = vec![0x0723_0203, 0x0001_0000, 0, 1, 0];
        let cmds = demo_compute_stream(spirv.clone());

        let bytes = encode_stream(&cmds);
        let decoded = decode_stream(&bytes).expect("host decodes the guest-produced IR");
        assert_eq!(decoded, cmds, "round-tripped IR must be byte-for-byte identical");

        // The keystone: the SPIR-V survived the seam untouched (no translation on the guest side).
        let (kind, shader) = decoded
            .iter()
            .find_map(|c| match c {
                Cmd::CreateShader { kind, spirv, .. } => Some((*kind, spirv.clone())),
                _ => None,
            })
            .expect("stream carries a CreateShader");
        assert_eq!(kind, hl_shim::ir::ShaderPayloadKind::SpirV);
        assert_eq!(shader, spirv, "VkShaderModule SPIR-V forwards to IR verbatim");

        // And a compute pipeline + dispatch + fence made it through intact.
        assert!(decoded.iter().any(|c| matches!(c, Cmd::CreateComputePipeline(..))));
        assert!(decoded.iter().any(|c| matches!(c, Cmd::Submit(_))));
        assert!(decoded.iter().any(|c| matches!(c, Cmd::WaitFence { .. })));
    }
}
