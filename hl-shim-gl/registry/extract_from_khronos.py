#!/usr/bin/env python3
# hl-shim-gl registry extractor. Reads the Khronos API registry XML (gl.xml, egl.xml — as vendored by
# the `khronos_api` crate, or a fresh checkout of KhronosGroup/OpenGL-Registry + EGL-Registry) and emits
# the compact `gles2_egl.manifest` build.rs consumes. This is the "generated from the Khronos XML"
# completeness path; the manifest is committed so the build needs no XML and no network.
#   usage: extract_from_khronos.py <gl.xml> <egl.xml> <out.manifest>
import re, sys, xml.etree.ElementTree as ET

def load(path, apis, feature_prefixes):
    t = ET.parse(path); r = t.getroot()
    # command defs
    cmds = {}
    for c in r.iter('command'):
        proto = c.find('proto')
        if proto is None: continue
        name_el = proto.find('name')
        if name_el is None: continue
        name = name_el.text
        # return type = all text in proto minus the <name>
        ret = ''.join(proto.itertext())
        ret = ret[:ret.rfind(name)].strip() if name in ret else ret.strip()
        params = []
        for p in c.findall('param'):
            pn = p.find('name').text
            ptype = ''.join(p.itertext())
            ptype = ptype[:ptype.rfind(pn)].strip()
            params.append((ptype, pn))
        cmds[name] = (ret, params)
    # feature require/remove sets
    req = set(); rem = set()
    for f in r.iter('feature'):
        if f.get('api') not in apis: continue
        nm = f.get('name','')
        if not any(nm.startswith(px) for px in feature_prefixes): continue
        for rq in f.findall('require'):
            for cc in rq.findall('command'): req.add(cc.get('name'))
        for rmv in f.findall('remove'):
            for cc in rmv.findall('command'): rem.add(cc.get('name'))
    names = sorted(req - rem)
    return [(n, cmds[n][0], cmds[n][1]) for n in names if n in cmds]

def emit(entries, label, out):
    for name, ret, params in entries:
        ps = ';'.join(f"{ty}|{pn}" for ty,pn in params)
        out.write(f"{label}\t{name}\t{ret}\t{ps}\n")

glxml = sys.argv[1]; eglxml = sys.argv[2]; outp = sys.argv[3]
gles = load(glxml, {'gles2'}, ['GL_ES_VERSION_'])
egl  = load(eglxml, {'egl'}, ['EGL_VERSION_'])
with open(outp,'w') as out:
    emit(gles, 'GL', out)
    emit(egl, 'EGL', out)
print(f"GLES2 commands: {len(gles)}   EGL commands: {len(egl)}")
