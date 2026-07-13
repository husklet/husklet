#!/usr/bin/env python3
"""Generate cumulative GLES and EGL core command inventories from pinned Khronos XML registries."""
from pathlib import Path
import argparse
import xml.etree.ElementTree as ET


def versions(path: Path, api: str, prefix: str) -> list[tuple[str, set[str]]]:
    root = ET.parse(path).getroot()
    cumulative: set[str] = set()
    out: list[tuple[str, set[str]]] = []
    for feature in root.iter("feature"):
        if feature.get("api") != api or not feature.get("name", "").startswith(prefix):
            continue
        for require in feature.findall("require"):
            cumulative.update(c.get("name") for c in require.findall("command") if c.get("name"))
        for remove in feature.findall("remove"):
            cumulative.difference_update(c.get("name") for c in remove.findall("command") if c.get("name"))
        out.append((feature.get("number", ""), set(cumulative)))
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("gl_xml", type=Path)
    ap.add_argument("egl_xml", type=Path)
    ap.add_argument("output", type=Path)
    ap.add_argument("--gl-revision", required=True)
    ap.add_argument("--egl-revision", required=True)
    args = ap.parse_args()
    surfaces = [
        ("GLES", versions(args.gl_xml, "gles2", "GL_ES_VERSION_")),
        ("EGL", versions(args.egl_xml, "egl", "EGL_VERSION_")),
    ]
    lines = [
        "# @generated from Khronos OpenGL-Registry gl.xml and EGL-Registry egl.xml — DO NOT EDIT.",
        f"# gl-source-revision: {args.gl_revision}",
        f"# egl-source-revision: {args.egl_revision}",
        "# Format: V<TAB>surface<TAB>core-version<TAB>mandatory-command (cumulative).",
    ]
    for surface, rows in surfaces:
        for version, commands in rows:
            lines.extend(f"V\t{surface}\t{version}\t{name}" for name in sorted(commands))
    args.output.write_text("\n".join(lines) + "\n")
    print(" ".join(f"{surface}-{version}={len(commands)}" for surface, rows in surfaces for version, commands in rows))


if __name__ == "__main__":
    main()
