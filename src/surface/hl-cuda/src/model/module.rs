//! Loaded PTX modules + resolved function handles + the `cuLaunchKernel` argument model.
//!
//! Ported from `hl-gpu/src/cuda.rs` (`PtxModule`, `Function`, `KernelArg`). A module keeps the PTX
//! source text and the parsed entry-point names — enough for `cuModuleGetFunction` and to forward the
//! kernel descriptor at launch. The full PTX → kernel-IR translation is [`crate::adapter::ptx`]; the
//! host executor compiles the forwarded source, so the module itself only needs the entry-name scan.

use std::collections::HashMap;

/// A `.global` / `.const` variable declared in PTX: the symbol name a host `cuModuleGetGlobal` looks up
/// and its byte size (element size × array length). Modeled so a `__device__`/`__constant__` global
/// resolves to a real backing device buffer instead of a false "not found".
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GlobalVar {
    pub name: String,
    pub size: u64,
}

/// A loaded PTX module: the source text, its `.entry` kernel names, and its `.global`/`.const` variable
/// declarations — all in declaration order.
#[derive(Clone, PartialEq, Debug)]
pub struct PtxModule {
    pub source: String,
    pub entries: Vec<String>,
    pub globals: Vec<GlobalVar>,
}

/// Byte size of a PTX scalar type suffix (`.b8`/`.u32`/`.f64`/…). `None` for a non-type token.
struct PtxType<'a>(&'a str);

impl PtxType<'_> {
    fn bytes(&self) -> Option<u64> {
        match self.0 {
            ".b8" | ".u8" | ".s8" => Some(1),
            ".b16" | ".u16" | ".s16" | ".f16" => Some(2),
            ".b32" | ".u32" | ".s32" | ".f32" => Some(4),
            ".b64" | ".u64" | ".s64" | ".f64" => Some(8),
            ".b128" | ".u128" | ".s128" => Some(16),
            _ => None,
        }
    }
}

/// Parse one PTX line as a `.global`/`.const` variable declaration, returning its symbol + byte size.
/// `None` if the line is not such a declaration (kernel bodies' `ld.global`/`st.global`/`cvta.global`
/// instructions start with `ld`/`st`/`cvta`, never a bare `.global` state-space directive, so they are
/// rejected here). Handles optional linkage qualifiers, `.align`, and `.vN` vector widths, and the
/// `name[count]` array form (an initializer, if any, is ignored).
impl GlobalVar {
    fn parse(line: &str) -> Option<Self> {
        let mut toks = line.trim().split_whitespace();
        // Skip leading linkage qualifiers to reach the state-space directive.
        let mut tok = toks.next()?;
        while matches!(
            tok,
            ".visible" | ".extern" | ".weak" | ".common" | ".hidden" | ".protected"
        ) {
            tok = toks.next()?;
        }
        // A variable declaration's state space is `.global` or `.const`; anything else (incl. instructions
        // like `ld.global.f32`, whose first token is `ld.global.f32`) is not a global we model here.
        if tok != ".global" && tok != ".const" {
            return None;
        }
        // Walk `.align N` / `.vN` qualifiers until the scalar type, then take the name token.
        let mut vec_width = 1u64;
        let mut elem = None;
        let mut name_tok = None;
        while let Some(t) = toks.next() {
            if t == ".align" {
                toks.next(); // consume the alignment value
            } else if let Some(w) = t.strip_prefix(".v").and_then(|n| n.parse::<u64>().ok()) {
                vec_width = w.max(1);
            } else if let Some(sz) = PtxType(t).bytes() {
                elem = Some(sz);
                name_tok = toks.next();
                break;
            }
        }
        let elem = elem?;
        let raw = name_tok?;
        // name is up to `[` (array), `=` (initializer), or `;`; the count lives between `[` and `]`.
        let (name, count) = match raw.split_once('[') {
            Some((n, rest)) => {
                let inner = rest.split(']').next().unwrap_or("").trim();
                (n, inner.parse::<u64>().unwrap_or(1).max(1))
            }
            None => (raw.trim_end_matches([';', '=']), 1),
        };
        let name = name.trim_end_matches([';', '=']).trim();
        if name.is_empty() {
            return None;
        }
        Some(Self {
            name: name.to_string(),
            size: elem * vec_width * count,
        })
    }
}

impl PtxModule {
    /// Parse `.entry` kernel names + `.global`/`.const` variable declarations out of PTX text. Real and
    /// testable; does NOT translate kernel bodies (that is [`crate::adapter::ptx::compile`], run host-side
    /// per launch).
    pub fn parse(source: &str) -> Self {
        let mut entries = Vec::new();
        let mut globals = Vec::new();
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
            } else if let Some(g) = GlobalVar::parse(l) {
                globals.push(g);
            }
        }
        Self {
            source: source.to_string(),
            entries,
            globals,
        }
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
        Self {
            map: HashMap::new(),
            next_id: 1,
        }
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

    /// `cuModuleGetGlobal(module, name)` → the byte size of the module's `.global`/`.const` variable
    /// `name`, or `None` if the module is unknown or declares no such global.
    pub fn get_global(&self, module: u32, name: &str) -> Option<u64> {
        let m = self.map.get(&module)?;
        m.globals.iter().find(|g| g.name == name).map(|g| g.size)
    }

    /// The (source, entry-name) a launch forwards as its kernel descriptor. `None` if the handle is
    /// stale (module freed) or the entry index is out of range.
    pub fn entry_source(&self, func: Function) -> Option<(String, String)> {
        let m = self.map.get(&func.module)?;
        let entry = m.entries.get(func.entry as usize)?;
        Some((m.source.clone(), entry.clone()))
    }
}
