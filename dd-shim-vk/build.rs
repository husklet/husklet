//! dd-shim-vk codegen: turn the Khronos-`vk.xml`-derived entry-point manifest into (a) the complete
//! set of `#[no_mangle] extern "C"` `vk*` exports the ICD must provide, and (b) a name→address
//! dispatch table the loader-facing `vk_icdGetInstanceProcAddr`/`vkGetInstanceProcAddr` resolve
//! against. Mirrors dd-shim-cuda/build.rs and dd-shim-gl/build.rs.
//!
//! Input:  `registry/vk_commands.manifest` (generated from vk.xml — see `registry/`).
//! Output: `$OUT_DIR/generated_entrypoints.rs`, `include!`d by `src/lib.rs`.
//!
//! For every command NOT hand-implemented in `src/` (`IMPLEMENTED` below), we emit a spec-faithful
//! *default* stub: correct C-ABI signature (so the loader/app resolves the symbol), a DD_SHIM_DEBUG
//! "unimplemented entry point" trace, and a benign default return (`VK_SUCCESS` = 0 for the `VkResult`
//! most Vulkan entry points return). Real bodies replace stubs incrementally without ever changing the
//! exported surface — the shrinking long tail, exactly like the GL/CUDA siblings.
//!
//! Manifest records: `T<TAB>type<TAB>kind` classify by-value types (so a bare param maps without
//! knowing every Vulkan type); `C<TAB>name<TAB>ret<TAB>params` are the commands. A pointer is a
//! pointer — every `*`-suffixed C type lowers to a `c_void` pointer regardless of pointee, so only
//! by-value types ever consult the T-table.
//!
//! Also sets the shared-object soname to `libvk_dd.so.1` (the ICD library the icd.json `library_path`
//! names).

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

// VkResult error values (stable Vulkan ABI, from vk.xml) a truthful stub returns instead of a false
// `VK_SUCCESS`. Kept in lock-step with `src/types.rs`; the crate's inventory test cross-checks them.
const VK_ERROR_EXTENSION_NOT_PRESENT: i32 = -7;
const VK_ERROR_FEATURE_NOT_PRESENT: i32 = -8;

/// The union of instance + device extensions the ICD advertises (mirrors `crate::capability`'s
/// `ADVERTISED_*_EXTENSIONS`). A `VkResult` stub whose command comes from an extension NOT in this set
/// returns `VK_ERROR_EXTENSION_NOT_PRESENT` (the extension is genuinely absent); a core command, or a
/// still-unimplemented command from an advertised extension, returns `VK_ERROR_FEATURE_NOT_PRESENT`.
fn advertised_extensions() -> HashSet<&'static str> {
    [
        "VK_KHR_surface",
        "VK_KHR_wayland_surface",
        "VK_KHR_get_physical_device_properties2",
        "VK_KHR_swapchain",
    ]
    .into_iter()
    .collect()
}

/// The `VkResult` error a `VkResult`-returning stub returns, from its origin + the advertised set.
fn stub_vk_error(origin: &str, advertised: &HashSet<&str>) -> i32 {
    match origin.strip_prefix("ext:") {
        Some(ext) if advertised.contains(ext) => VK_ERROR_FEATURE_NOT_PRESENT,
        Some(_) => VK_ERROR_EXTENSION_NOT_PRESENT, // unadvertised (or "(unlisted)") extension
        None => VK_ERROR_FEATURE_NOT_PRESENT,      // core:X.Y
    }
}

/// Load the `vk.xml`-derived origin sidecar (`registry/vk_command_origins.manifest`): command name →
/// `"core:1.0"`..`"core:1.3"` / `"ext:VK_..."`. A command absent from the sidecar (a platform/vulkansc
/// command with no plain-`vulkan` origin) defaults to `"ext:(unlisted)"` — truthfully "not present".
fn load_origins(dir: &str) -> HashMap<String, String> {
    let path = PathBuf::from(dir).join("registry/vk_command_origins.manifest");
    println!("cargo:rerun-if-changed={}", path.display());
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut m = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end();
        if !line.starts_with("O\t") {
            continue;
        }
        let mut f = line.split('\t');
        f.next();
        let name = f.next().unwrap_or("");
        let origin = f.next().unwrap_or("");
        if !name.is_empty() {
            m.insert(name.to_string(), origin.to_string());
        }
    }
    m
}

/// Bring-up bodies whose behaviour is bounded (Phase-0 `partial`, per audit §2.2) → the supported-domain
/// note. Any `IMPLEMENTED` command NOT listed here is classified `full`. Everything not `IMPLEMENTED`
/// is a `stub`.
fn partial_note(name: &str) -> Option<&'static str> {
    Some(match name {
        // physical-device queries: a valid answer over a bounded value domain
        "vkGetPhysicalDeviceProperties" | "vkGetPhysicalDeviceProperties2" => {
            "limits/props are a plausible Metal-class subset; the long tail defaults to zero"
        }
        "vkGetPhysicalDeviceFeatures" | "vkGetPhysicalDeviceFeatures2" => {
            "reports a conservative feature subset, not the full device feature set"
        }
        "vkGetPhysicalDeviceFormatProperties" | "vkGetPhysicalDeviceFormatProperties2" => {
            "broad fixed feature flags, not per-format-measured capabilities"
        }
        // memory: the single unified HOST_VISIBLE|COHERENT staging model
        "vkAllocateMemory" | "vkMapMemory" => {
            "single unified HOST_VISIBLE|COHERENT memory type modeled as a staging Vec<u8>"
        }
        "vkCreateImage" => "COLOR_ATTACHMENT render-target subset; general images/tiling not modeled",
        "vkCmdPipelineBarrier" => {
            "legacy color-image subresource layouts on one queue; no sync2 or backend hazard lowering"
        }
        "vkCmdCopyBufferToImage" | "vkCmdCopyImageToBuffer" => {
            "2D color/base-layer regions with exact buffer offsets and row pitch; no 3D or multilayer"
        }
        "vkCmdCopyImage" | "vkCmdBlitImage" | "vkCmdResolveImage" => {
            "2D uncompressed color regions; blits reject reversed coordinates and aliasing"
        }
        "vkCmdClearColorImage" => "base color subresource only; no mip/layer range clear lowering",
        "vkCreateShaderModule" => {
            "validated SPIR-V 1.0-1.6 subset: entry/interface/spec-constant reflection plus descriptor-type \
             inference (sampler/image/sampled-image/(texel|storage) buffer/input-attachment, incl. arrays)"
        }
        "vkCreateComputePipelines" | "vkCreateGraphicsPipelines" => {
            "validated entry/spec/interface/layout subset; a set-layout descriptor-type mismatch and \
             unsupported SPIR-V vocabulary are rejected before IR mutation"
        }
        // submit + synchronization: the bring-up dependency model
        "vkQueueSubmit" => "records + ships IR; ignores wait/signal semaphores beyond the bring-up model",
        "vkCreateSemaphore" | "vkDestroySemaphore" => {
            "binary semaphore handle only; no timeline or wait/signal state machine"
        }
        "vkWaitForFences" | "vkGetFenceStatus" | "vkResetFences" => {
            "fences modeled as already-signaled (synchronous host replay); no real timeout/unsignaled state"
        }
        // WSI: the fixed FIFO / round-robin bring-up swapchain
        "vkCreateSwapchainKHR" => "validated FIFO / one-format / identity swapchain with retirement",
        "vkAcquireNextImageKHR" => {
            "owned-image acquire with timeout/retirement; synchronous completion model"
        }
        "vkQueuePresentKHR" => "transactional synchronous present with pResults; no resize/SUBOPTIMAL",
        "vkGetPhysicalDeviceSurfaceCapabilitiesKHR"
        | "vkGetPhysicalDeviceSurfaceFormatsKHR"
        | "vkGetPhysicalDeviceSurfacePresentModesKHR" => {
            "validated fixed WSI capabilities (one format, FIFO, identity transform, opaque alpha)"
        }
        // queries: real availability + read/copy machinery, but a synchronous host replay has no GPU
        // sample counts, so occlusion/statistics results are a conservative 0 and timestamps a serial.
        "vkCmdBeginQuery" | "vkCmdEndQuery" | "vkCmdWriteTimestamp" | "vkCmdCopyQueryPoolResults"
        | "vkGetQueryPoolResults" => {
            "real availability + read/copy; occlusion/statistics results are conservative 0, timestamps a host serial"
        }
        // events: device set/reset apply at (synchronous) submit completion; no intra-submit ordering.
        "vkCmdSetEvent" | "vkCmdResetEvent" => {
            "device set/reset applied at synchronous submit completion; no intra-submit event ordering"
        }
        "vkCmdWaitEvents" => {
            "image barriers join the shared submit-time transition validation; the event wait is trivially satisfied (single-queue synchronous)"
        }
        // dynamic pipeline state: recorded verbatim (observable) but not yet lowered into IR draw state.
        "vkCmdSetLineWidth" | "vkCmdSetDepthBias" | "vkCmdSetDepthBounds" | "vkCmdSetBlendConstants"
        | "vkCmdSetStencilCompareMask" | "vkCmdSetStencilWriteMask" | "vkCmdSetStencilReference" => {
            "recorded verbatim (observable) but not yet lowered into the IR draw state"
        }
        "vkCmdPushConstants" => {
            "validated against the layout ranges and retained; the IR does not yet carry a push-constant block"
        }
        // inline buffer ops: uploaded at the start of the owning submit (not interleaved with draws).
        "vkCmdUpdateBuffer" | "vkCmdFillBuffer" => {
            "recorded as a start-of-submit IR WriteBuffer (not interleaved with intervening draws)"
        }
        "vkCmdClearAttachments" => "color attachment clears lower to ClearRect; depth/stencil clears are not materialized",
        "vkCmdClearDepthStencilImage" => "validates the depth/stencil target; depth is not materialized by the software oracle",
        "vkCmdNextSubpass" => "single-subpass render-pass model; subpass advance is a validated no-op",
        "vkCmdDrawIndirect" | "vkCmdDrawIndexedIndirect" | "vkCmdDispatchIndirect" => {
            "indirect parameter buffers are validated; the IR has no indirect encoder op yet"
        }
        "vkCreateBufferView" | "vkDestroyBufferView" => {
            "validated typed buffer window; retained for the texel-buffer descriptor IR increment"
        }
        "vkGetPhysicalDeviceImageFormatProperties" => {
            "reports the supported 2D color subset with device limits; other combinations are FORMAT_NOT_SUPPORTED"
        }
        "vkQueueBindSparse" => {
            "binary-semaphore + fence synchronization only; no sparse residency (no sparse resources exposed)"
        }
        _ => return None,
    })
}

fn main() {
    let dir = env("CARGO_MANIFEST_DIR");
    let manifest = PathBuf::from(&dir).join("registry/vk_commands.manifest");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed=build.rs");
    // The soname is the deployed Linux guest ICD name. `-Wl,-soname` is a GNU-ld flag; macOS `ld`
    // rejects it (it uses `-install_name`). The ICD only ships on the guest (Linux), and the macOS
    // build exists solely for the on-Metal validation tests (which link the rlib), so only emit the
    // soname link-arg on Linux — otherwise the cdylib fails to link on the test host.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-cdylib-link-arg=-Wl,-soname,libvk_dd.so.1");
    }

    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));

    // Pass 1: the by-value type classification table.
    let mut kinds: HashMap<String, String> = HashMap::new();
    for line in text.lines() {
        let line = line.trim_end();
        if !line.starts_with("T\t") {
            continue;
        }
        let mut f = line.split('\t');
        f.next();
        let name = f.next().unwrap_or("");
        let kind = f.next().unwrap_or("");
        kinds.insert(name.to_string(), kind.to_string());
    }

    // Pass 2: emit the C-ABI stub for every non-IMPLEMENTED command, and a dispatch entry for all.
    let mut out = String::new();
    out.push_str("// @generated by build.rs from registry/vk_commands.manifest — DO NOT EDIT.\n");
    out.push_str("// (crate-level `#![allow(non_snake_case, ...)]` lives in src/lib.rs.)\n\n");

    let origins = load_origins(&dir);
    let advertised = advertised_extensions();
    // One inventory row per exported command: (name, cap-level, vk_error, origin, note).
    let mut inventory: Vec<(String, &'static str, i32, String, &'static str)> = Vec::new();

    let mut cmds = 0usize;
    let mut stubs = 0usize;
    let mut dispatch = String::new();
    let mut names = String::new();
    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() || line.starts_with('#') || line.starts_with("T\t") {
            continue;
        }
        let mut f = line.split('\t');
        let rec = f.next().unwrap_or("");
        if rec != "C" {
            panic!("manifest line {}: unknown record {rec:?}", lineno + 1);
        }
        let name = f.next().unwrap_or("");
        let ret = f.next().unwrap_or("void");
        let params = f.next().unwrap_or("");
        if name.is_empty() {
            panic!("manifest line {}: empty name: {line:?}", lineno + 1);
        }
        cmds += 1;
        let origin = origins.get(name).cloned().unwrap_or_else(|| "ext:(unlisted)".to_string());
        // Every exported entry point — hand-written or generated — is resolvable at crate root by its
        // bare name (implemented ones via `pub use` in lib.rs), so the dispatch resolver references
        // them uniformly. The fn-item→address cast (`as *const () as usize`) must run at RUNTIME (a
        // `static` of it fails const-eval: pointers have no integer value at compile time), so we emit
        // a match function rather than a table.
        writeln!(dispatch, "        \"{name}\" => Some({name} as *const () as usize),").unwrap();
        writeln!(names, "    \"{name}\",").unwrap();
        if IMPLEMENTED.contains(&name) {
            // hand-written in src/; a generated stub would collide. Full unless the bring-up body is
            // bounded (audit §2.2), in which case it is `partial` with a supported-domain note.
            let (level, note) = match partial_note(name) {
                Some(n) => ("Partial", n),
                None => ("Full", ""),
            };
            inventory.push((name.to_string(), level, 0, origin, note));
            continue;
        }
        stubs += 1;
        // Truthful failure: a `VkResult` stub returns the API-defined error (never `VK_SUCCESS`); every
        // other return is the truthful zero/`VK_FALSE`/NULL. The error is recorded in the inventory.
        let vk_error = if ret.trim() == "VkResult" {
            stub_vk_error(&origin, &advertised)
        } else {
            0
        };
        emit_stub(&mut out, name, ret, params, &kinds, vk_error);
        inventory.push((name.to_string(), "Stub", vk_error, origin, ""));
    }

    writeln!(out, "\n/// Total Vulkan entry points in the manifest (the full vk.xml surface).").unwrap();
    writeln!(out, "pub const VK_ENTRYPOINTS: usize = {cmds};").unwrap();
    writeln!(out, "/// Entry points emitted as default stubs (not yet hand-implemented).").unwrap();
    writeln!(out, "pub const GENERATED_STUBS: usize = {stubs};").unwrap();

    emit_capabilities(&mut out, &inventory);

    // The name→address resolver `vk_icdGetInstanceProcAddr` / `vkGetInstanceProcAddr` scan. A match
    // over the whole exported `vk*` surface; the address cast runs at call time (see above).
    writeln!(out, "\n/// Resolve any exported `vk*` entry point by name to its address. Consulted by the").unwrap();
    writeln!(out, "/// loader-facing proc-addr resolvers in `crate::icd`. Runtime cast (not a const table).").unwrap();
    writeln!(out, "pub fn dispatch_addr(name: &str) -> Option<usize> {{\n    match name {{\n{dispatch}        _ => None,\n    }}\n}}").unwrap();
    // A name-only census the surface tests can iterate without taking any addresses.
    writeln!(out, "\n/// Every exported `vk*` entry-point name (the completeness census list).").unwrap();
    writeln!(out, "pub static DISPATCH_NAMES: &[&str] = &[\n{names}];", names = names).unwrap();

    let out_path = PathBuf::from(env("OUT_DIR")).join("generated_entrypoints.rs");
    std::fs::write(&out_path, out).unwrap();
}

fn emit_stub(out: &mut String, name: &str, ret: &str, params: &str, kinds: &HashMap<String, String>, vk_error: i32) {
    let mut sig = String::new();
    let mut argnames: Vec<String> = Vec::new();
    let mut last_out: Option<String> = None; // last param IF it is a single `*mut` output pointer
    if !params.is_empty() {
        let n = params.split(';').count();
        for (i, p) in params.split(';').enumerate() {
            let (ty, pname) = p.split_once('|').unwrap_or_else(|| panic!("bad param {p:?} in {name}"));
            let rty = map_type(ty.trim(), name, kinds);
            let mut pn = sanitize(pname.trim(), i);
            // Guarantee unique parameter identifiers (Rust rejects duplicates); a manifest can carry
            // repeated names across api-variant params. Names are cosmetic — the ABI is unaffected.
            while argnames.contains(&pn) {
                pn.push_str(&format!("_{i}"));
            }
            if i > 0 {
                sig.push_str(", ");
            }
            write!(sig, "{pn}: {rty}").unwrap();
            // A `vkCreate*`/`vkAllocate*` command's final parameter is its output handle (or handle
            // array) — a single, non-const `*mut` pointer to ≥8-byte handle storage. Record it so the
            // stub can null it (VK_NULL_HANDLE) before returning the error: truthful failure must
            // initialize the output, never leave a caller reading an uninitialized handle.
            if i + 1 == n && rty.starts_with("*mut ") && !rty.starts_with("*mut *mut") {
                last_out = Some(pn.clone());
            }
            argnames.push(pn);
        }
    }
    let rmap = map_ret(ret.trim(), name, kinds);
    let arrow = rmap.as_ref().map(|r| format!(" -> {r}")).unwrap_or_default();
    let touch: String = argnames.iter().map(|a| format!("let _ = {a}; ")).collect();
    // For a create/allocate VkResult stub, null the output handle before failing.
    let init_out = match (&rmap, &last_out) {
        (Some(_), Some(outp)) if ret.trim() == "VkResult" && (name.starts_with("vkCreate") || name.starts_with("vkAllocate")) => {
            format!("unsafe {{ if !{outp}.is_null() {{ *({outp} as *mut u64) = 0; }} }} ")
        }
        _ => String::new(),
    };
    let body = match &rmap {
        None => format!("{touch}crate::stub::hit(\"{name}\");"),
        Some(r) => {
            // A `VkResult` stub returns the API-defined error (never `VK_SUCCESS`); any other return is
            // the truthful zero/`VK_FALSE`/NULL default.
            let retval = if ret.trim() == "VkResult" {
                vk_error.to_string()
            } else {
                default_for(r).to_string()
            };
            format!("{touch}{init_out}crate::stub::hit(\"{name}\"); {retval}")
        }
    };
    writeln!(out, "#[no_mangle]\npub extern \"C\" fn {name}({sig}){arrow} {{ {body} }}\n").unwrap();
}

/// Emit the generated capability inventory (`CAPABILITIES` + `CAP_FULL`/`CAP_PARTIAL`/`CAP_STUB`) and
/// the Vulkan-1.0 mandatory-core census (`CORE_1_0_*`). This is Phase 0's machine-checkable "no command
/// advertised without a full/partial/stub record" deliverable.
fn emit_capabilities(out: &mut String, inventory: &[(String, &'static str, i32, String, &'static str)]) {
    let mut full = 0usize;
    let mut partial = 0usize;
    let mut stub = 0usize;
    let (mut core10_total, mut core10_impl) = (0usize, 0usize);
    writeln!(out, "\n/// The generated capability inventory — one record per exported `vk*` entry point:").unwrap();
    writeln!(out, "/// full/partial/stub, the `VkResult` each stub returns, and its core-version/extension origin.").unwrap();
    writeln!(out, "pub static CAPABILITIES: &[crate::capability::Entry] = &[").unwrap();
    for (name, level, err, origin, note) in inventory {
        match *level {
            "Full" => full += 1,
            "Partial" => partial += 1,
            "Stub" => stub += 1,
            other => panic!("bad capability level {other:?} for {name}"),
        }
        if origin == "core:1.0" {
            core10_total += 1;
            if *level != "Stub" {
                core10_impl += 1;
            }
        }
        let note_esc = note.replace('\\', "\\\\").replace('"', "\\\"");
        writeln!(
            out,
            "    crate::capability::Entry {{ name: {name:?}, cap: crate::capability::Cap::{level}, vk_error: {err}, origin: {origin:?}, note: \"{note_esc}\" }},"
        )
        .unwrap();
    }
    writeln!(out, "];").unwrap();
    writeln!(out, "/// Count of `full` entry points (real body, complete for the bring-up model).").unwrap();
    writeln!(out, "pub const CAP_FULL: usize = {full};").unwrap();
    writeln!(out, "/// Count of `partial` (bounded-domain) entry points.").unwrap();
    writeln!(out, "pub const CAP_PARTIAL: usize = {partial};").unwrap();
    writeln!(out, "/// Count of `stub` entry points (each returns a truthful non-success value).").unwrap();
    writeln!(out, "pub const CAP_STUB: usize = {stub};").unwrap();
    writeln!(out, "/// Vulkan-1.0 mandatory-core census: total core:1.0 commands in the exported surface.").unwrap();
    writeln!(out, "pub const CORE_1_0_TOTAL: usize = {core10_total};").unwrap();
    writeln!(out, "/// Vulkan-1.0 mandatory-core census: how many have a real (full/partial) body.").unwrap();
    writeln!(out, "pub const CORE_1_0_IMPLEMENTED: usize = {core10_impl};").unwrap();
}

/// C return type -> Rust type (None == `void`/unit). Most Vulkan entry points return `VkResult`
/// (an int-sized enum → `i32`); `vkGet*ProcAddr` return `PFN_vkVoidFunction` (a pointer).
fn map_ret(c: &str, ctx: &str, kinds: &HashMap<String, String>) -> Option<String> {
    if c == "void" {
        return None;
    }
    Some(map_type(c, ctx, kinds))
}

/// C type string -> Rust C-ABI type. Pointers (single/double, const/mut) lower to a `c_void` pointer
/// — a pointer is a pointer at the ABI, so the pointee is irrelevant for a stub's signature. Only a
/// bare by-value type consults the T-table (fail-loud on an unclassified base type). Same structure
/// as the GL/CUDA generators.
fn map_type(c: &str, ctx: &str, kinds: &HashMap<String, String>) -> String {
    let c = c.trim();
    if c.ends_with("*const*") || c.ends_with("* const*") {
        return "*const *const core::ffi::c_void".to_string();
    }
    if let Some(rest) = c.strip_suffix("**") {
        let is_const = rest.trim_start().starts_with("const ");
        return if is_const {
            "*const *const core::ffi::c_void".to_string()
        } else {
            "*mut *mut core::ffi::c_void".to_string()
        };
    }
    if let Some(rest) = c.strip_suffix('*') {
        let is_const = rest.trim_start().starts_with("const ");
        return if is_const {
            "*const core::ffi::c_void".to_string()
        } else {
            "*mut core::ffi::c_void".to_string()
        };
    }
    scalar(c, ctx, kinds).to_string()
}

/// A bare (by-value) Vulkan/C type -> its Rust C-ABI representation, via the vk.xml-derived T-table
/// (dispatchable handle → pointer, non-dispatchable handle → u64, enum → i32, bitmask → u32/u64,
/// funcptr → pointer) or a concrete scalar token. Fail-loud on anything unclassified so a registry
/// bump surfaces at build time rather than silently mis-typing the ABI.
fn scalar(c: &str, ctx: &str, kinds: &HashMap<String, String>) -> String {
    let base = c.strip_prefix("const ").unwrap_or(c).trim();
    let kind = kinds.get(base).map(String::as_str).unwrap_or(base);
    match kind {
        // T-table kinds
        "disp" | "funcptr" => "*mut core::ffi::c_void".to_string(),
        "nondisp" => "u64".to_string(),
        "enum" => "i32".to_string(),
        "enum64" => "i64".to_string(),
        "bitmask" => "u32".to_string(),
        "bitmask64" => "u64".to_string(),
        "struct" => panic!("by-value struct/union param {base:?} (in {ctx}); needs a real layout"),
        // concrete scalar tokens (from the T-table basetype resolution, or a plain C scalar)
        "void" => "core::ffi::c_void".to_string(),
        "char" => "core::ffi::c_char".to_string(),
        "i8" => "i8".to_string(),
        "i16" => "i16".to_string(),
        "i32" | "int" | "int32_t" => "i32".to_string(),
        "i64" | "int64_t" => "i64".to_string(),
        "u8" | "uint8_t" => "u8".to_string(),
        "u16" | "uint16_t" => "u16".to_string(),
        "u32" | "uint32_t" => "u32".to_string(),
        "u64" | "uint64_t" => "u64".to_string(),
        "usize" | "size_t" => "usize".to_string(),
        "f32" | "float" => "f32".to_string(),
        "f64" | "double" => "f64".to_string(),
        other => panic!("unmapped by-value type {other:?} (base {base:?} in {ctx}); extend build.rs"),
    }
}

fn default_for(rust_ty: &str) -> &'static str {
    if rust_ty.starts_with("*const") {
        "core::ptr::null()"
    } else if rust_ty.starts_with("*mut") {
        "core::ptr::null_mut()"
    } else {
        // For the `VkResult`/`i32` return this is `VK_SUCCESS` (0) — a benign "did nothing, ok"
        // default so an app keeps running while the entry point is still a stub (matches the siblings).
        "0"
    }
}

fn sanitize(name: &str, i: usize) -> String {
    const KW: &[&str] = &[
        "type", "ref", "box", "in", "fn", "let", "match", "move", "mut", "as", "impl", "loop",
        "where", "self", "final", "override", "become",
    ];
    if name.is_empty() {
        return format!("a{i}");
    }
    if KW.contains(&name) {
        return format!("{name}_");
    }
    name.to_string()
}

fn env(k: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| panic!("env {k} not set"))
}

/// Entry points hand-implemented in `src/` (so the generator skips them to avoid duplicate symbols).
/// The **bring-up** set: the ICD proc-addr resolvers, instance + physical-device enumeration and
/// property queries (reporting the "dd Metal (Vulkan)" device), device + queue + command-pool
/// creation. Everything else in the vk.xml surface is a generated stub, ported incrementally.
/// Ported from Vulkan-Loader (ICD interface), MoltenVK (object model / property values), ash (types).
const IMPLEMENTED: &[&str] = &[
    // ---- proc-addr dispatch (icd.rs) ----
    "vkGetInstanceProcAddr",
    "vkGetDeviceProcAddr",
    // ---- global / instance-level (instance.rs) ----
    "vkEnumerateInstanceVersion",
    "vkEnumerateInstanceExtensionProperties",
    "vkEnumerateInstanceLayerProperties",
    "vkCreateInstance",
    "vkDestroyInstance",
    "vkEnumeratePhysicalDevices",
    "vkGetPhysicalDeviceProperties",
    "vkGetPhysicalDeviceFeatures",
    "vkGetPhysicalDeviceMemoryProperties",
    "vkGetPhysicalDeviceQueueFamilyProperties",
    "vkGetPhysicalDeviceFormatProperties",
    // ...2 property queries (vkcube uses these via VK_KHR_get_physical_device_properties2)
    "vkGetPhysicalDeviceProperties2",
    "vkGetPhysicalDeviceFeatures2",
    "vkGetPhysicalDeviceMemoryProperties2",
    "vkGetPhysicalDeviceQueueFamilyProperties2",
    "vkGetPhysicalDeviceFormatProperties2",
    "vkEnumerateDeviceExtensionProperties",
    "vkEnumerateDeviceLayerProperties",
    // ---- device + queue + command pool (device.rs) ----
    "vkCreateDevice",
    "vkDestroyDevice",
    "vkGetDeviceQueue",
    "vkCreateCommandPool",
    "vkDestroyCommandPool",
    "vkAllocateCommandBuffers",
    "vkFreeCommandBuffers",
    // ---- increment 2: functional execution path (Vulkan -> dd-gpu IR -> host Metal seam) ----
    // memory + buffers + images (memory.rs)
    "vkCreateBuffer",
    "vkDestroyBuffer",
    "vkGetBufferMemoryRequirements",
    "vkGetImageMemoryRequirements",
    "vkAllocateMemory",
    "vkFreeMemory",
    "vkBindBufferMemory",
    "vkMapMemory",
    "vkUnmapMemory",
    "vkFlushMappedMemoryRanges",
    "vkInvalidateMappedMemoryRanges",
    "vkCreateImage",
    "vkDestroyImage",
    "vkGetImageSubresourceLayout",
    "vkBindImageMemory",
    "vkCreateImageView",
    "vkDestroyImageView",
    // shaders + pipelines + render pass (pipeline.rs)
    "vkCreateShaderModule",
    "vkDestroyShaderModule",
    "vkCreatePipelineLayout",
    "vkDestroyPipelineLayout",
    "vkCreateComputePipelines",
    "vkCreateGraphicsPipelines",
    "vkDestroyPipeline",
    "vkCreateRenderPass",
    "vkDestroyRenderPass",
    "vkCreateFramebuffer",
    "vkDestroyFramebuffer",
    // descriptors (descriptor.rs)
    "vkCreateDescriptorSetLayout",
    "vkDestroyDescriptorSetLayout",
    "vkCreateDescriptorPool",
    "vkDestroyDescriptorPool",
    "vkAllocateDescriptorSets",
    "vkFreeDescriptorSets",
    "vkUpdateDescriptorSets",
    // command buffers + submit + sync (command.rs)
    "vkBeginCommandBuffer",
    "vkEndCommandBuffer",
    "vkResetCommandBuffer",
    "vkResetCommandPool",
    "vkCmdBindPipeline",
    "vkCmdBindDescriptorSets",
    "vkCmdBindVertexBuffers",
    "vkCmdBindIndexBuffer",
    "vkCmdSetViewport",
    "vkCmdSetScissor",
    "vkCmdDispatch",
    "vkCmdBeginRenderPass",
    "vkCmdEndRenderPass",
    "vkCmdDraw",
    "vkCmdDrawIndexed",
    "vkCmdCopyBuffer",
    "vkCmdCopyBufferToImage",
    "vkCmdCopyImageToBuffer",
    "vkCmdCopyImage",
    "vkCmdBlitImage",
    "vkCmdResolveImage",
    "vkCmdClearColorImage",
    "vkCmdPipelineBarrier",
    "vkQueueSubmit",
    "vkQueueWaitIdle",
    "vkDeviceWaitIdle",
    "vkCreateFence",
    "vkDestroyFence",
    "vkWaitForFences",
    "vkResetFences",
    "vkGetFenceStatus",
    "vkCreateSemaphore",
    "vkDestroySemaphore",
    // ---- increment 3: WSI + present (wsi.rs) — the vkcube-through-dd-shim-vk path ----
    "vkCreateWaylandSurfaceKHR",
    "vkDestroySurfaceKHR",
    "vkGetPhysicalDeviceWaylandPresentationSupportKHR",
    "vkGetPhysicalDeviceSurfaceSupportKHR",
    "vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
    "vkGetPhysicalDeviceSurfaceFormatsKHR",
    "vkGetPhysicalDeviceSurfacePresentModesKHR",
    "vkCreateSwapchainKHR",
    "vkDestroySwapchainKHR",
    "vkGetSwapchainImagesKHR",
    "vkAcquireNextImageKHR",
    "vkQueuePresentKHR",
    // ---- increment 4: complete the Vulkan 1.0 mandatory core (query.rs / event.rs / memory.rs /
    // pipeline.rs / instance.rs / descriptor.rs / command.rs) — drives the core:1.0 census to 137/137.
    // Ported from MoltenVK: MVKQueryPool, MVKSync (MVKEvent), MVKSampler, MVKBufferView,
    // MVKPipelineCache, MVKPhysicalDevice format queries, MVKCommandEncoderState (dynamic state),
    // MVKCmd{PushConstants,FillBuffer,BufferUpdate,ClearAttachments,ExecuteCommands}, MVKQueue::bindSparse.
    // query pools + timestamps
    "vkCreateQueryPool",
    "vkDestroyQueryPool",
    "vkGetQueryPoolResults",
    "vkCmdResetQueryPool",
    "vkCmdBeginQuery",
    "vkCmdEndQuery",
    "vkCmdWriteTimestamp",
    "vkCmdCopyQueryPoolResults",
    // events (host + device)
    "vkCreateEvent",
    "vkDestroyEvent",
    "vkGetEventStatus",
    "vkSetEvent",
    "vkResetEvent",
    "vkCmdSetEvent",
    "vkCmdResetEvent",
    "vkCmdWaitEvents",
    // samplers + buffer views
    "vkCreateSampler",
    "vkDestroySampler",
    "vkCreateBufferView",
    "vkDestroyBufferView",
    // pipeline cache
    "vkCreatePipelineCache",
    "vkDestroyPipelineCache",
    "vkGetPipelineCacheData",
    "vkMergePipelineCaches",
    // memory + sparse + format queries
    "vkGetDeviceMemoryCommitment",
    "vkGetImageSparseMemoryRequirements",
    "vkGetPhysicalDeviceImageFormatProperties",
    "vkGetPhysicalDeviceSparseImageFormatProperties",
    "vkGetRenderAreaGranularity",
    "vkResetDescriptorPool",
    // dynamic pipeline state
    "vkCmdSetLineWidth",
    "vkCmdSetDepthBias",
    "vkCmdSetDepthBounds",
    "vkCmdSetBlendConstants",
    "vkCmdSetStencilCompareMask",
    "vkCmdSetStencilWriteMask",
    "vkCmdSetStencilReference",
    // push constants + inline buffer ops + clears + subpass + secondary + indirect
    "vkCmdPushConstants",
    "vkCmdUpdateBuffer",
    "vkCmdFillBuffer",
    "vkCmdClearAttachments",
    "vkCmdClearDepthStencilImage",
    "vkCmdNextSubpass",
    "vkCmdExecuteCommands",
    "vkCmdDrawIndirect",
    "vkCmdDrawIndexedIndirect",
    "vkCmdDispatchIndirect",
    // sparse binding (synchronization only; no sparse residency)
    "vkQueueBindSparse",
];
