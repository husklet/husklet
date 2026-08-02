# Husklet ownership

This directory is the crates.io `wgpu-core` 24.0.5 package source copied from
Cargo's registry cache. The upstream package version is retained in
`Cargo.toml`; `.cargo_vcs_info.json` records upstream revision
`99e6524b2b03cc35733f199bb5cb28787e8d42de`. The crates.io archive checksum is
`7f0aa306497a238d169b9dc70659105b4a096859a34894544ca81719242e1499`.

Husklet changes only the Naga shader-binding validation boundary. Its vendored
Naga preserves SPIR-V `DimBuffer` as `ImageDimension::Buffer`; wgpu has no
native texture-buffer binding, so `hl-gpu-wgpu` must lower that resource to a
typed storage buffer before creating a wgpu shader module. Reaching wgpu-core
with the dimension intact returns `UnloweredTexelBuffer` instead of silently
substituting a texture dimension.
