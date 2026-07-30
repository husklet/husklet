use std::collections::HashMap;
use std::fmt::Write as _;

/// `VkResult` returned by truthful generated stubs.
const VK_ERROR_FEATURE_NOT_PRESENT: i32 = -8;

pub(super) fn emit_stub(
    out: &mut String,
    name: &str,
    ret: &str,
    params: &str,
    kinds: &HashMap<String, String>,
) {
    let is_vk_result = ret.trim() == "VkResult";
    let mut sig = String::new();
    let mut argnames: Vec<String> = Vec::new();
    let mut last_out: Option<String> = None; // last param IF it is a single `*mut` output pointer
    if !params.is_empty() {
        let n = params.split(';').count();
        for (i, p) in params.split(';').enumerate() {
            let (ty, pname) = p
                .split_once('|')
                .unwrap_or_else(|| panic!("bad param {p:?} in {name}"));
            let rty = map_type(ty.trim(), name, kinds);
            let mut pn = ParameterName::sanitize(pname.trim(), i);
            // Guarantee unique parameter identifiers (a manifest can carry repeated names across
            // api-variant params). Names are cosmetic — the ABI is unaffected.
            while argnames.contains(&pn) {
                pn.push_str(&format!("_{i}"));
            }
            if i > 0 {
                sig.push_str(", ");
            }
            write!(sig, "{pn}: {rty}").unwrap();
            // A `vkCreate*`/`vkAllocate*` command's final parameter is its output handle — a single,
            // non-const `*mut` pointer. Record it so a failing stub can null it (VK_NULL_HANDLE) before
            // returning: truthful failure must initialize the output, never leave a caller reading junk.
            if i + 1 == n && rty.starts_with("*mut ") && !rty.starts_with("*mut *mut") {
                last_out = Some(pn.clone());
            }
            argnames.push(pn);
        }
    }
    let rmap = map_ret(ret.trim(), name, kinds);
    let arrow = rmap
        .as_ref()
        .map(|r| format!(" -> {r}"))
        .unwrap_or_default();
    let touch: String = argnames.iter().map(|a| format!("let _ = {a}; ")).collect();
    let init_out = match &last_out {
        Some(outp)
            if is_vk_result && (name.starts_with("vkCreate") || name.starts_with("vkAllocate")) =>
        {
            format!("unsafe {{ if !{outp}.is_null() {{ *({outp} as *mut u64) = 0; }} }} ")
        }
        _ => String::new(),
    };
    let body = match &rmap {
        None => format!("{touch}crate::stub::Stub::hit(\"{name}\");"),
        Some(r) => {
            let retval = if is_vk_result {
                VK_ERROR_FEATURE_NOT_PRESENT.to_string()
            } else {
                RustType::new(r).default_value().to_string()
            };
            format!("{touch}{init_out}crate::stub::Stub::hit(\"{name}\"); {retval}")
        }
    };
    writeln!(
        out,
        "pub extern \"C\" fn {name}({sig}){arrow} {{ {body} }}\n"
    )
    .unwrap();
}

/// C return type -> Rust type (None == `void`/unit).
fn map_ret(c: &str, ctx: &str, kinds: &HashMap<String, String>) -> Option<String> {
    if c == "void" {
        return None;
    }
    Some(map_type(c, ctx, kinds))
}

/// C type string -> Rust C-ABI type. Pointers (single/double, const/mut) lower to a `c_void` pointer —
/// a pointer is a pointer at the ABI, so the pointee is irrelevant for a stub's signature. Only a bare
/// by-value type consults the T-table (fail-loud on an unclassified base type).
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

/// A bare (by-value) Vulkan/C type -> its Rust C-ABI representation, via the vk.xml-derived T-table or
/// a concrete scalar token. Fail-loud on anything unclassified so a registry bump surfaces at build
/// time rather than silently mis-typing the ABI.
fn scalar(c: &str, ctx: &str, kinds: &HashMap<String, String>) -> String {
    let base = c.strip_prefix("const ").unwrap_or(c).trim();
    let kind = kinds.get(base).map(String::as_str).unwrap_or(base);
    match kind {
        "disp" | "funcptr" => "*mut core::ffi::c_void".to_string(),
        "nondisp" => "u64".to_string(),
        "enum" => "i32".to_string(),
        "enum64" => "i64".to_string(),
        "bitmask" => "u32".to_string(),
        "bitmask64" => "u64".to_string(),
        "struct" => panic!("by-value struct/union param {base:?} (in {ctx}); needs a real layout"),
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
        other => {
            panic!("unmapped by-value type {other:?} (base {base:?} in {ctx}); extend build.rs")
        }
    }
}

struct RustType<'a>(&'a str);

impl<'a> RustType<'a> {
    fn new(value: &'a str) -> Self {
        Self(value)
    }

    fn default_value(&self) -> &'static str {
        if self.0.starts_with("*const") {
            "core::ptr::null()"
        } else if self.0.starts_with("*mut") {
            "core::ptr::null_mut()"
        } else {
            "0" // truthful zero / VK_FALSE / NULL-equivalent for a non-VkResult scalar return.
        }
    }
}

struct ParameterName;

impl ParameterName {
    fn sanitize(name: &str, index: usize) -> String {
        const KEYWORDS: &[&str] = &[
            "type", "ref", "box", "in", "fn", "let", "match", "move", "mut", "as", "impl", "loop",
            "where", "self", "final", "override", "become",
        ];
        if name.is_empty() {
            return format!("a{index}");
        }
        if KEYWORDS.contains(&name) {
            return format!("{name}_");
        }
        name.to_string()
    }
}
