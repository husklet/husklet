#!/usr/bin/env python3
"""Fetch and audit the fixed Alpine developer APK closure; emit a patch, never edit source."""

import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
SOURCE = ROOT / "nix/developer-rootfs.nix"
ARCHES = {"amd64": "x86_64", "arm64": "aarch64"}
HASH_START = "  # APK_HASHES_START -- updated only by nix/update-developer-rootfs-apks.py output.\n"
HASH_END = "  # APK_HASHES_END\n"
PACKAGE = re.compile(r'^\s*\[ "([^"]+)" "([^"]+)" "(main|community)" (\d+) (\d+) \]$')
INDEX = re.compile(r'^\s*(main|community) = \{ url = "([^"]+)"; sha256 = "([0-9a-f]{64})"; \};$')
CONTROL = re.compile(r"^\.(pre|post)-(install|upgrade|deinstall)$|^\.trigger$")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def fetch(url, path):
    temporary = path.with_name(path.name + ".part")
    with urllib.request.urlopen(url) as source, temporary.open("wb") as target:
        while chunk := source.read(1024 * 1024):
            target.write(chunk)
        target.flush()
        os.fsync(target.fileno())
    os.replace(temporary, path)


def parse_source(text):
    packages = [match.groups() for line in text.splitlines() if (match := PACKAGE.match(line))]
    if len(packages) != 34:
        raise ValueError(f"expected 34 closure entries, found {len(packages)}")
    indexes = {}
    architecture = None
    for line in text.splitlines():
        if line.strip() in ("arm64 = {", "amd64 = {"):
            architecture = line.strip().split()[0]
        if architecture and (match := INDEX.match(line)):
            indexes[(architecture, match[1])] = (match[2], match[3])
    if len(indexes) != 4:
        raise ValueError(f"expected four signed indexes, found {len(indexes)}")
    return packages, indexes


def index_sizes(archive):
    with tarfile.open(archive, "r:gz") as bundle:
        names = bundle.getnames()
        if not any(name.startswith(".SIGN.RSA.") for name in names):
            raise ValueError(f"{archive} has no Alpine index signature member")
        records = bundle.extractfile("APKINDEX").read().decode()
    result = {}
    for block in records.split("\n\n"):
        fields = {line[0]: line[2:] for line in block.splitlines() if len(line) > 1 and line[1] == ":"}
        if "P" in fields:
            result[(fields["P"], fields["V"])] = int(fields["S"])
    return result


def control_members(path):
    output = subprocess.run(
        ["tar", "--ignore-zeros", "-tzf", path], check=True, capture_output=True, text=True
    ).stdout.splitlines()
    controls = {}
    for name in sorted(name for name in output if CONTROL.match(Path(name).name)):
        content = subprocess.run(
            ["tar", "--ignore-zeros", "-xOzf", path, name], check=True, capture_output=True
        ).stdout
        controls[name] = {"sha256": hashlib.sha256(content).hexdigest(), "bytes": len(content)}
    return controls


def nix_hash(path):
    return subprocess.run(
        ["nix", "hash", "file", "--type", "sha256", "--sri", path],
        check=True, capture_output=True, text=True,
    ).stdout.strip()


def render_hashes(hashes, scripts_audited):
    lines = [HASH_START, "  payloadHashes = {\n"]
    for architecture in ("arm64", "amd64"):
        lines.append(f"    {architecture} = {{\n")
        for name, value in hashes[architecture]:
            lines.append(f'      "{name}" = "{value}";\n')
        lines.append("    };\n")
    lines.extend(["  };\n", HASH_END])
    return "".join(lines), scripts_audited


def replace_block(text, block, scripts_audited):
    start = text.index(HASH_START)
    end = text.index(HASH_END, start) + len(HASH_END)
    updated = text[:start] + block + text[end:]
    if scripts_audited:
        updated = updated.replace("scriptsAudited = false;", "scriptsAudited = true;")
        updated = updated.replace("closureComplete = false;", "closureComplete = true;")
    return updated


def atomic_write(path, content):
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=path.name + ".", dir=path.parent)
    try:
        with os.fdopen(descriptor, "w") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def self_test():
    text = SOURCE.read_text()
    packages, indexes = parse_source(text)
    assert len(packages) == 34 and len(indexes) == 4
    text = text.replace("scriptsAudited = true;", "scriptsAudited = false;")
    text = text.replace("closureComplete = true;", "closureComplete = false;")
    block, audited = render_hashes({"arm64": [("one", "sha256-a")], "amd64": [("one", "sha256-b")]}, True)
    changed = replace_block(text, block, audited)
    assert '"one" = "sha256-a";' in changed
    assert changed.count("scriptsAudited = true;") == 2
    assert changed.count("closureComplete = true;") == 2


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--patch-output", type=Path)
    parser.add_argument("--fetch", action="store_true", help="perform bounded index/APK downloads")
    parser.add_argument("--self-test", action="store_true")
    options = parser.parse_args()
    if options.self_test:
        self_test()
        return
    if options.output_dir is None:
        parser.error("--output-dir is required")
    text = SOURCE.read_text()
    packages, indexes = parse_source(text)
    total = sum(int(entry[3 if architecture == "amd64" else 4]) for architecture in ARCHES for entry in packages)
    if not options.fetch:
        print(json.dumps({"payloads": 68, "expected_bytes": total, "output_dir": str(options.output_dir)}))
        return
    if options.patch_output is None:
        parser.error("--patch-output is required with --fetch")
    if options.patch_output.resolve() == SOURCE:
        parser.error("--patch-output must not name the source manifest")
    options.output_dir.mkdir(parents=True, exist_ok=True)
    sizes = {}
    for key, (url, expected) in indexes.items():
        path = options.output_dir / f"APKINDEX-{key[0]}-{key[1]}.tar.gz"
        if not path.exists():
            fetch(url, path)
        if sha256(path) != expected:
            raise ValueError(f"signed index digest mismatch: {path}")
        sizes[key] = index_sizes(path)
    hashes = {architecture: [] for architecture in ARCHES}
    audit = []
    for architecture, apk_architecture in ARCHES.items():
        for name, version, repository, x86_size, arm_size in packages:
            expected = int(x86_size if architecture == "amd64" else arm_size)
            if sizes[(architecture, repository)].get((name, version)) != expected:
                raise ValueError(f"signed index changed {architecture} {name}-{version}")
            url = f"https://dl-cdn.alpinelinux.org/alpine/v3.24/{repository}/{apk_architecture}/{name}-{version}.apk"
            path = options.output_dir / architecture / f"{name}-{version}.apk"
            path.parent.mkdir(exist_ok=True)
            if not path.exists():
                fetch(url, path)
            if path.stat().st_size != expected:
                raise ValueError(f"APK size mismatch: {path}")
            controls = control_members(path)
            audit.append({"architecture": architecture, "package": name, "version": version, "controls": controls})
            hashes[architecture].append((name, nix_hash(path)))
    scripts_audited = not any(entry["controls"] for entry in audit)
    atomic_write(options.output_dir / "control-audit.json", json.dumps(audit, indent=2) + "\n")
    block, _ = render_hashes(hashes, scripts_audited)
    updated = replace_block(text, block, scripts_audited)
    patch = "".join(difflib.unified_diff(
        text.splitlines(True), updated.splitlines(True),
        fromfile="a/nix/developer-rootfs.nix", tofile="b/nix/developer-rootfs.nix",
    ))
    atomic_write(options.patch_output, patch)
    print(json.dumps({"payloads": 68, "scripts_audited": scripts_audited, "patch": str(options.patch_output)}))


if __name__ == "__main__":
    main()
