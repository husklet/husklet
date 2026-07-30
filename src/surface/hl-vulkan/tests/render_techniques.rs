//! Multi-pass render-technique lowering tests (task #239).
//!
//! Task #222's `lowering.rs` proves every *single* `vkCmd*` lowers to the right `Enc`. THIS file proves the
//! multi-pass *compositions* real engines build out of those commands lower to a correct `Enc` stream: a
//! G-buffer MRT pass feeding a fullscreen lighting pass, a depth-only shadow pass feeding a shadow-sampling
//! pass, a ping-pong post-process chain, an MSAA resolve, and render-to-layer/mip. Each test mints the
//! `VkCmd` sequence in-process (driving the same `record::cmd_*` / `create::*` / `submit::*` seam the
//! shipping ICD marshals into) against a `RecordingSink`, and asserts the exact `Enc` IR — the
//! `BeginRenderPass` color/depth attachments, the pipeline/draw ops, and the cross-pass sampler bindings.
//!
//! MSAA is now threaded end to end (#240): `vkCreateImage` maps `VkImageCreateInfo::samples` →
//! `TextureDesc.sample_count` and `vkCreateGraphicsPipelines` maps
//! `VkPipelineMultisampleStateCreateInfo::rasterizationSamples` → `RenderPipelineDesc.sample_count`, so a
//! multisample `vkCmdResolveImage` lowers to the executor's real `Enc::ResolveTexture` (#179) — averaging
//! the samples — while a single-sample resolve stays a same-extent content-moving COPY. See
//! `msaa_resolve_pass_lowers_to_a_real_resolve` and `single_sample_resolve_still_lowers_to_a_copy`.
//!
//! Typed texture-view resources carry the selected format, aspect, dimension, mip range, and layer range.
//! Render attachments and descriptors name the view resource, so render-to-layer/mip and cube subviews keep
//! their Vulkan semantics through the neutral IR.

#[path = "render_techniques/deferred.rs"]
mod deferred;
#[path = "render_techniques/harness.rs"]
mod harness;
#[path = "render_techniques/post_process.rs"]
mod post_process;
#[path = "render_techniques/resolve.rs"]
mod resolve;
#[path = "render_techniques/shadow.rs"]
mod shadow;
#[path = "render_techniques/subresource.rs"]
mod subresource;
