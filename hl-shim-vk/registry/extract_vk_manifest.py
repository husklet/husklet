#!/usr/bin/env python3
# hl-shim-vk manifest extractor. Mirrors hl-shim-gl/registry/extract_from_khronos.py and
# hl-shim-cuda/registry/extract_cuda_manifest.py, but reads the Khronos **Vulkan** registry
# (`vk.xml`) directly — Vulkan *does* ship a machine-readable XML registry (unlike the CUDA
# Driver API), so this is the closest analogue to the GLES2/EGL extractor.
#
# It emits the compact `vk_commands.manifest` that `build.rs` consumes to code-generate the full
# `vk*` C-ABI export surface (an ICD must expose every core + extension entry point via
# vk_icdGetInstanceProcAddr). Two record kinds:
#
#   T\t<typename>\t<kind>              -- a by-value type classification (build.rs maps bare params)
#   C\t<vkName>\t<retType>\t<params>   -- one command; params are `<ctype>|<name>` ';'-joined
#
# `<kind>` is one of: disp (dispatchable handle -> pointer), nondisp (64-bit non-dispatchable
# handle), enum (int-sized enum), enum64 (64-bit enum), bitmask (VkFlags=u32), bitmask64
# (VkFlags64=u64), funcptr (PFN_* -> pointer), or a concrete scalar token (i8/u8/.../f32/f64/
# usize/char/void). Pointer params never need a `T` entry — a pointer is a pointer, so build.rs
# lowers every `*`-suffixed C type to a `c_void` pointer regardless of pointee.
#
# The manifest is committed so the build needs neither the multi-MB vk.xml nor the network;
# regenerate it (and re-review build.rs's IMPLEMENTED set) when bumping the registry:
#
#   usage: extract_vk_manifest.py <vk.xml> <out.manifest>
import sys
import xml.etree.ElementTree as ET

# Plain C scalars (used by value in a few commands) -> Rust C-ABI token. Also a handful of
# platform-extern base scalars that appear by value in WSI command params (xcb/xlib/ggp/fuchsia).
C_SCALAR = {
    "void": "void", "char": "char",
    "float": "f32", "double": "f64",
    "int8_t": "i8", "int16_t": "i16", "int32_t": "i32", "int64_t": "i64", "int": "i32",
    "uint8_t": "u8", "uint16_t": "u16", "uint32_t": "u32", "uint64_t": "u64",
    "size_t": "usize",
    "xcb_window_t": "u32", "xcb_visualid_t": "u32",
    "Window": "u64", "VisualID": "u64", "RROutput": "u64",
    "zx_handle_t": "u32", "GgpStreamDescriptor": "u32", "GgpFrameToken": "u64",
    "_screen_context": "void", "_screen_window": "void",
}


def text_of(node):
    """Full C text of a <type>/<proto>/<param>, minus the trailing <name>. A NUL marker separates
    any post-name array suffix (e.g. `[4]`)."""
    parts = []
    if node.text:
        parts.append(node.text)
    for child in node:
        if child.tag == "name":
            if child.tail:
                parts.append("\x00" + child.tail)
            continue
        if child.text:
            parts.append(child.text)
        if child.tail:
            parts.append(child.tail)
    return "".join(parts)


def name_of(node):
    n = node.find("name")
    return n.text if n is not None else None


def classify_types(root):
    """Build {typename: kind} for every by-value-relevant type in the registry."""
    kinds = {}
    for t in root.iter("type"):
        cat = t.get("category")
        nm = t.get("name") or name_of(t)
        if not nm:
            continue
        if cat == "handle":
            body = "".join(t.itertext())
            kinds[nm] = "nondisp" if "NON_DISPATCHABLE" in body else "disp"
        elif cat == "enum":
            kinds[nm] = "enum64" if t.get("bitwidth") == "64" else "enum"
        elif cat == "bitmask":
            body = "".join(t.itertext())
            kinds[nm] = "bitmask64" if "VkFlags64" in body else "bitmask"
        elif cat == "funcpointer":
            kinds[nm] = "funcptr"
        elif cat == "basetype":
            inner = t.find("type")
            if inner is not None and inner.text in C_SCALAR:
                kinds[nm] = C_SCALAR[inner.text]
            else:
                kinds[nm] = "struct"  # opaque struct basetype (ANativeWindow, ...) -> ptr-only
        elif cat in ("struct", "union"):
            kinds[nm] = "struct"
    kinds.setdefault("VkFlags", "u32")
    kinds.setdefault("VkFlags64", "u64")
    return kinds


def parse_param(node):
    """-> (ctype, name). ctype is normalized C: pointers as `*`/`**`, a leading `const`, base type
    name; arrays `[N]` collapse to a pointer (array-to-pointer param decay)."""
    nm = name_of(node)
    raw = text_of(node)
    arr = "[" in raw.split("\x00", 1)[1] if "\x00" in raw else False
    raw = raw.replace("\x00", " ")
    toks = raw.replace("*", " * ").split()
    is_const = "const" in toks
    stars = raw.count("*")
    base = None
    for tk in toks:
        if tk in ("const", "*", "struct"):
            continue
        base = tk
        break
    if base is None:
        base = "void"
    if arr and stars == 0:
        stars = 1
    if stars == 0:
        ctype = base
    elif stars == 1:
        ctype = ("const " if is_const else "") + base + "*"
    else:
        ctype = ("const " if is_const else "") + base + "*" * stars
    return ctype, (nm or "")


def is_vulkan(node):
    """Whether an <command>/<param>'s `api` attribute includes the plain "vulkan" API (as opposed to
    a "vulkansc"-only entity). Absent attribute == applies to all APIs → included."""
    api = node.get("api")
    return api is None or "vulkan" in api.split(",")


def collect_commands(root):
    """Return [(name, ret, [(ctype,name)])], resolving aliases, one per exported symbol. Params (and
    whole commands) restricted to the plain "vulkan" API — `api="vulkansc"` variants (which duplicate
    a param name with a different type) are dropped so signatures stay single-valued and ABI-faithful."""
    defs = {}      # name -> (ret, params)
    aliases = {}   # alias name -> target name
    for c in root.find("commands"):
        if c.tag != "command":
            continue
        if not is_vulkan(c):
            continue
        al = c.get("alias")
        if al:
            aliases[c.get("name")] = al
            continue
        proto = c.find("proto")
        if proto is None:
            continue
        name = name_of(proto)
        ret = text_of(proto).strip() or "void"
        params = [parse_param(p) for p in c.findall("param") if is_vulkan(p)]
        defs[name] = (ret, params)
    out = list(defs.items())
    for alias, target in aliases.items():
        if target in defs:
            out.append((alias, defs[target]))
    seen, uniq = set(), []
    for name, (ret, params) in sorted(out):
        if name in seen:
            continue
        seen.add(name)
        uniq.append((name, ret, params))
    return uniq


def bare(ctype):
    """The base type name of a non-pointer ctype, else None."""
    if "*" in ctype:
        return None
    return ctype.replace("const", "").strip()


def main():
    vk, outp = sys.argv[1], sys.argv[2]
    root = ET.parse(vk).getroot()
    kinds = classify_types(root)
    cmds = collect_commands(root)

    used_types = set()
    kept, skipped = [], []
    for name, ret, params in cmds:
        need = [bare(ct) for ct, _ in params] + [bare(ret)]
        need = [b for b in need if b and b != "void"]
        bad = [b for b in need if b not in kinds and b not in C_SCALAR]
        if bad:
            skipped.append((name, bad))
            continue
        used_types.update(need)
        kept.append((name, ret, params))

    with open(outp, "w") as f:
        f.write("# @generated by extract_vk_manifest.py from Khronos vk.xml — DO NOT EDIT.\n")
        f.write("# Format: `T<TAB>type<TAB>kind` (by-value classification) then\n")
        f.write("#         `C<TAB>vkName<TAB>ret<TAB>ctype|name;ctype|name;...` (one per export).\n")
        for t in sorted(used_types):
            f.write(f"T\t{t}\t{kinds.get(t) or C_SCALAR.get(t)}\n")
        for name, ret, params in kept:
            ps = ";".join(f"{ct}|{pn}" for ct, pn in params)
            f.write(f"C\t{name}\t{ret}\t{ps}\n")

    print(f"vk commands emitted: {len(kept)}  types: {len(used_types)}  skipped: {len(skipped)}")
    for name, bad in skipped:
        print(f"  skip {name}: unresolved by-value {bad}")


if __name__ == "__main__":
    main()
