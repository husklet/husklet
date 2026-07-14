#!/usr/bin/env python3
# hl-shim-cuda manifest extractor. Mirrors hl-shim-gl/registry/extract_from_khronos.py, but the
# CUDA Driver API has no Khronos-style XML registry, so the entry-point surface is extracted from an
# OPEN, CLEAN-ROOM C source of `cu*` definitions rather than a vendor header.
#
# Source of truth: dd's own `hl-gpu/cuda/cuda_shim.c` — the hand-rolled clean-room CUDA Driver API
# shim ("No NVIDIA source is used"; see its file header). It is the CUDA analogue of `gl_shim.c`, and
# the spec-faithful `cu*` surface dd already ships (init / device / context / module / memory / stream /
# event / launch / occupancy / pointer-attribute families, with the versioned `_v2`/`_v3` symbol
# names). We parse the top-level `CUresult cuXxx(...)` definitions out of it and emit the compact
# `cuda_driver.manifest` build.rs consumes. The manifest is committed so the build needs no C source
# and no network; regenerate it (and re-review build.rs's IMPLEMENTED set) when the surface changes.
#
#   usage: extract_cuda_manifest.py <cuda_shim.c> <out.manifest>
import re, sys

def load(path):
    src = open(path).read()
    src = re.sub(r'/\*.*?\*/', ' ', src, flags=re.S)   # strip block comments
    src = re.sub(r'//[^\n]*', ' ', src)                 # strip line comments
    # top-level driver-API definitions: `CUresult cuXxx( <params> ) {`
    pat = re.compile(r'\bCUresult\s+(cu[A-Za-z0-9_]+)\s*\(([^;{]*?)\)\s*\{', re.S)
    out = []
    seen = set()
    for m in pat.finditer(src):
        name = m.group(1)
        if name in seen:
            continue
        seen.add(name)
        raw = ' '.join(m.group(2).split())
        params = []
        if raw and raw != 'void':
            for part in raw.split(','):
                part = part.strip()
                mm = re.match(r'(.*?)([A-Za-z_][A-Za-z0-9_]*)\s*$', part)
                if mm and mm.group(1).strip():
                    ty, pn = mm.group(1).strip(), mm.group(2)
                else:
                    ty, pn = part, ''            # unnamed (build.rs synthesizes a name)
                params.append((ty, pn))
        out.append((name, 'CUresult', params))
    return sorted(out)

def emit(entries, out):
    for name, ret, params in entries:
        ps = ';'.join(f"{ty}|{pn}" for ty, pn in params)
        out.write(f"CU\t{name}\t{ret}\t{ps}\n")

src = sys.argv[1]; outp = sys.argv[2]
entries = load(src)
with open(outp, 'w') as out:
    emit(entries, out)
print(f"CUDA driver-API commands: {len(entries)}")
