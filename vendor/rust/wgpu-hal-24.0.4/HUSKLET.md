# Husklet patch

This is the exact crates.io `wgpu-hal 24.0.4` source with one Metal resource-
layout correction.

Upstream assigns physical Metal vertex buffers downward from
`max_vertex_buffers`, which is the WebGPU logical vertex-buffer count (16).
Metal buffer arguments share one 31-slot namespace with uniform/storage
buffers and Naga's sizes buffer. A pipeline with transform-feedback storage
buffers and a wide vertex layout can therefore assign both resources to the
same physical indices even though the combined resource count fits Metal.

The patch assigns vertex buffers downward from `max_buffers_per_stage` (31),
uses the same mapping when binding them, and checks the combined layout against
that physical limit. WebGPU's logical count remains enforced by wgpu-core.
