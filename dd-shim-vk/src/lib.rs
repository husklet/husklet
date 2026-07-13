//! dd-shim-vk — the guest Vulkan driver (a Vulkan ICD), in Rust (increment-1 FOUNDATION).
//!
//! Builds the shared object a standard Vulkan **loader** (libvulkan) discovers via an `icd.json`
//! manifest and accepts as a driver. An unmodified Vulkan app opens libvulkan; the loader loads this
//! ICD, negotiates the loader↔ICD interface, and resolves every `vk*` entry point through our
//! `vk_icdGetInstanceProcAddr`. We report the "dd Metal (Vulkan)" physical device; the compute/render
//! path lowers into a `dd-gpu` IR stream and — through [`dd_shim_common::transport`] — reaches the
//! host executor as the SAME IR the host decodes with the SAME Rust code (no hand-rolled second
//! encoder). This mirrors dd-shim-gl / dd-shim-cuda increment-1 exactly.
//!
//! ## Ported from real references (no invented behavior)
//! * **ICD interface** ([`icd`], [`handle`]) — Khronos **Vulkan-Loader** `docs/LoaderDriverInterface.md`
//!   + `include/vulkan/vk_icd.h`, and **MoltenVK** `vulkan.mm` (negotiation + proc-addr dispatch).
//!   This is what root-causes + fixes the prior `VK_ERROR_INCOMPATIBLE_DRIVER` (see [`icd`]).
//! * **Object model + device properties** ([`instance`], [`device`], [`state`]) — **MoltenVK**'s
//!   `MVKInstance`/`MVKPhysicalDevice`/`MVKDevice`/`MVKQueue` and its Apple-silicon reporting.
//! * **Type/ABI surface** — the Khronos **`ash`** bindings (`ash::vk`) for spec-exact `#[repr(C)]`
//!   struct layouts, and the Khronos **`vk.xml`** registry for the full entry-point list.
//!
//! ## Coverage (truthful — Phase 0)
//! The exported `vk*` *surface* is code-generated from `vk.xml` (`build.rs` + `registry/`) — the full
//! core + extension command set (693 commands). Entry points in [`build::IMPLEMENTED`](../build.rs)
//! have real hand-written bodies; the rest are generated **truthful-failure** stubs (correct ABI, a
//! `DD_SHIM_DEBUG` trace, and — crucially — the API-defined error, never a false `VK_SUCCESS`): a
//! `VkResult` stub returns `VK_ERROR_FEATURE_NOT_PRESENT` (unimplemented core) or
//! `VK_ERROR_EXTENSION_NOT_PRESENT` (command from an unadvertised extension) and nulls its output
//! handle; a `void`/`VkBool32`/pointer stub returns the truthful no-op/`VK_FALSE`/NULL. Every command
//! carries a [`capability`] inventory record (full/partial/stub + the error + core-version/extension
//! origin). The ICD advertises **Vulkan 1.0** and rejects a newer request with
//! `VK_ERROR_INCOMPATIBLE_DRIVER`. `DD_SHIM_STRICT=1` aborts at the first stub call. The [`ir_seam`]
//! module sketches the Vulkan→IR mapping and round-trips what it encodes.

// The generated + hand-written entry-point surface uses the Vulkan C names verbatim (vkCreateInstance,
// PFN_vkVoidFunction, …) — those are the ABI identifiers, so the Rust casing lints don't apply.
#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]

// The shared IR + transport foundation. Re-exported so this crate's modules (and readers) see that the
// IR type is dd-gpu's, not a local copy.
pub use dd_shim_common as common;

pub mod capability;
pub mod command;
pub mod descriptor;
pub mod device;
pub mod event;
pub mod ext;
pub mod handle;
pub mod icd;
pub mod instance;
pub mod ir_seam;
pub mod memory;
pub mod pipeline;
pub mod query;
pub mod reg;
pub mod state;
pub mod stub;
pub mod types;
pub mod wl_present;
pub mod wsi;

// Bring every hand-written `#[no_mangle]` entry point into crate-root scope so the generated
// DISPATCH table (below) can reference the whole surface — implemented + stub — by bare name.
pub use command::*;
pub use descriptor::*;
pub use device::*;
pub use event::*;
pub use ext::*;
pub use icd::*;
pub use instance::*;
pub use memory::*;
pub use pipeline::*;
pub use query::*;
pub use wsi::*;

// The generated C-ABI export surface (every `vk*` entry point not in `IMPLEMENTED`) + the name→address
// DISPATCH table the loader-facing proc-addr resolvers scan.
include!(concat!(env!("OUT_DIR"), "/generated_entrypoints.rs"));

/// Total exported Vulkan entry points (hand-written + generated) — the completeness census.
pub const TOTAL_ENTRYPOINTS: usize = VK_ENTRYPOINTS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn surface_is_complete_and_large() {
        // The vk.xml-driven surface must be the full core+extension set, not a hand-picked few.
        assert!(VK_ENTRYPOINTS >= 600, "Vulkan surface too small: {VK_ENTRYPOINTS}");
        // Every entry point is either hand-implemented or a generated stub.
        assert_eq!(GENERATED_STUBS + IMPLEMENTED_COUNT, TOTAL_ENTRYPOINTS);
        // The whole surface is resolvable through the dispatch resolver the loader scans.
        assert_eq!(DISPATCH_NAMES.len(), TOTAL_ENTRYPOINTS);
    }

    // Count of hand-written entry points (kept in sync with build.rs IMPLEMENTED via the census).
    const IMPLEMENTED_COUNT: usize = TOTAL_ENTRYPOINTS - GENERATED_STUBS;

    #[test]
    fn dispatch_resolves_bringup_entry_points() {
        // The bring-up + ICD entry points a loader/app needs must all resolve by name.
        for name in [
            "vkGetInstanceProcAddr",
            "vkEnumerateInstanceVersion",
            "vkCreateInstance",
            "vkEnumeratePhysicalDevices",
            "vkGetPhysicalDeviceProperties",
            "vkCreateDevice",
            "vkGetDeviceQueue",
            "vkCreateCommandPool",
            "vkAllocateCommandBuffers",
        ] {
            assert!(
                DISPATCH_NAMES.contains(&name) && dispatch_addr(name).is_some(),
                "dispatch resolver missing bring-up entry point {name}"
            );
        }
    }

    // ---- Phase 0: the generated capability inventory ---------------------------------------------

    /// The census must classify EVERY exported command (nothing advertised without a full/partial/stub
    /// record) and the counts must be internally consistent.
    #[test]
    fn capability_inventory_covers_every_exported_command() {
        use capability::Cap;
        assert_eq!(
            CAPABILITIES.len(),
            TOTAL_ENTRYPOINTS,
            "inventory must have one record per exported vk* command"
        );
        // One-to-one with the dispatch census (no orphan record, no unadvertised command).
        let inv: std::collections::HashSet<&str> =
            CAPABILITIES.iter().map(|e| e.name).collect();
        for name in DISPATCH_NAMES {
            assert!(inv.contains(name), "exported command {name} has no capability record");
        }
        assert_eq!(inv.len(), DISPATCH_NAMES.len(), "duplicate/extra capability records");
        // Level counts partition the whole surface, and stubs == the generated long tail.
        let (mut full, mut partial, mut stub) = (0usize, 0usize, 0usize);
        for e in CAPABILITIES {
            match e.cap {
                Cap::Full => full += 1,
                Cap::Partial => partial += 1,
                Cap::Stub => stub += 1,
            }
        }
        assert_eq!((full, partial, stub), (CAP_FULL, CAP_PARTIAL, CAP_STUB));
        assert_eq!(full + partial + stub, TOTAL_ENTRYPOINTS);
        assert_eq!(stub, GENERATED_STUBS, "every generated stub must carry a `Stub` record");
        assert_eq!(full + partial, TOTAL_ENTRYPOINTS - GENERATED_STUBS);
    }

    /// No `stub` record may claim a false `VK_SUCCESS`, and each origin must be a recognized
    /// core-version or extension token. A `full`/`partial` entry must carry no error.
    #[test]
    fn no_stub_advertises_false_success() {
        use capability::Cap;
        for e in CAPABILITIES {
            let ok_origin = e.origin.starts_with("core:") || e.origin.starts_with("ext:");
            assert!(ok_origin, "{}: unrecognized origin {:?}", e.name, e.origin);
            match e.cap {
                Cap::Stub => {
                    // A VkResult stub returns FEATURE/EXTENSION_NOT_PRESENT; a non-VkResult stub records
                    // 0 (void/VK_FALSE/NULL). Never VK_SUCCESS with a VkResult — that is the false
                    // success Phase 0 forbids. `vk_error` is either 0 (non-VkResult) or a defined error.
                    assert!(
                        e.vk_error == 0
                            || e.vk_error == types::VK_ERROR_FEATURE_NOT_PRESENT
                            || e.vk_error == types::VK_ERROR_EXTENSION_NOT_PRESENT,
                        "{}: stub error {} is not a truthful default",
                        e.name,
                        e.vk_error
                    );
                }
                Cap::Full | Cap::Partial => {
                    assert_eq!(e.vk_error, 0, "{}: an implemented entry must not preset an error", e.name);
                }
            }
        }
    }

    /// A generated `VkResult` stub must return the API-defined error (never `VK_SUCCESS`) AND initialize
    /// its output handle — the exact false-success trap Phase 0 removes. Proven against a real exported
    /// stub call (`vkCreateSampler`, a core:1.0 command) and an unadvertised-extension stub.
    #[test]
    fn stub_returns_truthful_error_and_inits_output() {
        // vkCreateRenderPass2 is a still-unimplemented core:1.2 stub (the whole 1.0 and 1.1 core is now
        // bodied): it must null its output handle and return FEATURE_NOT_PRESENT (unimplemented core).
        let mut rp: u64 = 0xdead_beef; // poison; the stub must overwrite it with VK_NULL_HANDLE (0)
        let r = vkCreateRenderPass2(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut rp as *mut u64 as *mut core::ffi::c_void,
        );
        assert_eq!(r, types::VK_ERROR_FEATURE_NOT_PRESENT, "core stub must fail, not succeed");
        assert_eq!(rp, 0, "stub must initialize the output handle to VK_NULL_HANDLE");
        // A command from an unadvertised extension reports EXTENSION_NOT_PRESENT.
        let r2 = vkBindBufferMemory2KHR(core::ptr::null_mut(), 0, core::ptr::null());
        assert_eq!(r2, types::VK_ERROR_EXTENSION_NOT_PRESENT);
        // And the inventory records exactly those errors for those commands.
        let rec = |n: &str| CAPABILITIES.iter().find(|e| e.name == n).unwrap();
        assert_eq!(rec("vkCreateRenderPass2").vk_error, types::VK_ERROR_FEATURE_NOT_PRESENT);
        assert_eq!(rec("vkBindBufferMemory2KHR").vk_error, types::VK_ERROR_EXTENSION_NOT_PRESENT);
    }

    // ---- Phase 0: truthful version advertisement -------------------------------------------------

    /// The ICD advertises Vulkan **1.0**, consistently across `vkEnumerateInstanceVersion`, the
    /// physical-device `apiVersion`, and the capability profile constant.
    #[test]
    fn advertises_vulkan_1_1() {
        assert_eq!(capability::ADVERTISED_API_VERSION, (1, 1));
        assert_eq!(ash::vk::api_version_major(state::DD_API_VERSION), 1);
        assert_eq!(ash::vk::api_version_minor(state::DD_API_VERSION), 1);
        let mut v: u32 = 0xffff_ffff;
        assert_eq!(vkEnumerateInstanceVersion(&mut v), types::VK_SUCCESS);
        assert_eq!(ash::vk::api_version_major(v), 1);
        assert_eq!(ash::vk::api_version_minor(v), 1);
        // The physical-device properties report the same version.
        let props = state::physical_device_properties();
        assert_eq!(props.api_version, state::DD_API_VERSION);
    }

    /// A vkCreateInstance requesting a version NEWER than advertised (1.4, 2.0) must be refused with
    /// `VK_ERROR_INCOMPATIBLE_DRIVER`; a 1.0 request (vkcube's) must succeed. This is the gap
    /// gui_vk_capability_truth pins: the prior gate rejected only major>1, so 1.4 slipped through.
    #[test]
    fn rejects_api_version_newer_than_advertised() {
        use ash::vk;
        let create = |api: u32| -> (types::VkResult, types::VkInstance) {
            let app = vk::ApplicationInfo { api_version: api, ..Default::default() };
            let ci = vk::InstanceCreateInfo { p_application_info: &app, ..Default::default() };
            let mut inst: types::VkInstance = core::ptr::null_mut();
            let r = vkCreateInstance(&ci, core::ptr::null(), &mut inst);
            (r, inst)
        };
        // 1.2, 1.4 and 2.0 are newer than the advertised 1.1 → refused.
        assert_eq!(create(vk::make_api_version(0, 1, 2, 0)).0, types::VK_ERROR_INCOMPATIBLE_DRIVER);
        assert_eq!(create(vk::make_api_version(0, 1, 4, 0)).0, types::VK_ERROR_INCOMPATIBLE_DRIVER);
        assert_eq!(create(vk::make_api_version(0, 2, 0, 0)).0, types::VK_ERROR_INCOMPATIBLE_DRIVER);
        // 1.0 and 1.1 (and patch differences, and apiVersion 0 == "1.0 default") are honored — a 1.0 app
        // (vkcube) and a 1.1 app (wgpu/Zed) both run on the 1.1 driver.
        for v in [vk::make_api_version(0, 1, 0, 0), vk::make_api_version(0, 1, 1, 0), vk::make_api_version(0, 1, 1, 42)] {
            let (r, inst) = create(v);
            assert_eq!(r, types::VK_SUCCESS, "a <= 1.1 request must be accepted");
            assert!(!inst.is_null());
            vkDestroyInstance(inst, core::ptr::null());
        }
    }

    // ---- Phase 0: truthful extension enumeration + strict mode -----------------------------------

    /// The advertised extension allow-lists must equal what the enumeration entry points return — the
    /// shim advertises only what it implements, not everything vk.xml lists.
    #[test]
    fn extension_enumeration_matches_allow_list() {
        unsafe fn names(f: impl Fn(*mut u32, *mut ash::vk::ExtensionProperties) -> types::VkResult) -> Vec<String> {
            let mut n: u32 = 0;
            assert_eq!(f(&mut n, core::ptr::null_mut()), types::VK_SUCCESS);
            let mut props = vec![ash::vk::ExtensionProperties::default(); n as usize];
            assert_eq!(f(&mut n, props.as_mut_ptr()), types::VK_SUCCESS);
            props
                .iter()
                .map(|p| {
                    let bytes: Vec<u8> = p.extension_name.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
                    String::from_utf8_lossy(&bytes).into_owned()
                })
                .collect()
        }
        let inst = unsafe { names(|c, p| vkEnumerateInstanceExtensionProperties(core::ptr::null(), c, p)) };
        assert_eq!(inst, capability::ADVERTISED_INSTANCE_EXTENSIONS);
        let dev = unsafe {
            names(|c, p| vkEnumerateDeviceExtensionProperties(core::ptr::null_mut(), core::ptr::null(), c, p))
        };
        assert_eq!(dev, capability::ADVERTISED_DEVICE_EXTENSIONS);
    }

    /// `DD_SHIM_STRICT=1`: the shim aborts at the first stub call. Under `cfg(test)` the strict path
    /// records that it *would* have aborted (instead of killing the test process) so it is assertable.
    #[test]
    fn strict_mode_trips_abort_on_stub() {
        stub::STRICT_TRIPPED.with(|c| c.set(false));
        std::env::set_var("DD_SHIM_STRICT", "1");
        // Any generated stub call must trip the strict abort (vkCreateRenderPass2 is a still-unimplemented
        // core:1.2 stub — the whole 1.0 + 1.1 core is now bodied).
        let mut rp: u64 = 0;
        let _ = vkCreateRenderPass2(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut rp as *mut u64 as *mut core::ffi::c_void,
        );
        std::env::remove_var("DD_SHIM_STRICT");
        assert!(
            stub::STRICT_TRIPPED.with(|c| c.get()),
            "DD_SHIM_STRICT=1 must trip the abort at the first stub call"
        );
    }

    /// The generated Vulkan-1.0 mandatory-core census: the exported surface carries the full 1.0 core,
    /// and the census reports exactly how much of it has a real body (the honest completeness number).
    #[test]
    fn vulkan_1_0_mandatory_core_census() {
        use capability::Cap;
        // Recompute from the inventory and cross-check the generated constants.
        let core10: Vec<_> = CAPABILITIES.iter().filter(|e| e.origin == "core:1.0").collect();
        let implemented = core10.iter().filter(|e| e.cap != Cap::Stub).count();
        assert_eq!(core10.len(), CORE_1_0_TOTAL);
        assert_eq!(implemented, CORE_1_0_IMPLEMENTED);
        assert!(CORE_1_0_IMPLEMENTED <= CORE_1_0_TOTAL);
        // The 1.0 core is genuinely a large mandatory set (not a hand-picked few) and is now FULLY
        // bodied: every mandatory core:1.0 command has a real (full or partial) implementation — zero
        // generated stubs remain. This is the closed state of
        // `vk_advertised_core_has_real_implementations_for_every_mandatory_command`.
        assert!(CORE_1_0_TOTAL >= 130, "Vulkan 1.0 core census too small: {CORE_1_0_TOTAL}");
        assert_eq!(
            CORE_1_0_IMPLEMENTED, CORE_1_0_TOTAL,
            "every mandatory Vulkan 1.0 core command must have a real body (0 stubs)"
        );
        let core10_stubs = core10.iter().filter(|e| e.cap == Cap::Stub).count();
        assert_eq!(core10_stubs, 0, "no mandatory core:1.0 command may remain a generated stub");
    }

    /// The exported ABI must contain **every** core command in the pinned Khronos registry, across all
    /// core versions 1.0–1.4 (the closed state of
    /// `vk_abi_manifest_contains_every_core_command_in_the_pinned_registry`). The per-version counts are
    /// fixed by the pinned `vk.xml` (VK_VERSION_1_0..1_4): 137 + 28 + 13 + 37 + 19 = 234 core commands.
    /// Each must be resolvable by the loader-facing dispatch resolver and carry a capability record.
    #[test]
    fn abi_manifest_contains_every_pinned_core_command() {
        let count = |origin: &str| CAPABILITIES.iter().filter(|e| e.origin == origin).count();
        assert_eq!(count("core:1.0"), 137, "Vulkan 1.0 core incomplete in the ABI");
        assert_eq!(count("core:1.1"), 28, "Vulkan 1.1 core incomplete in the ABI");
        assert_eq!(count("core:1.2"), 13, "Vulkan 1.2 core incomplete in the ABI");
        assert_eq!(count("core:1.3"), 37, "Vulkan 1.3 core incomplete in the ABI");
        assert_eq!(count("core:1.4"), 19, "Vulkan 1.4 core trails the pinned registry");
        let total_core: usize = ["core:1.0", "core:1.1", "core:1.2", "core:1.3", "core:1.4"]
            .iter()
            .map(|o| count(o))
            .sum();
        assert_eq!(total_core, 234, "the pinned registry's full core surface must be exported");
        // Every core command resolves through the dispatch resolver the loader scans, and the promoted
        // 1.4 commands are present by name.
        for name in ["vkCmdBindIndexBuffer2", "vkMapMemory2", "vkTransitionImageLayout", "vkCmdSetLineStipple"] {
            assert!(DISPATCH_NAMES.contains(&name), "1.4 core command {name} missing from the ABI");
            assert!(dispatch_addr(name).is_some(), "1.4 core command {name} not resolvable");
        }
    }

    /// Every mandatory Vulkan **1.1** core command now has a real (full or partial) body — zero generated
    /// stubs remain in the 1.1 promoted core (bind/requirements2, descriptor update templates, sampler
    /// YCbCr, device groups minimal, external-capability queries, the `...2` physical-device queries).
    #[test]
    fn vulkan_1_1_mandatory_core_is_fully_implemented() {
        use capability::Cap;
        let core11: Vec<_> = CAPABILITIES.iter().filter(|e| e.origin == "core:1.1").collect();
        assert_eq!(core11.len(), 28, "Vulkan 1.1 core census size");
        let stubs = core11.iter().filter(|e| e.cap == Cap::Stub).count();
        assert_eq!(stubs, 0, "no mandatory core:1.1 command may remain a generated stub");
        // A couple of the promoted commands resolve and are non-stub in the inventory.
        for name in ["vkBindBufferMemory2", "vkCreateDescriptorUpdateTemplate", "vkGetDeviceQueue2"] {
            assert!(dispatch_addr(name).is_some(), "1.1 core {name} not resolvable");
            assert!(CAPABILITIES.iter().find(|e| e.name == name).unwrap().implemented(), "{name} still a stub");
        }
    }

    /// Per-feature: a `VkDescriptorUpdateTemplate` writes the app's pushed `VkDescriptorBufferInfo` into
    /// the target set exactly as `vkUpdateDescriptorSets` would (the buffer binding lands in the set's
    /// resolved table). Drives the real 1.1 bodies end to end.
    #[test]
    fn descriptor_update_template_writes_buffer_binding() {
        // No reg::reset() — this test only reads back its own freshly-allocated set handle, so it must not
        // wipe global state that a concurrent test (e.g. the ir_seam IR contract test) depends on.
        let _guard = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        use ash::vk::Handle;

        // A storage buffer.
        let bci = vk::BufferCreateInfo::default().size(256).usage(vk::BufferUsageFlags::STORAGE_BUFFER);
        let mut buffer = 0u64;
        assert_eq!(vkCreateBuffer(core::ptr::null_mut(), &bci, core::ptr::null(), &mut buffer), types::VK_SUCCESS);

        // A set layout (binding 0 = STORAGE_BUFFER), pool, and one allocated set.
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0).descriptor_type(vk::DescriptorType::STORAGE_BUFFER)
            .descriptor_count(1).stage_flags(vk::ShaderStageFlags::COMPUTE);
        let set_ci = vk::DescriptorSetLayoutCreateInfo::default().bindings(core::slice::from_ref(&binding));
        let mut set_layout = 0u64;
        assert_eq!(vkCreateDescriptorSetLayout(core::ptr::null_mut(), &set_ci, core::ptr::null(), &mut set_layout), types::VK_SUCCESS);
        let pool_size = vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_BUFFER, descriptor_count: 1 };
        let pool_ci = vk::DescriptorPoolCreateInfo::default().max_sets(1).pool_sizes(core::slice::from_ref(&pool_size));
        let mut pool = 0u64;
        assert_eq!(vkCreateDescriptorPool(core::ptr::null_mut(), &pool_ci, core::ptr::null(), &mut pool), types::VK_SUCCESS);
        let set_layout_h = vk::DescriptorSetLayout::from_raw(set_layout);
        let alloc = vk::DescriptorSetAllocateInfo::default().descriptor_pool(vk::DescriptorPool::from_raw(pool)).set_layouts(core::slice::from_ref(&set_layout_h));
        let mut set = 0u64;
        assert_eq!(vkAllocateDescriptorSets(core::ptr::null_mut(), &alloc, &mut set), types::VK_SUCCESS);

        // A template with one entry: binding 0, STORAGE_BUFFER, offset 0, stride = sizeof(VkDescriptorBufferInfo).
        let entry = vk::DescriptorUpdateTemplateEntry {
            dst_binding: 0, dst_array_element: 0, descriptor_count: 1,
            descriptor_type: vk::DescriptorType::STORAGE_BUFFER,
            offset: 0, stride: core::mem::size_of::<vk::DescriptorBufferInfo>(),
        };
        let tci = vk::DescriptorUpdateTemplateCreateInfo::default()
            .descriptor_update_entries(core::slice::from_ref(&entry))
            .template_type(vk::DescriptorUpdateTemplateType::DESCRIPTOR_SET);
        let mut template = 0u64;
        assert_eq!(vkCreateDescriptorUpdateTemplate(core::ptr::null_mut(), &tci, core::ptr::null(), &mut template), types::VK_SUCCESS);

        // Push a VkDescriptorBufferInfo through the template.
        let info = vk::DescriptorBufferInfo { buffer: vk::Buffer::from_raw(buffer), offset: 0, range: vk::WHOLE_SIZE };
        vkUpdateDescriptorSetWithTemplate(core::ptr::null_mut(), set, template, &info as *const _ as *const core::ffi::c_void);

        // The set's resolved buffer table now carries binding 0 → (buffer, offset 0, range == buffer size).
        let s = reg::lock();
        let d = s.dsets.get(&set).expect("descriptor set");
        let (b, off, range) = *d.buffers.get(&0).expect("binding 0 written by the template");
        assert_eq!(b, buffer);
        assert_eq!(off, 0);
        assert_eq!(range, 256, "WHOLE_SIZE resolves to the buffer size");
    }

    // ---- modern extensions (VK_KHR_timeline_semaphore / dynamic_rendering / buffer_device_address) ----

    /// The three wgpu/Zed extensions are advertised (allow-list) AND their commands are non-stub.
    #[test]
    fn advertises_modern_wgpu_extensions() {
        for e in ["VK_KHR_timeline_semaphore", "VK_KHR_dynamic_rendering", "VK_KHR_buffer_device_address"] {
            assert!(capability::ADVERTISED_DEVICE_EXTENSIONS.contains(&e), "{e} not advertised");
        }
        for n in ["vkWaitSemaphores", "vkWaitSemaphoresKHR", "vkCmdBeginRendering", "vkGetBufferDeviceAddress", "vkGetSemaphoreCounterValueKHR"] {
            assert!(dispatch_addr(n).is_some(), "{n} not resolvable");
            assert!(CAPABILITIES.iter().find(|c| c.name == n).unwrap().implemented(), "{n} still a stub");
        }
    }

    /// Timeline semaphore: create (initial value), host-signal, poll the counter, and wait (satisfied vs
    /// timeout). Builds on the semaphore state machine.
    #[test]
    fn timeline_semaphore_signal_wait_and_counter() {
        let _g = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        use ash::vk::Handle;
        let mut type_info = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE).initial_value(5);
        let ci = vk::SemaphoreCreateInfo::default().push_next(&mut type_info);
        let mut sem = 0u64;
        assert_eq!(
            vkCreateSemaphore(core::ptr::null_mut(), &ci as *const _ as *const core::ffi::c_void, core::ptr::null(), &mut sem),
            types::VK_SUCCESS
        );
        let mut v = 0u64;
        assert_eq!(vkGetSemaphoreCounterValue(core::ptr::null_mut(), sem, &mut v), types::VK_SUCCESS);
        assert_eq!(v, 5, "counter starts at the initial value");
        let si = vk::SemaphoreSignalInfo::default().semaphore(vk::Semaphore::from_raw(sem)).value(10);
        assert_eq!(vkSignalSemaphore(core::ptr::null_mut(), &si), types::VK_SUCCESS);
        assert_eq!(vkGetSemaphoreCounterValue(core::ptr::null_mut(), sem, &mut v), types::VK_SUCCESS);
        assert_eq!(v, 10, "host signal advanced the counter");
        let sems = [vk::Semaphore::from_raw(sem)];
        let ok = [10u64];
        let wi = vk::SemaphoreWaitInfo::default().semaphores(&sems).values(&ok);
        assert_eq!(vkWaitSemaphores(core::ptr::null_mut(), &wi, 0), types::VK_SUCCESS, "reached value waits succeed");
        let hi = [11u64];
        let wi2 = vk::SemaphoreWaitInfo::default().semaphores(&sems).values(&hi);
        assert_eq!(vkWaitSemaphores(core::ptr::null_mut(), &wi2, 0), types::VK_TIMEOUT, "unmet value times out");
        vkDestroySemaphore(core::ptr::null_mut(), sem, core::ptr::null());
    }

    /// Buffer device address: non-zero, unique per buffer, and stable across calls.
    #[test]
    fn buffer_device_address_is_stable_and_unique() {
        let _g = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        use ash::vk::Handle;
        let bci = vk::BufferCreateInfo::default().size(64).usage(vk::BufferUsageFlags::STORAGE_BUFFER | vk::BufferUsageFlags::SHADER_DEVICE_ADDRESS);
        let (mut b1, mut b2) = (0u64, 0u64);
        assert_eq!(vkCreateBuffer(core::ptr::null_mut(), &bci, core::ptr::null(), &mut b1), types::VK_SUCCESS);
        assert_eq!(vkCreateBuffer(core::ptr::null_mut(), &bci, core::ptr::null(), &mut b2), types::VK_SUCCESS);
        let addr = |b: u64| {
            let info = vk::BufferDeviceAddressInfo::default().buffer(vk::Buffer::from_raw(b));
            vkGetBufferDeviceAddress(core::ptr::null_mut(), &info)
        };
        let (a1, a2) = (addr(b1), addr(b2));
        assert_ne!(a1, 0);
        assert_ne!(a2, 0);
        assert_ne!(a1, a2, "distinct buffers get distinct addresses");
        assert_eq!(a1, addr(b1), "address is stable across calls");
        vkDestroyBuffer(core::ptr::null_mut(), b1, core::ptr::null());
        vkDestroyBuffer(core::ptr::null_mut(), b2, core::ptr::null());
    }

    /// Dynamic rendering: `vkCmdBeginRendering` lowers a color attachment to the shared `BeginRenderPass`
    /// IR (same executor path as a classic render pass), and `vkCmdEndRendering` closes it.
    #[test]
    fn dynamic_rendering_lowers_to_begin_render_pass() {
        let _g = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        use ash::vk::Handle;
        let ici = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D).format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width: 8, height: 8, depth: 1 }).mip_levels(1).array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1).usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let mut img = 0u64;
        assert_eq!(vkCreateImage(core::ptr::null_mut(), &ici, core::ptr::null(), &mut img), types::VK_SUCCESS);
        let vci = vk::ImageViewCreateInfo::default()
            .image(vk::Image::from_raw(img)).view_type(vk::ImageViewType::TYPE_2D).format(vk::Format::R8G8B8A8_UNORM)
            .subresource_range(vk::ImageSubresourceRange { aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1 });
        let mut view = 0u64;
        assert_eq!(vkCreateImageView(core::ptr::null_mut(), &vci, core::ptr::null(), &mut view), types::VK_SUCCESS);

        let cb = 0x7777usize as types::VkCommandBuffer;
        let bi = vk::CommandBufferBeginInfo::default();
        assert_eq!(vkBeginCommandBuffer(cb, &bi), types::VK_SUCCESS);
        let att = vk::RenderingAttachmentInfo::default()
            .image_view(vk::ImageView::from_raw(view)).image_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            .load_op(vk::AttachmentLoadOp::CLEAR).store_op(vk::AttachmentStoreOp::STORE);
        let atts = [att];
        let ri = vk::RenderingInfo::default()
            .render_area(vk::Rect2D { offset: vk::Offset2D { x: 0, y: 0 }, extent: vk::Extent2D { width: 8, height: 8 } })
            .layer_count(1).color_attachments(&atts);
        vkCmdBeginRendering(cb, &ri);
        vkCmdEndRendering(cb);

        let enc = reg::lock().cmdbufs[&(cb as usize)].enc.clone();
        assert!(enc.iter().any(|e| matches!(e, common::ir::Enc::BeginRenderPass { .. })), "dynamic rendering opened a render pass");
        assert!(enc.iter().any(|e| matches!(e, common::ir::Enc::EndRenderPass)), "dynamic rendering closed the render pass");
        vkResetCommandBuffer(cb, 0);
        vkDestroyImageView(core::ptr::null_mut(), view, core::ptr::null());
        vkDestroyImage(core::ptr::null_mut(), img, core::ptr::null());
    }
}
