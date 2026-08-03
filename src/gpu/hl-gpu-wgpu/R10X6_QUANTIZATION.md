# R10X6 GPU quantization

`R10X6G10X6B10X6A10X6_UNORM_4PACK16` is physically backed by `RGBA16Unorm`. Exact Vulkan attachment
semantics require the stored destination to be reduced to ten bits after every draw: the next fixed-function
blend consumes that reduced destination. Quantizing fragment output is not equivalent because the blend
source remains full precision until the ROP.

The current correctness implementation ends each shadow-target draw, reads the logical plane, and writes it
back. It proves ordering and rounding but introduces a queue wait and host transfer per draw. Replace only
that projection boundary with this GPU sequence:

1. End the guest draw pass after one draw, as today.
2. Allocate or reuse an `RGBA16Unorm` scratch texture matching each R10X6 attachment. It needs
   `RENDER_ATTACHMENT | COPY_SRC`; the attachment already has `TEXTURE_BINDING | COPY_DST`.
3. Draw one fullscreen triangle into scratch. Read the source with `textureLoad` (never filtering) and emit
   `round(clamp(value, 0, 1) * 1023) / 1023`. The scratch target's 16-bit UNORM store is the exact expanded
   representation used by logical transfer conversion.
4. Copy scratch back to the attachment with a native same-format texture copy.
5. Begin the next split guest draw with `Load`, replaying the existing state prefix.

Cache the shader, pipeline, bind-group layout, and size-keyed scratch textures on the executor. Encode the
draw and copy in the same command encoder as the surrounding split passes; no queue submission, mapping, or
CPU wait belongs at this boundary. Multiple color attachments need one scratch/projection each, while depth
and stencil remain untouched. Write masks need no special handling because projection preserves every
already-stored ten-bit component.

The replacement must retain the adversarial two-draw blend and render-then-sample tests. Add a submission/
wait-count assertion proving projection adds zero host waits before removing the CPU path.

Multisampling remains excluded. WebGPU cannot write individual samples of a multisampled attachment from a
compute or ordinary render quantizer, and resolving would destroy per-sample state. Do not add 4x to the
image-format query until a distinct exact per-sample representation exists.
