//! Loaded PTX modules + resolved function handles + the `cuLaunchKernel` argument model.
//!
//! Ported from `hl-gpu/src/cuda.rs` (`PtxModule`, `Function`, `KernelArg`). A module keeps the PTX
//! source text and the parsed entry-point names — enough for `cuModuleGetFunction` and to forward the
//! kernel descriptor at launch. The full PTX → kernel-IR translation is [`crate::adapter::ptx`]; the
//! host executor compiles the forwarded source, so the module itself only needs the entry-name scan.

use std::collections::HashMap;

/// A loaded PTX module: the source text + its `.entry` kernel names, in declaration order.
#[derive(Clone, PartialEq, Debug)]
pub struct PtxModule {
    pub source: String,
    pub entries: Vec<String>,
}

impl PtxModule {
    /// Parse `.entry` / `.visible .entry` kernel names out of PTX text. Real and testable; does NOT
    /// translate kernel bodies (that is [`crate::adapter::ptx::compile`], run host-side per launch).
    pub fn parse(source: &str) -> Self {
        let mut entries = Vec::new();
        for line in source.lines() {
            let l = line.trim();
            if let Some(rest) = l.strip_prefix(".entry").map(str::trim).or_else(|| {
                l.strip_prefix(".visible")
                    .map(str::trim)
                    .and_then(|r| r.strip_prefix(".entry"))
                    .map(str::trim)
            }) {
                // name is up to '(' or whitespace
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                    .collect();
                if !name.is_empty() {
                    entries.push(name);
                }
            }
        }
        Self { source: source.to_string(), entries }
    }
}

/// A resolved CUDA function handle (module id + entry index within that module).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Function {
    pub module: u32,
    pub entry: u32,
}

/// A kernel argument in a `cuLaunchKernel` call.
#[derive(Clone, PartialEq, Debug)]
pub enum KernelArg {
    /// A device-pointer argument → bound as its own storage buffer (binding `region+1`).
    Ptr(super::device::DevicePtr),
    /// A by-value scalar → packed into the flat kernel-parameter blob (binding 0).
    Scalar(Vec<u8>),
}

/// The per-context module table: module id → [`PtxModule`], with a monotonic id counter.
#[derive(Debug, Default)]
pub struct Modules {
    map: HashMap<u32, PtxModule>,
    next_id: u32,
}

impl Modules {
    pub fn new() -> Self {
        Self { map: HashMap::new(), next_id: 1 }
    }

    /// `cuModuleLoadData` — store a parsed module and return its id (entries parsed now; body compiled
    /// host-side later).
    pub fn load(&mut self, source: &str) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.map.insert(id, PtxModule::parse(source));
        id
    }

    pub fn get(&self, id: u32) -> Option<&PtxModule> {
        self.map.get(&id)
    }

    /// `cuModuleGetFunction(module, name)` → the (module, entry-index) handle, or `None` if the module
    /// is unknown or has no such entry.
    pub fn get_function(&self, module: u32, name: &str) -> Option<Function> {
        let m = self.map.get(&module)?;
        let entry = m.entries.iter().position(|e| e == name)? as u32;
        Some(Function { module, entry })
    }

    /// The (source, entry-name) a launch forwards as its kernel descriptor. `None` if the handle is
    /// stale (module freed) or the entry index is out of range.
    pub fn entry_source(&self, func: Function) -> Option<(String, String)> {
        let m = self.map.get(&func.module)?;
        let entry = m.entries.get(func.entry as usize)?;
        Some((m.source.clone(), entry.clone()))
    }
}
