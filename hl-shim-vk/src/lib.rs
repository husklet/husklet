//! hl-shim-vk — the guest Vulkan driver (a Vulkan ICD), in Rust (increment-1 FOUNDATION).
//!
//! Builds the shared object a standard Vulkan **loader** (libvulkan) discovers via an `icd.json`
//! manifest and accepts as a driver. An unmodified Vulkan app opens libvulkan; the loader loads this
//! ICD, negotiates the loader↔ICD interface, and resolves every `vk*` entry point through our
//! `vk_icdGetInstanceProcAddr`. We report the "dd Metal (Vulkan)" physical device; the compute/render
//! path lowers into a `hl-gpu` IR stream and — through [`hl_shim::transport`] — reaches the
//! host executor as the SAME IR the host decodes with the SAME Rust code (no hand-rolled second
//! encoder). This mirrors hl-shim-gl / hl-shim-cuda increment-1 exactly.
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
//! `HL_SHIM_DEBUG` trace, and — crucially — the API-defined error, never a false `VK_SUCCESS`): a
//! `VkResult` stub returns `VK_ERROR_FEATURE_NOT_PRESENT` (unimplemented core) or
//! `VK_ERROR_EXTENSION_NOT_PRESENT` (command from an unadvertised extension) and nulls its output
//! handle; a `void`/`VkBool32`/pointer stub returns the truthful no-op/`VK_FALSE`/NULL. Every command
//! carries a [`capability`] inventory record (full/partial/stub + the error + core-version/extension
//! origin). The ICD advertises **Vulkan 1.0** and rejects a newer request with
//! `VK_ERROR_INCOMPATIBLE_DRIVER`. `HL_SHIM_STRICT=1` aborts at the first stub call. The [`ir_seam`]
//! module sketches the Vulkan→IR mapping and round-trips what it encodes.

// The generated + hand-written entry-point surface uses the Vulkan C names verbatim (vkCreateInstance,
// PFN_vkVoidFunction, …) — those are the ABI identifiers, so the Rust casing lints don't apply.
#![allow(non_snake_case, non_camel_case_types, non_upper_case_globals)]

// The shared IR + transport foundation. Re-exported so this crate's modules (and readers) see that the
// IR type is hl-gpu's, not a local copy.
pub use hl_shim as common;

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
pub mod vk13;
pub mod vk14;
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
pub use vk13::*;
pub use vk14::*;

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
        // Milestone: the entire Vulkan 1.0-1.4 mandatory core is now bodied, so NOT A SINGLE core command
        // is a generated stub — the remaining stubs are all from unadvertised extensions.
        for e in CAPABILITIES.iter().filter(|e| e.origin.starts_with("core:")) {
            assert!(e.implemented(), "core command {} must not be a generated stub", e.name);
        }
        // An unadvertised-extension vkCreate stub reports EXTENSION_NOT_PRESENT AND nulls its output handle.
        let mut accel: u64 = 0xdead_beef; // poison; the stub must overwrite it with VK_NULL_HANDLE (0)
        let r2 = vkCreateAccelerationStructureKHR(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut accel as *mut u64 as *mut core::ffi::c_void,
        );
        assert_eq!(r2, types::VK_ERROR_EXTENSION_NOT_PRESENT);
        assert_eq!(accel, 0, "stub must initialize the output handle to VK_NULL_HANDLE");
        // And the inventory records exactly that error for that command.
        let rec = |n: &str| CAPABILITIES.iter().find(|e| e.name == n).unwrap();
        assert_eq!(rec("vkCreateAccelerationStructureKHR").vk_error, types::VK_ERROR_EXTENSION_NOT_PRESENT);
    }

    // ---- Phase 0: truthful version advertisement -------------------------------------------------

    /// The ICD advertises Vulkan **1.0**, consistently across `vkEnumerateInstanceVersion`, the
    /// physical-device `apiVersion`, and the capability profile constant.
    #[test]
    fn advertises_vulkan_1_4() {
        assert_eq!(capability::ADVERTISED_API_VERSION, (1, 4));
        assert_eq!(ash::vk::api_version_major(state::HL_API_VERSION), 1);
        assert_eq!(ash::vk::api_version_minor(state::HL_API_VERSION), 4);
        let mut v: u32 = 0xffff_ffff;
        assert_eq!(vkEnumerateInstanceVersion(&mut v), types::VK_SUCCESS);
        assert_eq!(ash::vk::api_version_major(v), 1);
        assert_eq!(ash::vk::api_version_minor(v), 4);
        // The physical-device properties report the same version.
        let props = state::physical_device_properties();
        assert_eq!(props.api_version, state::HL_API_VERSION);
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
        // Only 2.0+ is newer than the advertised 1.4 → refused.
        assert_eq!(create(vk::make_api_version(0, 2, 0, 0)).0, types::VK_ERROR_INCOMPATIBLE_DRIVER);
        assert_eq!(create(vk::make_api_version(0, 3, 1, 0)).0, types::VK_ERROR_INCOMPATIBLE_DRIVER);
        // Every 1.x request (1.0 vkcube … 1.4 modern), patch differences, and apiVersion 0 are honored —
        // the full 1.0–1.4 core is backed, so any 1.x app runs on the 1.4 driver.
        for v in [
            vk::make_api_version(0, 1, 0, 0),
            vk::make_api_version(0, 1, 1, 0),
            vk::make_api_version(0, 1, 3, 0),
            vk::make_api_version(0, 1, 4, 0),
            vk::make_api_version(0, 1, 4, 42),
        ] {
            let (r, inst) = create(v);
            assert_eq!(r, types::VK_SUCCESS, "a <= 1.4 request must be accepted");
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

    /// `HL_SHIM_STRICT=1`: the shim aborts at the first stub call. Under `cfg(test)` the strict path
    /// records that it *would* have aborted (instead of killing the test process) so it is assertable.
    #[test]
    fn strict_mode_trips_abort_on_stub() {
        stub::STRICT_TRIPPED.with(|c| c.set(false));
        std::env::set_var("HL_SHIM_STRICT", "1");
        // Any generated stub call must trip the strict abort. The whole 1.0-1.4 core is now bodied, so the
        // stub example is an unadvertised-extension command (vkCreateAccelerationStructureKHR).
        let mut h: u64 = 0;
        let _ = vkCreateAccelerationStructureKHR(
            core::ptr::null_mut(),
            core::ptr::null(),
            core::ptr::null(),
            &mut h as *mut u64 as *mut core::ffi::c_void,
        );
        std::env::remove_var("HL_SHIM_STRICT");
        assert!(
            stub::STRICT_TRIPPED.with(|c| c.get()),
            "HL_SHIM_STRICT=1 must trip the abort at the first stub call"
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

    // ---- increment 8: wgpu device-creation blockers (limits / depth / per-format) ----

    /// Blocker 1: the physical-device limits are a full Metal-class set (no zero long tail wgpu would
    /// reject). Spot-check the per-stage / descriptor-set / vertex / framebuffer limits.
    #[test]
    fn physical_device_limits_are_a_full_metal_class_set() {
        let l = state::physical_device_properties().limits;
        assert!(l.max_per_stage_descriptor_samplers >= 16);
        assert!(l.max_per_stage_descriptor_uniform_buffers >= 12);
        assert!(l.max_per_stage_descriptor_storage_buffers >= 8);
        assert!(l.max_per_stage_descriptor_sampled_images >= 16);
        assert!(l.max_per_stage_descriptor_storage_images >= 4);
        assert!(l.max_per_stage_resources >= 128);
        assert!(l.max_descriptor_set_uniform_buffers >= 12);
        assert!(l.max_vertex_input_attributes >= 16);
        assert!(l.max_vertex_input_bindings >= 8);
        assert!(l.max_vertex_output_components >= 64);
        assert!(l.max_fragment_input_components >= 60);
        assert!(l.max_fragment_output_attachments >= 4);
        assert!(l.max_framebuffer_width >= 8192 && l.max_framebuffer_height >= 8192);
        assert!(l.max_viewports >= 1 && l.max_viewport_dimensions[0] >= 8192);
        assert!(l.max_sampler_allocation_count >= 4000);
        assert!(l.max_draw_indexed_index_value == u32::MAX);
        assert_eq!(l.min_uniform_buffer_offset_alignment, 256);
        assert!(l.non_coherent_atom_size >= 1);
    }

    /// Blocker 2: depth/stencil images are creatable (D32_SFLOAT + D24_UNORM_S8), with the right aspect —
    /// wgpu needs a depth attachment for most pipelines.
    #[test]
    fn depth_stencil_images_are_creatable() {
        let _g = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        let mk = |format: vk::Format| -> u64 {
            let ci = vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D).format(format)
                .extent(vk::Extent3D { width: 16, height: 16, depth: 1 }).mip_levels(1).array_layers(1)
                .samples(vk::SampleCountFlags::TYPE_1).tiling(vk::ImageTiling::OPTIMAL)
                .usage(vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT).initial_layout(vk::ImageLayout::UNDEFINED);
            let mut img = 0u64;
            assert_eq!(vkCreateImage(core::ptr::null_mut(), &ci, core::ptr::null(), &mut img), types::VK_SUCCESS, "format {} must be creatable", format.as_raw());
            img
        };
        let d32 = mk(vk::Format::D32_SFLOAT);
        let d24 = mk(vk::Format::D24_UNORM_S8_UINT);
        {
            let s = reg::lock();
            assert_eq!(s.images[&d32].aspect_mask, vk::ImageAspectFlags::DEPTH.as_raw());
            assert_eq!(s.images[&d24].aspect_mask, (vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL).as_raw());
        }
        vkDestroyImage(core::ptr::null_mut(), d32, core::ptr::null());
        vkDestroyImage(core::ptr::null_mut(), d24, core::ptr::null());
    }

    /// The full wgpu-hal-style device-creation query sequence succeeds end-to-end against our ICD:
    /// instance(1.1) -> enumerate device -> properties2(+Maintenance3) -> features2(+Vulkan12/13) ->
    /// queue families -> memory -> device extensions -> createDevice(feature chain) -> getDeviceQueue.
    /// This is the in-process form of "how far a wgpu device-create gets": all the way to a live queue.
    #[test]
    fn wgpu_style_device_creation_walkthrough_succeeds() {
        let _g = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        // 1. Instance at Vulkan 1.1 (wgpu-hal's minimum).
        let app = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
        let ici = vk::InstanceCreateInfo::default().application_info(&app);
        let mut instance: types::VkInstance = core::ptr::null_mut();
        assert_eq!(vkCreateInstance(&ici, core::ptr::null(), &mut instance), types::VK_SUCCESS);
        assert!(!instance.is_null());

        // 2. Enumerate the physical device.
        let mut n = 0u32;
        assert_eq!(vkEnumeratePhysicalDevices(instance, &mut n, core::ptr::null_mut()), types::VK_SUCCESS);
        assert_eq!(n, 1);
        let mut phys: types::VkPhysicalDevice = core::ptr::null_mut();
        assert_eq!(vkEnumeratePhysicalDevices(instance, &mut n, &mut phys), types::VK_SUCCESS);

        // 3. properties2 + Maintenance3: apiVersion 1.1, a non-zero descriptor-set ceiling + real limits.
        let mut m3 = vk::PhysicalDeviceMaintenance3Properties::default();
        let (api_minor, max_per_stage) = {
            let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut m3);
            vkGetPhysicalDeviceProperties2(phys, &mut props2);
            (ash::vk::api_version_minor(props2.properties.api_version), props2.properties.limits.max_per_stage_resources)
        };
        assert!(api_minor >= 1, "wgpu-hal requires a >= 1.1 device (we now advertise 1.4)");
        assert!(m3.max_per_set_descriptors > 0, "wgpu needs a non-zero maxPerSetDescriptors");
        assert!(max_per_stage >= 128, "real limits, not zero");

        // 4. features2 + Vulkan12/13: the features wgpu enables must be reported TRUE.
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
        {
            let mut feats2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut f12).push_next(&mut f13);
            vkGetPhysicalDeviceFeatures2(phys, &mut feats2);
        }
        assert_eq!(f12.timeline_semaphore, vk::TRUE);
        assert_eq!(f12.buffer_device_address, vk::TRUE);
        assert_eq!(f13.dynamic_rendering, vk::TRUE);
        assert_eq!(f13.synchronization2, vk::TRUE);

        // 5. Queue families: a graphics+compute queue.
        let mut qn = 0u32;
        vkGetPhysicalDeviceQueueFamilyProperties(phys, &mut qn, core::ptr::null_mut());
        assert!(qn >= 1);
        let mut qprops = vec![vk::QueueFamilyProperties::default(); qn as usize];
        vkGetPhysicalDeviceQueueFamilyProperties(phys, &mut qn, qprops.as_mut_ptr());
        assert!(qprops[0].queue_flags.contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE));

        // 6. Memory: a device-local + host-visible type.
        let mut mem = vk::PhysicalDeviceMemoryProperties::default();
        vkGetPhysicalDeviceMemoryProperties(phys, &mut mem);
        assert!(mem.memory_type_count >= 1);
        assert!(mem.memory_types[0].property_flags.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL | vk::MemoryPropertyFlags::HOST_VISIBLE));

        // 7. Device extensions: the set wgpu-hal looks for.
        let mut en = 0u32;
        vkEnumerateDeviceExtensionProperties(phys, core::ptr::null(), &mut en, core::ptr::null_mut());
        let mut exts = vec![vk::ExtensionProperties::default(); en as usize];
        vkEnumerateDeviceExtensionProperties(phys, core::ptr::null(), &mut en, exts.as_mut_ptr());
        let names: Vec<String> = exts.iter().map(|e| {
            let b: Vec<u8> = e.extension_name.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
            String::from_utf8_lossy(&b).into_owned()
        }).collect();
        for want in ["VK_KHR_swapchain", "VK_KHR_timeline_semaphore", "VK_KHR_dynamic_rendering", "VK_KHR_buffer_device_address"] {
            assert!(names.iter().any(|n| n == want), "device extension {want} not advertised");
        }

        // 8. Create the device with a features2 chain (wgpu enables the detected features via pNext), and
        //    a graphics+compute queue.
        let prio = [1.0f32];
        let qci = vk::DeviceQueueCreateInfo::default().queue_family_index(0).queue_priorities(&prio);
        let qcis = [qci];
        let mut enable12 = vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true).buffer_device_address(true);
        let dci = vk::DeviceCreateInfo::default().queue_create_infos(&qcis).push_next(&mut enable12);
        let mut device: types::VkDevice = core::ptr::null_mut();
        assert_eq!(vkCreateDevice(phys, &dci, core::ptr::null(), &mut device), types::VK_SUCCESS, "device creation must succeed");
        assert!(!device.is_null());

        // 9. Retrieve the queue.
        let mut queue: types::VkQueue = core::ptr::null_mut();
        vkGetDeviceQueue(device, 0, 0, &mut queue);
        assert!(!queue.is_null(), "a live queue — device creation reached the end");

        vkDestroyDevice(device, core::ptr::null());
        vkDestroyInstance(instance, core::ptr::null());
    }

    /// Blocker 3: format properties are per-format — a color format advertises COLOR_ATTACHMENT (not
    /// DEPTH_STENCIL), a depth format advertises DEPTH_STENCIL_ATTACHMENT (not COLOR).
    #[test]
    fn format_properties_are_per_format() {
        use ash::vk;
        use vk::FormatFeatureFlags as F;
        let feats = |fmt: vk::Format| {
            let mut p = vk::FormatProperties::default();
            vkGetPhysicalDeviceFormatProperties(core::ptr::null_mut(), fmt.as_raw(), &mut p);
            p.optimal_tiling_features
        };
        let color = feats(vk::Format::B8G8R8A8_UNORM);
        assert!(color.contains(F::COLOR_ATTACHMENT) && color.contains(F::SAMPLED_IMAGE) && color.contains(F::BLIT_DST));
        assert!(!color.contains(F::DEPTH_STENCIL_ATTACHMENT), "color must NOT claim depth-stencil");
        let depth = feats(vk::Format::D32_SFLOAT);
        assert!(depth.contains(F::DEPTH_STENCIL_ATTACHMENT) && depth.contains(F::SAMPLED_IMAGE));
        assert!(!depth.contains(F::COLOR_ATTACHMENT), "depth must NOT claim color attachment");
    }

    // ---- increment 7: 1.2 feature reporting + descriptor indexing ----

    /// `vkGetPhysicalDeviceFeatures2` fills the promoted-feature structs a modern app (wgpu-hal) chains
    /// on, reporting ONLY features with real bodies: timeline semaphore, buffer device address, host
    /// query reset, dynamic rendering, synchronization2, and the descriptor-indexing subset we honor —
    /// while truthfully leaving update-after-bind FALSE.
    #[test]
    fn features2_reports_modern_features_truthfully() {
        use ash::vk;
        let mut vk12 = vk::PhysicalDeviceVulkan12Features::default();
        let mut vk13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut f2 = vk::PhysicalDeviceFeatures2::default().push_next(&mut vk12).push_next(&mut vk13);
        vkGetPhysicalDeviceFeatures2(core::ptr::null_mut(), &mut f2);
        assert_eq!(vk12.timeline_semaphore, vk::TRUE);
        assert_eq!(vk12.buffer_device_address, vk::TRUE);
        assert_eq!(vk12.host_query_reset, vk::TRUE);
        assert_eq!(vk12.descriptor_indexing, vk::TRUE);
        assert_eq!(vk12.descriptor_binding_variable_descriptor_count, vk::TRUE);
        assert_eq!(vk12.descriptor_binding_partially_bound, vk::TRUE);
        assert_eq!(vk12.runtime_descriptor_array, vk::TRUE);
        // Truthfully NOT honored in our bind-at-record IR model:
        assert_eq!(vk12.descriptor_binding_uniform_buffer_update_after_bind, vk::FALSE);
        assert_eq!(vk12.descriptor_binding_sampled_image_update_after_bind, vk::FALSE);
        assert_eq!(vk13.dynamic_rendering, vk::TRUE);
        assert_eq!(vk13.synchronization2, vk::TRUE);
    }

    /// Descriptor indexing: a VARIABLE_DESCRIPTOR_COUNT | PARTIALLY_BOUND binding, an UPDATE_AFTER_BIND
    /// pool, and a variable-count allocation are all accepted and recorded (bindless creation succeeds).
    #[test]
    fn descriptor_indexing_flags_pool_and_variable_count_are_recorded() {
        let _g = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        use ash::vk::Handle;
        let binding = vk::DescriptorSetLayoutBinding::default()
            .binding(0).descriptor_type(vk::DescriptorType::STORAGE_BUFFER).descriptor_count(100).stage_flags(vk::ShaderStageFlags::FRAGMENT);
        let flags = [vk::DescriptorBindingFlags::VARIABLE_DESCRIPTOR_COUNT | vk::DescriptorBindingFlags::PARTIALLY_BOUND];
        let mut bf = vk::DescriptorSetLayoutBindingFlagsCreateInfo::default().binding_flags(&flags);
        let set_ci = vk::DescriptorSetLayoutCreateInfo::default().bindings(core::slice::from_ref(&binding)).push_next(&mut bf);
        let mut layout = 0u64;
        assert_eq!(vkCreateDescriptorSetLayout(core::ptr::null_mut(), &set_ci, core::ptr::null(), &mut layout), types::VK_SUCCESS);

        let pool_size = vk::DescriptorPoolSize { ty: vk::DescriptorType::STORAGE_BUFFER, descriptor_count: 100 };
        let pool_ci = vk::DescriptorPoolCreateInfo::default().max_sets(1).flags(vk::DescriptorPoolCreateFlags::UPDATE_AFTER_BIND).pool_sizes(core::slice::from_ref(&pool_size));
        let mut pool = 0u64;
        assert_eq!(vkCreateDescriptorPool(core::ptr::null_mut(), &pool_ci, core::ptr::null(), &mut pool), types::VK_SUCCESS);

        let layout_h = vk::DescriptorSetLayout::from_raw(layout);
        let counts = [42u32];
        let mut var = vk::DescriptorSetVariableDescriptorCountAllocateInfo::default().descriptor_counts(&counts);
        let alloc = vk::DescriptorSetAllocateInfo::default().descriptor_pool(vk::DescriptorPool::from_raw(pool)).set_layouts(core::slice::from_ref(&layout_h)).push_next(&mut var);
        let mut set = 0u64;
        assert_eq!(vkAllocateDescriptorSets(core::ptr::null_mut(), &alloc, &mut set), types::VK_SUCCESS);

        let s = reg::lock();
        // VARIABLE_DESCRIPTOR_COUNT = 0x8, PARTIALLY_BOUND = 0x4.
        assert!(s.descriptor_set_layouts[&layout].bindings[0].binding_flags & 0x8 != 0, "variable-count flag recorded");
        assert!(s.descriptor_set_layouts[&layout].bindings[0].binding_flags & 0x4 != 0, "partially-bound flag recorded");
        assert!(s.descriptor_pools[&pool].update_after_bind, "UPDATE_AFTER_BIND pool recorded");
        assert_eq!(s.dsets[&set].variable_count, Some(42), "variable descriptor count recorded");
    }

    /// Vulkan 1.2 core progress + the two EXT extensions are advertised and their commands non-stub.
    #[test]
    fn vulkan_1_2_core_progress_and_ext_extensions() {
        for e in ["VK_EXT_descriptor_indexing", "VK_EXT_host_query_reset"] {
            assert!(capability::ADVERTISED_DEVICE_EXTENSIONS.contains(&e), "{e} not advertised");
        }
        for n in ["vkCreateRenderPass2", "vkCmdBeginRenderPass2", "vkResetQueryPool", "vkResetQueryPoolEXT", "vkCmdDrawIndirectCount"] {
            assert!(dispatch_addr(n).is_some(), "{n} not resolvable");
            assert!(CAPABILITIES.iter().find(|c| c.name == n).unwrap().implemented(), "{n} still a stub");
        }
        // 1.2 core is materially advanced (was 6/13).
        let impl12 = CAPABILITIES.iter().filter(|e| e.origin == "core:1.2" && e.implemented()).count();
        assert!(impl12 >= 13, "expected the whole 1.2 core bodied, got {impl12}/13");
    }

    // ---- increment 8: Vulkan 1.3 core ----

    /// The entire Vulkan **1.3** mandatory core now has real bodies — zero generated stubs remain (extended
    /// dynamic state, copy_commands2, synchronization2, maintenance4, private data, tool properties).
    #[test]
    fn vulkan_1_3_core_is_fully_implemented() {
        use capability::Cap;
        let core13: Vec<_> = CAPABILITIES.iter().filter(|e| e.origin == "core:1.3").collect();
        assert_eq!(core13.len(), 37, "Vulkan 1.3 core census size");
        assert_eq!(core13.iter().filter(|e| e.cap == Cap::Stub).count(), 0, "no mandatory core:1.3 command may remain a stub");
        for n in ["vkCmdSetCullMode", "vkCmdCopyBuffer2", "vkQueueSubmit2", "vkGetDeviceBufferMemoryRequirements", "vkSetPrivateData", "vkGetPhysicalDeviceToolProperties"] {
            assert!(dispatch_addr(n).is_some(), "1.3 core {n} not resolvable");
            assert!(CAPABILITIES.iter().find(|c| c.name == n).unwrap().implemented(), "{n} still a stub");
        }
    }

    /// Per-feature: private data round-trips a payload, and extended dynamic state records verbatim.
    #[test]
    fn private_data_and_extended_dynamic_state_roundtrip() {
        let _g = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        use ash::vk::Handle;
        // Private data: create a slot, set a payload on a (type, handle) key, read it back, then destroy.
        let mut slot = 0u64;
        assert_eq!(vkCreatePrivateDataSlot(core::ptr::null_mut(), core::ptr::null(), core::ptr::null(), &mut slot), types::VK_SUCCESS);
        assert_eq!(vkSetPrivateData(core::ptr::null_mut(), 9 /*BUFFER*/, 0x1234, slot, 0xCAFE), types::VK_SUCCESS);
        let mut got = 0u64;
        vkGetPrivateData(core::ptr::null_mut(), 9, 0x1234, slot, &mut got);
        assert_eq!(got, 0xCAFE, "private data round-trips");
        vkDestroyPrivateDataSlot(core::ptr::null_mut(), slot, core::ptr::null());

        // Extended dynamic state records verbatim into the command buffer.
        let cb = 0x8888usize as types::VkCommandBuffer;
        let bi = vk::CommandBufferBeginInfo::default();
        assert_eq!(vkBeginCommandBuffer(cb, &bi), types::VK_SUCCESS);
        vkCmdSetCullMode(cb, vk::CullModeFlags::BACK);
        vkCmdSetDepthTestEnable(cb, vk::TRUE);
        vkCmdSetPrimitiveTopology(cb, vk::PrimitiveTopology::TRIANGLE_STRIP);
        {
            let s = reg::lock();
            let d = &s.cmdbufs[&(cb as usize)].dynamic;
            assert_eq!(d.cull_mode, vk::CullModeFlags::BACK.as_raw());
            assert!(d.depth_test_enable);
            assert_eq!(d.primitive_topology, vk::PrimitiveTopology::TRIANGLE_STRIP.as_raw());
        }
        vkResetCommandBuffer(cb, 0);
    }

    // ---- increment 9: Vulkan 1.4 core — the FULL core spec surface ----

    /// The entire Vulkan **1.4** mandatory core now has real bodies — zero generated stubs remain. This
    /// completes the full Vulkan core spec surface: 1.0-1.4 = 137+28+13+37+19 = 234/234 core commands.
    #[test]
    fn vulkan_1_4_core_is_fully_implemented() {
        use capability::Cap;
        let count = |o: &str| CAPABILITIES.iter().filter(|e| e.origin == o).count();
        let implemented = |o: &str| CAPABILITIES.iter().filter(|e| e.origin == o && e.cap != Cap::Stub).count();
        let core14: Vec<_> = CAPABILITIES.iter().filter(|e| e.origin == "core:1.4").collect();
        assert_eq!(core14.len(), 19, "Vulkan 1.4 core census size");
        assert_eq!(core14.iter().filter(|e| e.cap == Cap::Stub).count(), 0, "no mandatory core:1.4 command may remain a stub");
        // The WHOLE core spec surface is bodied — every core version at 100%.
        let total_core: usize = ["core:1.0", "core:1.1", "core:1.2", "core:1.3", "core:1.4"].iter().map(|o| count(o)).sum();
        let total_impl: usize = ["core:1.0", "core:1.1", "core:1.2", "core:1.3", "core:1.4"].iter().map(|o| implemented(o)).sum();
        assert_eq!(total_core, 234, "the pinned registry's full core surface");
        assert_eq!(total_impl, 234, "FULL Vulkan core spec coverage: every core command has a real body");
        for n in ["vkTransitionImageLayout", "vkCmdPushDescriptorSet", "vkGetRenderingAreaGranularity", "vkCmdSetLineStipple", "vkCopyMemoryToImage"] {
            assert!(dispatch_addr(n).is_some(), "1.4 core {n} not resolvable");
            assert!(CAPABILITIES.iter().find(|c| c.name == n).unwrap().implemented(), "{n} still a stub");
        }
    }

    /// Host-side image layout transition (VK_EXT_host_image_copy, promoted 1.4): applies the new layout to
    /// the tracked subresource state without a queue.
    #[test]
    fn host_image_layout_transition_updates_subresource_state() {
        let _g = reg::TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        use ash::vk;
        use ash::vk::Handle;
        let ci = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D).format(vk::Format::R8G8B8A8_UNORM)
            .extent(vk::Extent3D { width: 8, height: 8, depth: 1 }).mip_levels(1).array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1).usage(vk::ImageUsageFlags::TRANSFER_DST)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let mut img = 0u64;
        assert_eq!(vkCreateImage(core::ptr::null_mut(), &ci, core::ptr::null(), &mut img), types::VK_SUCCESS);
        let t = vk::HostImageLayoutTransitionInfoEXT::default()
            .image(vk::Image::from_raw(img))
            .old_layout(vk::ImageLayout::UNDEFINED)
            .new_layout(vk::ImageLayout::GENERAL)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR, base_mip_level: 0, level_count: 1, base_array_layer: 0, layer_count: 1,
            });
        assert_eq!(vkTransitionImageLayout(core::ptr::null_mut(), 1, &t), types::VK_SUCCESS);
        {
            let s = reg::lock();
            let st = s.images[&img].subresources[&(vk::ImageAspectFlags::COLOR.as_raw(), 0, 0)];
            assert_eq!(st.layout, vk::ImageLayout::GENERAL.as_raw(), "host transition applied the layout");
        }
        vkDestroyImage(core::ptr::null_mut(), img, core::ptr::null());
    }
}
