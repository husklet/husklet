#!/usr/bin/env python3
"""Extract cumulative Vulkan core command membership from Khronos vk.xml.

Output is committed so audits are offline and reproducible:
  # source-revision: <git commit>
  V<TAB>major.minor<TAB>vkCommand

The regular ABI manifest intentionally flattens core and extension commands. This companion inventory preserves
the metadata needed to prove that every mandatory command for an advertised core version has a real body.
"""
from pathlib import Path
import argparse
import xml.etree.ElementTree as ET


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("vk_xml", type=Path)
    ap.add_argument("output", type=Path)
    ap.add_argument("--source-revision", required=True)
    args = ap.parse_args()

    root = ET.parse(args.vk_xml).getroot()
    feature_by_name = {f.get("name", ""): f for f in root.findall("feature")}

    def dependency_names(expr: str) -> set[str]:
        # Core feature dependencies use comma/plus composition; extracting VK_* identifiers gives the
        # dependency closure for either form without treating extension boolean syntax as Python.
        import re
        return set(re.findall(r"VK_[A-Z0-9_]+", expr or ""))

    def commands_for(feature_name: str, seen: set[str] | None = None) -> set[str]:
        seen = set() if seen is None else seen
        if feature_name in seen:
            return set()
        seen.add(feature_name)
        feature = feature_by_name.get(feature_name)
        if feature is None:
            return set()
        commands: set[str] = set()
        for dep in dependency_names(feature.get("depends", "")):
            if dep in feature_by_name:
                commands.update(commands_for(dep, seen))
        for require in feature.findall("require"):
            req_api = require.get("api")
            if req_api and "vulkan" not in req_api.split(","):
                continue
            commands.update(c.get("name") for c in require.findall("command") if c.get("name"))
        for remove in feature.findall("remove"):
            rem_api = remove.get("api")
            if rem_api and "vulkan" not in rem_api.split(","):
                continue
            commands.difference_update(c.get("name") for c in remove.findall("command") if c.get("name"))
        return commands

    versions: list[tuple[tuple[int, int], str, set[str]]] = []
    for feature in root.findall("feature"):
        api = feature.get("api", "")
        name = feature.get("name", "")
        number = feature.get("number", "")
        if "vulkan" not in api.split(",") or not name.startswith("VK_VERSION_"):
            continue
        try:
            major, minor = (int(x) for x in number.split(".")[:2])
        except ValueError:
            continue
        versions.append(((major, minor), f"{major}.{minor}", commands_for(name)))

    versions.sort()
    lines = [
        "# @generated from Khronos Vulkan-Docs xml/vk.xml — DO NOT EDIT.",
        f"# source-revision: {args.source_revision}",
        "# Format: V<TAB>core-version<TAB>mandatory-command (cumulative through that version).",
    ]
    for _, version, commands in versions:
        lines.extend(f"V\t{version}\t{name}" for name in sorted(commands))
    args.output.write_text("\n".join(lines) + "\n")
    print(" ".join(f"{version}={len(commands)}" for _, version, commands in versions))


if __name__ == "__main__":
    main()
