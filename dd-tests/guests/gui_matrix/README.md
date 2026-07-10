# dd GUI Probe Matrix

Small Linux/aarch64 probes for isolating Chrome-like Wayland/EGL startup under `dd-jit`.

Build inside a GUI-enabled aarch64 workspace or image with the existing mounted GUI libs:

```sh
cd /path/to/dd-tests/guests/gui_matrix
make
```

If the GUI drop-in directory only has versioned sonames, pass explicit linker names:

```sh
make egl EGL_LIBS='-l:libwayland-egl.so.1 -l:libEGL.so.1 -l:libGLESv2.so.2'
```

Run with the same environment used by GUI apps:

```sh
export WAYLAND_DISPLAY=wayland-0
export XDG_RUNTIME_DIR=/run/user/0
export DD_GPU_EXEC=/run/user/0/dd-gpu-0
./run_gui_matrix.sh
```

Run an individual probe or subset by passing names to the runner:

```sh
./run_gui_matrix.sh gui_xdg_ack gui_egl_textured_quad
GUI_MATRIX_TIMEOUT=10 ./run_gui_matrix.sh gui_dmabuf_frame gui_egl_resize_lifecycle
DD_SHIM_ES3=1 ./run_gui_matrix.sh gui_egl_blit_framebuffer gui_egl_compositor_stress
DD_SHIM_ES3=1 ./run_gui_matrix.sh gui_egl_texture_upload_formats
./run_gui_matrix.sh chrome_coverage_path
./run_gui_matrix.sh gui_egl_vao_state
./run_gui_matrix.sh gui_egl_dynamic_buffer_reuse
./run_gui_matrix.sh gui_egl_content_composite_layer gui_egl_content_composite_damage
./run_gui_matrix.sh gui_egl_state_churn_vao_elements gui_egl_state_churn_pipeline
./run_gui_matrix.sh gui_egl_swap_lifecycle_repeated gui_egl_swap_lifecycle_resize_recreate
./run_gui_matrix.sh gui_egl_viewport_scale_clip
DD_SHIM_ES3=1 ./run_gui_matrix.sh gui_egl_texture_formats_fbo_readback
```

Probe intent:

- `gui_xdg_ack`: xdg toplevel initial nil commit, receive configure, `ack_configure`, second nil commit. This isolates the exact configure/ack step Chrome currently does not send.
- `gui_frame_nil`: requests `wl_surface.frame` on an unbuffered xdg surface after ack. dd-display should not fire a callback without an attached buffer; if it does, frame pacing semantics are too eager.
- `gui_shm_frame`: creates a `wl_shm` buffer, attach/damage/frame/commit, and waits for `wl_buffer.release` plus `wl_callback.done`. This proves the software first-frame path.
- `gui_dmabuf_frame`: allocates a dd synthetic render-node buffer through `/dev/dri/renderD128`, fills it from the guest CPU mapping, attaches it through `zwp_linux_dmabuf_v1`, and waits for release plus frame done. This proves the accelerated presentation path without EGL or Chrome.
- `gui_egl_window_swap`: performs the xdg handshake, creates a `wl_egl_window`, then drives `eglCreateWindowSurface`/`eglMakeCurrent`/`eglSwapBuffers` through the mounted EGL/GLES shim.
- `gui_egl_textured_quad`: performs the xdg/EGL window path, uploads an RGBA checker texture, creates VBO/EBO buffers, and draws an indexed textured quad across multiple swaps. This exercises shader translation, texture upload, buffer residency, indexed draw, and swap-to-dmabuf presentation before running Chrome.
- `gui_egl_resize_lifecycle`: creates an EGL window surface, swaps once, resizes the `wl_egl_window`, recreates the EGL surface at the resized dimensions, swaps again, then destroys and creates a second surface. This isolates resize bookkeeping and surface teardown/recreation from textured rendering.
- `gui_egl_fbo_texture_sample`: renders a solid quad into a texture-backed FBO, samples that texture into another FBO for pixel validation, then samples it into the default framebuffer and swaps. This isolates the draw-to-texture then sample-to-window path.
- `gui_egl_copytex_sample`: copies pixels with `glCopyTexSubImage2D` from a texture-backed FBO into one half of a texture and from the default framebuffer into the other half, validates both halves through an FBO readback, then samples the texture to the default framebuffer.
- `gui_egl_blit_framebuffer`: explicit ES3 probe that validates `glBlitFramebuffer` from FBO to FBO and default to FBO, and also exercises FBO to default before swapping. Use `DD_SHIM_ES3=1` for the dd GLES shim path.
- `gui_egl_premul_blend`: draws premultiplied-alpha red over black and white texture-backed FBOs with `glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA)`, validates the blended pixels, then presents both results through the default framebuffer.
- `gui_egl_scissored_quad`: draws a full quad through a central scissor box into a texture-backed FBO, validates center and outside pixels, then repeats the central scissor draw against the default framebuffer.
- `gui_egl_vertex_formats`: renders an interleaved VBO with `GL_FLOAT` positions, `GL_UNSIGNED_BYTE` normalized colors, and `GL_UNSIGNED_SHORT` integer `uvec2` data from `glVertexAttribIPointer`, then validates the FBO pixel. This catches component-count-only vertex descriptors and integer attributes mapped as floats.
- `gui_egl_vao_state`: binds two VAOs with incompatible vertex layouts, then switches back and validates the first VAO's packed color draw. This catches global-stub VAO state leaking across Chrome/ANGLE draw batches.
- `gui_egl_dynamic_buffer_reuse`: draws indexed quads from one VBO/EBO, mutates both buffers with `glBufferSubData` before the first swap, draws again, then poisons both buffers with `glBufferData`. This catches swap-time replay that resolves queued draws from the buffer object's latest contents instead of draw-time contents.
- `chrome_coverage_path`: renders into a 514x257 RGBA FBO, clears a pale background, draws a solid strip, uploads an RGBA coverage atlas, then draws indexed glyph-like triangles from interleaved `float2` positions, normalized `ubyte4` colors, normalized `ushort2` UVs, and normalized `ushort` coverage before sampling the offscreen texture into another FBO and the window.
- `gui_egl_content_composite_layer`: renders a Chrome-like offscreen RGBA content layer with subimage upload, viewport/scissor changes, indexed fullscreen composite, and premultiplied blending before validating default-framebuffer pixels.
- `gui_egl_content_composite_damage`: performs sequential texture subimage damage updates, scissored compositor draws, retained first-batch output checks, and final fullscreen default-framebuffer composition.
- `gui_egl_state_churn_vao_elements`: switches VAOs with separate element array buffers, byte-offset `glDrawElements`, rebinding `GL_ARRAY_BUFFER` after attribute setup, disabled attributes, and normalized `ubyte4` colors.
- `gui_egl_state_churn_pipeline`: alternates viewport, scissor, blend, and draw state across compositor-like batches to catch stale state restoration.
- `gui_egl_viewport_scale_clip`: samples a 4-color content texture into an offset, non-square default-framebuffer viewport while a smaller scissor clips it; validates scaled quadrant pixels and untouched gutters to catch viewport scaling and content-clipping regressions.
- `gui_egl_swap_lifecycle_repeated`: performs repeated swaps with alternating `glFlush`, `glFinish`, optional fence sync, scissored redraws, and final readback checks.
- `gui_egl_swap_lifecycle_resize_recreate`: resizes a `wl_egl_window`, recreates EGL surfaces, recreates the native window, and validates final pixels after each lifecycle transition.
- `gui_egl_compositor_stress`: explicit-only probe that requests an ES3 context, uploads two premultiplied RGBA textures plus a `GL_R8`/`GL_RED` glyph atlas, then performs three scissored blended draws across two programs and three texture bindings in a single swap. This is the Chrome-like compositor stress probe.
- `gui_egl_texture_upload_formats`: explicit ES3 probe that uploads `GL_BGRA_EXT`, `GL_RED`/`GL_R8`, and `GL_LUMINANCE` textures, including `GL_UNPACK_ROW_LENGTH`, `GL_UNPACK_SKIP_ROWS`, `GL_UNPACK_SKIP_PIXELS`, and `GL_UNPACK_ALIGNMENT`, samples each texture into an RGBA FBO, and validates readback pixels.
- `gui_egl_texture_formats_fbo_readback`: validates NPOT RGBA uploads, partial `glTexSubImage2D` updates with pixel-store row/skip state, tightly packed RGB upload, optional BGRA extension upload, premultiplied blending, FBO completeness, and readback.
- `gui_egl_surfaceless`: creates an EGL context without a window surface, makes it current with `EGL_NO_SURFACE`, probes GL strings, then performs the xdg handshake. This separates ANGLE-style bootstrap context creation from window-surface creation.

Suggested failure triage order:

1. `gui_xdg_ack`, `gui_frame_nil`, `gui_shm_frame`: Wayland protocol and software buffer basics.
2. `gui_dmabuf_frame`: dd render-node allocation plus linux-dmabuf presentation.
3. `gui_egl_surfaceless`: EGL/GLES bootstrap without a window.
4. `gui_egl_window_swap`: EGL window clear and swap path.
5. `gui_egl_textured_quad`: Chrome-adjacent GLES2 texture, VBO/EBO, and indexed draw path.
6. `gui_egl_fbo_texture_sample`, `gui_egl_copytex_sample`, `gui_egl_blit_framebuffer`: offscreen texture, copy, and blit transfer paths that can blank Chrome compositor content.
7. `gui_egl_premul_blend`, `gui_egl_scissored_quad`, `gui_egl_vertex_formats`, `gui_egl_vao_state`, `gui_egl_dynamic_buffer_reuse`, `chrome_coverage_path`, `gui_egl_content_composite_layer`, `gui_egl_content_composite_damage`, `gui_egl_state_churn_vao_elements`, `gui_egl_state_churn_pipeline`, `gui_egl_viewport_scale_clip`: compositor-state correctness for premultiplied alpha, clipped central content, packed/integer vertex attributes, VAO/EBO state isolation, dynamic buffer reuse, Chrome-like coverage atlas draws, damage updates, state churn, and viewport-scale clipping.
8. `gui_egl_resize_lifecycle`, `gui_egl_swap_lifecycle_repeated`, `gui_egl_swap_lifecycle_resize_recreate`: resize, repeated swap, fence/flush, and EGL surface lifecycle behavior.
9. `gui_egl_compositor_stress`, `gui_egl_texture_upload_formats`, `gui_egl_texture_formats_fbo_readback`: Chrome-like multi-draw compositor and texture upload format probes. Use `DD_SHIM_ES3=1` for the dd GLES shim path.
