#!/usr/bin/env python3
"""
Run the Chrome GPU pulse page and check the glyph atlas orientation in captured PNGs.

This catches the specific failure where Chrome UI was upright but Skia's single-channel
glyph atlas content rendered vertically inverted inside the page.
"""

from __future__ import annotations

import argparse
import os
import re
import struct
import subprocess
import sys
import zlib
from pathlib import Path
from typing import List, Sequence, Tuple


REPO = Path(__file__).resolve().parents[2]
RUNNER = REPO / "target-chrome-codex" / "run_chrome_gpu_bounded.sh"
PULSE_URL = (
    "data:text/html,%3Chtml%3E%3Ctitle%3EDD%20GPU%20Pulse%3C/title%3E"
    "%3Cstyle%3E@keyframes%20pulse%7B0%25%7Bbackground%3A%23109050%7D"
    "100%25%7Bbackground%3A%232060c0%7D%7Dhtml%2Cbody%7Bmargin%3A0%3Bheight%3A100%25%3B"
    "overflow%3Ahidden%7Dbody%7Bdisplay%3Agrid%3Bplace-items%3Acenter%3Bbackground%3A%2313161f%3B"
    "font%3A44px%20sans-serif%3Bcolor%3Awhite%7Ddiv%7Bwidth%3A420px%3Bheight%3A150px%3B"
    "display%3Agrid%3Bplace-items%3Acenter%3Banimation%3Apulse%201.2s%20linear%20infinite%20alternate%7D"
    "%3C/style%3E%3Cbody%3E%3Cdiv%3EDD%20GPU%20PULSE%3C/div%3E%3C/body%3E%3C/html%3E"
)


def png_rgba(path: Path) -> Tuple[int, int, bytes]:
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    pos = 8
    width = height = bit_depth = color_type = None
    chunks: List[bytes] = []
    while pos + 8 <= len(data):
        length = struct.unpack(">I", data[pos : pos + 4])[0]
        kind = data[pos + 4 : pos + 8]
        payload = data[pos + 8 : pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, bit_depth, color_type, comp, filt, interlace = struct.unpack(">IIBBBBB", payload)
            if comp != 0 or filt != 0 or interlace != 0:
                raise ValueError(f"{path} uses unsupported PNG options")
        elif kind == b"IDAT":
            chunks.append(payload)
        elif kind == b"IEND":
            break
    channels = {2: 3, 6: 4}.get(color_type)
    if width is None or height is None or bit_depth != 8 or channels is None:
        raise ValueError(f"{path} has unsupported PNG format")
    raw = zlib.decompress(b"".join(chunks))
    stride = width * channels
    prev = bytearray(stride)
    out = bytearray(width * height * 4)
    src = dst = 0
    for _ in range(height):
        filter_type = raw[src]
        src += 1
        row = bytearray(raw[src : src + stride])
        src += stride
        for i in range(stride):
            left = row[i - channels] if i >= channels else 0
            up = prev[i]
            up_left = prev[i - channels] if i >= channels else 0
            if filter_type == 1:
                row[i] = (row[i] + left) & 0xFF
            elif filter_type == 2:
                row[i] = (row[i] + up) & 0xFF
            elif filter_type == 3:
                row[i] = (row[i] + ((left + up) >> 1)) & 0xFF
            elif filter_type == 4:
                p = left + up - up_left
                pa = abs(p - left)
                pb = abs(p - up)
                pc = abs(p - up_left)
                pr = left if pa <= pb and pa <= pc else (up if pb <= pc else up_left)
                row[i] = (row[i] + pr) & 0xFF
            elif filter_type != 0:
                raise ValueError(f"{path} uses unsupported PNG filter {filter_type}")
        if channels == 4:
            out[dst : dst + width * 4] = row
            dst += width * 4
        else:
            for x in range(width):
                out[dst : dst + 4] = row[x * 3 : x * 3 + 3] + b"\xff"
                dst += 4
        prev = row
    return width, height, bytes(out)


def text_projection(path: Path) -> Tuple[int, int, int]:
    width, _height, rgba = png_rgba(path)
    # Fixed crop for the 512x384 Chrome pulse harness. The old inverted-glyph bug
    # has the same white pixel count, but the row projection is vertically reversed.
    rows: List[int] = []
    for y in range(275, 330):
        count = 0
        for x in range(80, 450):
            i = (y * width + x) * 4
            if rgba[i] > 210 and rgba[i + 1] > 210 and rgba[i + 2] > 210:
                count += 1
        rows.append(count)
    total = sum(rows)
    if total < 1500:
        raise AssertionError(f"{path}: not enough white glyph pixels in pulse crop ({total})")
    strongest_y = 275 + max(range(len(rows)), key=lambda i: rows[i])
    return total, strongest_y, max(rows)


def run_chrome(duration: int) -> Path:
    env = os.environ.copy()
    env.update(
        {
            "HL_APP_MODE": "0",
            "HL_APP_URL": PULSE_URL,
            "HL_EXTRA_FLAGS": (
                "--no-zygote --renderer-process-limit=1 --disable-site-isolation-trials "
                "--disable-backgrounding-occluded-windows --disable-renderer-backgrounding "
                "--disable-background-timer-throttling --ipc-connection-timeout=120 "
                "--enable-logging=stderr --v=0"
            ),
        }
    )
    proc = subprocess.run(
        ["bash", str(RUNNER), str(duration)],
        cwd=REPO,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        check=False,
    )
    sys.stdout.write(proc.stdout)
    match = re.search(r"^WORK=(.+)$", proc.stdout, re.MULTILINE)
    if proc.returncode != 0 or not match:
        raise SystemExit(f"Chrome pulse run failed with exit {proc.returncode}")
    return Path(match.group(1)) / "frames"


def select_frame(frames_dir: Path) -> Path:
    frames = sorted(frames_dir.glob("surface-6-*.png"))
    if len(frames) < 20:
        raise SystemExit(f"{frames_dir}: expected at least 20 captured frames, got {len(frames)}")
    return frames[min(10, len(frames) - 1)]


def main(argv: Sequence[str]) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--frames-dir", type=Path, help="analyze an existing Chrome frames directory")
    parser.add_argument("--duration", type=int, default=75)
    args = parser.parse_args(argv)

    frames_dir = args.frames_dir or run_chrome(args.duration)
    frame = select_frame(frames_dir)
    total, strongest_y, strongest = text_projection(frame)
    if strongest_y < 305:
        raise SystemExit(
            f"FAIL {frame}: glyph row projection looks vertically inverted "
            f"(strongest_y={strongest_y}, white_pixels={total}, strongest={strongest})"
        )
    print(f"PASS {frame}: upright glyphs strongest_y={strongest_y} white_pixels={total}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
