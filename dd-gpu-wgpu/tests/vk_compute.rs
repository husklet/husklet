//! End-to-end Vulkan COMPUTE on the REAL Metal GPU, driven through dd-shim-vk's exported `vk*` API.
//!
//! The milestone: an app that calls ONLY the Vulkan entry points dd-shim-vk exports — create
//! instance/device, create buffers + device memory, map/write inputs, create a SPIR-V compute shader,
//! a compute pipeline, a descriptor set, record `vkCmdDispatch`, `vkQueueSubmit` — produces a dd-gpu
//! IR stream that, replayed on the host `WgpuBackend`, runs `c[i] = a[i] + b[i]` on a live Metal
//! device with the correct readback. This is the Vulkan analogue of `dd-gpu-wgpu`'s
//! `spirv_compute.rs`, but every GPU action goes through the guest driver's `vk*` surface.
//!
//! The test plays the role of the host GPU-exec service: it drains the shim-recorded IR
//! (`reg::take_ir`) and replays it on the backend, exactly as `$DD_GPU_EXEC` would in production
//! (and as dd-shim-cuda's tests replay on an embedded backend). Needs a Metal device → macOS only.
//! Run: `cargo test -p dd-shim-vk --test vk_compute`.

#![cfg(target_os = "macos")]

use ash::vk;
use ash::vk::Handle;
use core::ffi::c_void;
use dd_gpu::backend::GpuBackend;
use dd_gpu::id::BufferId;
use dd_gpu_wgpu::WgpuBackend;
use vk_dd as ddvk; // the crate's lib is named `vk_dd` (the ICD library name)

fn module_to_spirv(module: &naga::Module) -> Result<Vec<u32>, String> {
    let info = naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::all(),
    )
    .validate(module)
    .map_err(|e| format!("validate: {e:?}"))?;
    naga::back::spv::write_vec(module, &info, &naga::back::spv::Options::default(), None)
        .map_err(|e| format!("spv-out: {e:?}"))
}

const VECADD_GLSL: &str = "\
#version 450
layout(local_size_x = 64) in;
layout(set = 0, binding = 0) buffer A { float a[]; };
layout(set = 0, binding = 1) buffer B { float b[]; };
layout(set = 0, binding = 2) buffer C { float c[]; };
void main() { uint i = gl_GlobalInvocationID.x; c[i] = a[i] + b[i]; }
";
const VECADD_WGSL: &str = "\
@group(0) @binding(0) var<storage, read> a: array<f32>;
@group(0) @binding(1) var<storage, read> b: array<f32>;
@group(0) @binding(2) var<storage, read_write> c: array<f32>;
@compute @workgroup_size(64)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) { let i = gid.x; c[i] = a[i] + b[i]; }
";

fn vecadd_spirv() -> Vec<u32> {
    let mut fe = naga::front::glsl::Frontend::default();
    match fe
        .parse(&naga::front::glsl::Options::from(naga::ShaderStage::Compute), VECADD_GLSL)
        .map_err(|e| format!("{e:?}"))
        .and_then(|m| module_to_spirv(&m))
    {
        Ok(w) => {
            eprintln!("vk_compute: SPIR-V minted from GLSL 450 (Vulkan path)");
            w
        }
        Err(e) => {
            eprintln!("vk_compute: GLSL SSBO front end unavailable ({e}); using WGSL-sourced SPIR-V");
            let m = naga::front::wgsl::parse_str(VECADD_WGSL).expect("wgsl");
            module_to_spirv(&m).expect("wgsl→spv")
        }
    }
}

/// Create a STORAGE buffer + host-visible memory, bind them, and (optionally) write `data`.
unsafe fn make_buffer(dev: *mut c_void, size: u64, data: Option<&[u8]>) -> (u64, u32) {
    let ci = vk::BufferCreateInfo::default()
        .size(size)
        .usage(vk::BufferUsageFlags::STORAGE_BUFFER);
    let mut buf: u64 = 0;
    assert_eq!(ddvk::vkCreateBuffer(dev, &ci, core::ptr::null(), &mut buf), 0);

    let ai = vk::MemoryAllocateInfo::default().allocation_size(size);
    let mut mem: u64 = 0;
    assert_eq!(ddvk::vkAllocateMemory(dev, &ai, core::ptr::null(), &mut mem), 0);
    assert_eq!(ddvk::vkBindBufferMemory(dev, buf, mem, 0), 0);

    if let Some(bytes) = data {
        let mut p: *mut c_void = core::ptr::null_mut();
        assert_eq!(ddvk::vkMapMemory(dev, mem, 0, size, 0, &mut p), 0);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
        ddvk::vkUnmapMemory(dev, mem);
    }
    // The IR id the shim assigned (for readback), read from the recording registry.
    let ir = ddvk::reg::lock().buffers.get(&buf).map(|b| b.ir_id).unwrap();
    (buf, ir)
}

#[test]
fn vk_vecadd_runs_on_real_metal() {
    const N: usize = 1024;
    ddvk::reg::reset();

    // --- instance / device / queue / command pool (the bring-up path) ---
    let mut inst: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkCreateInstance(core::ptr::null(), core::ptr::null(), &mut inst), 0);
    let mut count = 0u32;
    assert_eq!(ddvk::vkEnumeratePhysicalDevices(inst, &mut count, core::ptr::null_mut()), 0);
    let mut phys: *mut c_void = core::ptr::null_mut();
    count = 1;
    assert_eq!(ddvk::vkEnumeratePhysicalDevices(inst, &mut count, &mut phys), 0);
    let mut dev: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkCreateDevice(phys, core::ptr::null(), core::ptr::null(), &mut dev), 0);
    let mut queue: *mut c_void = core::ptr::null_mut();
    ddvk::vkGetDeviceQueue(dev, 0, 0, &mut queue);
    let pool_ci = vk::CommandPoolCreateInfo::default();
    let mut pool: u64 = 0;
    assert_eq!(ddvk::vkCreateCommandPool(dev, &pool_ci, core::ptr::null(), &mut pool), 0);

    // --- buffers a, b (written), c (output) ---
    let ha: Vec<f32> = (0..N).map(|i| i as f32).collect();
    let hb: Vec<f32> = (0..N).map(|i| (N - i) as f32 * 0.5).collect();
    let bytes = |v: &[f32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    let sz = (N * 4) as u64;
    let (buf_a, _) = unsafe { make_buffer(dev, sz, Some(&bytes(&ha))) };
    let (buf_b, _) = unsafe { make_buffer(dev, sz, Some(&bytes(&hb))) };
    let (buf_c, ir_c) = unsafe { make_buffer(dev, sz, None) };

    // --- SPIR-V compute shader + pipeline ---
    let spirv = vecadd_spirv();
    assert_eq!(spirv.first().copied(), Some(0x0723_0203), "payload is real SPIR-V");
    let sm_ci = vk::ShaderModuleCreateInfo::default().code(&spirv);
    let mut shader: u64 = 0;
    assert_eq!(ddvk::vkCreateShaderModule(dev, &sm_ci, core::ptr::null(), &mut shader), 0);

    let layout_ci = vk::PipelineLayoutCreateInfo::default();
    let mut layout: u64 = 0;
    assert_eq!(
        ddvk::vkCreatePipelineLayout(dev, (&layout_ci as *const _) as *const c_void, core::ptr::null(), &mut layout),
        0
    );

    let stage = vk::PipelineShaderStageCreateInfo::default()
        .stage(vk::ShaderStageFlags::COMPUTE)
        .module(vk::ShaderModule::from_raw(shader))
        .name(c"main");
    let cp_ci = vk::ComputePipelineCreateInfo::default()
        .stage(stage)
        .layout(vk::PipelineLayout::from_raw(layout));
    let mut pipeline: u64 = 0;
    assert_eq!(
        ddvk::vkCreateComputePipelines(dev, 0, 1, &cp_ci, core::ptr::null(), &mut pipeline),
        0
    );

    // --- descriptor set: bindings 0/1/2 -> a/b/c ---
    let dsl_ci = vk::DescriptorSetLayoutCreateInfo::default();
    let mut dsl: u64 = 0;
    assert_eq!(
        ddvk::vkCreateDescriptorSetLayout(dev, (&dsl_ci as *const _) as *const c_void, core::ptr::null(), &mut dsl),
        0
    );
    let dp_ci = vk::DescriptorPoolCreateInfo::default();
    let mut pool_d: u64 = 0;
    assert_eq!(
        ddvk::vkCreateDescriptorPool(dev, (&dp_ci as *const _) as *const c_void, core::ptr::null(), &mut pool_d),
        0
    );
    let set_layouts = [vk::DescriptorSetLayout::from_raw(dsl)];
    let ds_ai = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(vk::DescriptorPool::from_raw(pool_d))
        .set_layouts(&set_layouts);
    let mut dset: u64 = 0;
    assert_eq!(ddvk::vkAllocateDescriptorSets(dev, &ds_ai, &mut dset), 0);

    let bi = |b: u64| [vk::DescriptorBufferInfo::default().buffer(vk::Buffer::from_raw(b)).offset(0).range(sz)];
    let (ba, bb, bc) = (bi(buf_a), bi(buf_b), bi(buf_c));
    let writes = [
        vk::WriteDescriptorSet::default().dst_set(vk::DescriptorSet::from_raw(dset)).dst_binding(0).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&ba),
        vk::WriteDescriptorSet::default().dst_set(vk::DescriptorSet::from_raw(dset)).dst_binding(1).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&bb),
        vk::WriteDescriptorSet::default().dst_set(vk::DescriptorSet::from_raw(dset)).dst_binding(2).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).buffer_info(&bc),
    ];
    ddvk::vkUpdateDescriptorSets(dev, writes.len() as u32, writes.as_ptr(), 0, core::ptr::null());

    // --- record + submit the dispatch ---
    let cb_ai = vk::CommandBufferAllocateInfo::default()
        .command_pool(vk::CommandPool::from_raw(pool))
        .command_buffer_count(1);
    let mut cb: *mut c_void = core::ptr::null_mut();
    assert_eq!(ddvk::vkAllocateCommandBuffers(dev, &cb_ai, &mut cb), 0);

    let begin = vk::CommandBufferBeginInfo::default();
    assert_eq!(ddvk::vkBeginCommandBuffer(cb, (&begin as *const _) as *const vk::CommandBufferBeginInfo), 0);
    ddvk::vkCmdBindPipeline(cb, vk::PipelineBindPoint::COMPUTE.as_raw(), pipeline);
    let sets = [dset];
    ddvk::vkCmdBindDescriptorSets(
        cb,
        vk::PipelineBindPoint::COMPUTE.as_raw(),
        layout,
        0,
        1,
        sets.as_ptr(),
        0,
        core::ptr::null(),
    );
    ddvk::vkCmdDispatch(cb, (N as u32).div_ceil(64), 1, 1);
    assert_eq!(ddvk::vkEndCommandBuffer(cb), 0);

    let submit = vk::SubmitInfo::default();
    // command_buffer_count/p_command_buffers set manually (raw u64->*mut c_void handle array).
    let cbs = [cb];
    let submit = vk::SubmitInfo { command_buffer_count: 1, p_command_buffers: cbs.as_ptr() as *const vk::CommandBuffer, ..submit };
    assert_eq!(ddvk::vkQueueSubmit(queue, 1, &submit, 0), 0);
    assert_eq!(ddvk::vkQueueWaitIdle(queue), 0);

    // --- replay the shim-produced IR on real Metal, then read back c ---
    let ir = ddvk::reg::take_ir();
    eprintln!("vk_compute: shim produced {} IR commands", ir.len());
    let bytes_ir = dd_gpu::ir::encode_stream(&ir);
    let mut be = WgpuBackend::new().expect("wgpu Metal backend");
    dd_gpu::replay::replay_stream(&mut be, &bytes_ir).expect("replay vk compute IR on Metal");

    let mut out = vec![0u8; N * 4];
    be.read_buffer(BufferId(ir_c), 0, &mut out).expect("DtoH c");
    let mut mism = 0;
    for i in 0..N {
        let got = f32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
        let want = ha[i] + hb[i];
        if got != want {
            if mism < 8 {
                eprintln!("mismatch c[{i}]: got {got} want {want}");
            }
            mism += 1;
        }
    }
    assert_eq!(mism, 0, "Vulkan vecadd on real Metal: {mism} mismatched elements");
    let c3 = f32::from_le_bytes(out[12..16].try_into().unwrap());
    assert_eq!(c3, 513.5, "c[3] = 3 + (1024-3)*0.5");
    eprintln!("vk_vecadd_runs_on_real_metal: OK (n={N}, c[3]={c3}, ir_cmds={})", ir.len());
}
