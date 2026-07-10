CC ?= gcc
CFLAGS ?= -O2 -Wall -Wextra
LDFLAGS ?=
EGL_LIBS ?= -lwayland-egl -lEGL -lGLESv2

RAW_PROBES := gui_xdg_ack gui_frame_nil gui_shm_frame gui_dmabuf_frame
EGL_PROBES := gui_egl_window_swap gui_egl_surfaceless gui_egl_textured_quad gui_egl_resize_lifecycle \
	gui_egl_fbo_texture_sample gui_egl_copytex_sample gui_egl_premul_blend gui_egl_scissored_quad \
	gui_egl_vertex_formats gui_egl_vao_state gui_egl_dynamic_buffer_reuse chrome_coverage_path \
	gui_egl_content_composite_layer gui_egl_content_composite_damage \
	gui_egl_state_churn_vao_elements gui_egl_state_churn_pipeline \
	gui_egl_swap_lifecycle_repeated gui_egl_swap_lifecycle_resize_recreate \
	gui_egl_damage_scissor_readback gui_egl_copy_texture_bridge \
	gui_egl_alpha_composite_readback gui_egl_viewport_scale_clip
EGL_ES3_PROBES := gui_egl_blit_framebuffer gui_egl_compositor_stress gui_egl_texture_upload_formats \
	gui_egl_texture_formats_fbo_readback
# Render probes built on the shared gui_egl_render_probe.h harness. Kept as their own group so every
# such source has a home in the matrix (enforced by the coverage gate in dd-tests/tests/gate_invariants.rs).
EGL_RENDER_PROBES := gui_egl_clear_only_swap gui_egl_draw_count_sentinel gui_egl_fifth_sampler \
	gui_egl_r8_alpha_orientation gui_egl_renderbuffer_msaa_resolve retained_frame_partial_load \
	single_channel_texture_probe
PROBES := $(RAW_PROBES) $(EGL_PROBES) $(EGL_ES3_PROBES) $(EGL_RENDER_PROBES)

.PHONY: all raw egl egl-es3 egl-render clean

all: $(PROBES)
raw: $(RAW_PROBES)
egl: $(EGL_PROBES)
egl-es3: $(EGL_ES3_PROBES)
egl-render: $(EGL_RENDER_PROBES)

$(RAW_PROBES): %: %.c gui_probe_wayland.h
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

gui_egl_window_swap gui_egl_surfaceless: %: %.c gui_probe_wayland.h
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS) $(EGL_LIBS)

gui_egl_textured_quad gui_egl_resize_lifecycle gui_egl_compositor_stress: %: %.c gui_probe_wayland.h gui_egl_probe.h
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS) $(EGL_LIBS)

gui_egl_fbo_texture_sample gui_egl_copytex_sample gui_egl_blit_framebuffer gui_egl_premul_blend gui_egl_scissored_quad gui_egl_vertex_formats gui_egl_vao_state gui_egl_dynamic_buffer_reuse gui_egl_texture_upload_formats chrome_coverage_path gui_egl_content_composite_layer gui_egl_content_composite_damage gui_egl_state_churn_vao_elements gui_egl_state_churn_pipeline gui_egl_swap_lifecycle_repeated gui_egl_swap_lifecycle_resize_recreate gui_egl_damage_scissor_readback gui_egl_copy_texture_bridge gui_egl_alpha_composite_readback gui_egl_viewport_scale_clip gui_egl_texture_formats_fbo_readback: %: %.c gui_probe_wayland.h gui_egl_probe.h gui_egl_render_probe.h
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS) $(EGL_LIBS)

$(EGL_RENDER_PROBES): %: %.c gui_probe_wayland.h gui_egl_probe.h gui_egl_render_probe.h
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS) $(EGL_LIBS)

clean:
	rm -f $(PROBES)
