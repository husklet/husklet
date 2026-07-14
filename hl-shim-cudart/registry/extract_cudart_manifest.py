#!/usr/bin/env python3
# hl-shim-cudart manifest extractor. The CUDA analogue of hl-shim-cuda/registry/extract_cuda_manifest.py,
# but for the RUNTIME API. The CUDA Runtime API has no Khronos-style XML registry, so the entry-point
# surface is extracted from an OPEN, CLEAN-ROOM C source of `cuda*`/`__cuda*` definitions rather than a
# vendor header.
#
# Source of truth: dd's own `hl-gpu/cuda/cudart_shim.c` — the hand-rolled clean-room CUDA Runtime API
# shim ("No NVIDIA source is used"; see its file header). It is the runtime analogue of `cuda_shim.c`,
# and the spec-faithful `cuda*`/`__cuda*` surface dd already ships (device / error / memory / stream /
# event families + the nvcc registration glue). We parse the top-level definitions out of it and emit
# the compact base manifest build.rs consumes.
#
# The runtime API has several return types (unlike the driver's uniform CUresult), so we match a small
# closed set: `cudaError_t`, `const char*`, `void`, `void**`, `unsigned int`.
#
# The COMMITTED manifest (`cudart.manifest`) is this extracted base PLUS a hand-curated tail of the
# standard driver-backed runtime surface (cudaMallocManaged / cudaFuncGetAttributes / cudaStreamWaitEvent
# / cudaDeviceReset / ... — see the `# --- additional ...` section there) so the shipped surface is
# genuinely complete, not just what the compact oracle happens to spell out. Every tail entry maps to a
# real driver `cu*` body. Regenerate the base (and re-review build.rs's IMPLEMENTED set) when the surface
# changes.
#
#   usage: extract_cudart_manifest.py <cudart_shim.c> <out.manifest>
import re, sys

RET = r'cudaError_t|const char\s*\*|void\s*\*\*|unsigned int|void'

def load(path):
    src = open(path).read()
    src = re.sub(r'/\*.*?\*/', ' ', src, flags=re.S)   # strip block comments
    src = re.sub(r'//[^\n]*', ' ', src)                 # strip line comments
    # top-level runtime-API definitions: `<ret> cuda<Xxx>( <params> ) {` / `__cuda<Xxx>(...) {`
    pat = re.compile(r'\b(' + RET + r')\s+((?:__)?cuda[A-Za-z0-9_]+)\s*\(([^;{]*?)\)\s*\{', re.S)
    out = []
    seen = set()
    for m in pat.finditer(src):
        ret = ' '.join(m.group(1).split())   # normalize `const char *` spacing
        name = m.group(2)
        if name in seen:
            continue
        seen.add(name)
        raw = ' '.join(m.group(3).split())
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
        out.append((name, ret, params))
    return sorted(out)

def emit(entries, out):
    for name, ret, params in entries:
        ps = ';'.join(f"{ty}|{pn}" for ty, pn in params)
        out.write(f"RT\t{name}\t{ret}\t{ps}\n")

src = sys.argv[1]; outp = sys.argv[2]
entries = load(src)
with open(outp, 'w') as out:
    emit(entries, out)
print(f"CUDA runtime-API entry points (extracted base): {len(entries)}")
