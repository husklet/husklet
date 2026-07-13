//! gui — EGL/Wayland GL-shim probes. These are fixture-style because they link dynamically against
//! the GUI/EGL shim stack built by `dd-tests/guests/gui_matrix/Makefile`.

use dd_tests::{fixture, group, Engine, Group};

pub fn groups() -> Vec<Group> {
    vec![group(
        "ext_gui",
        vec![
            fixture(
                "egl-texture-upload-formats",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_texture_upload_formats"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("DD_SHIM_ES3", "1")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("bgra_swizzle=1 red=1 luminance=1 row_length=5 skip=3,2 alignment=4 sampled_fbo=1")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "chrome-coverage-path",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/chrome_coverage_path"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("coverage_path=1 offscreen_rgba=514x257 atlas=8x8 indexed_tris=4")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-vao-state",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_vao_state"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("DD_SHIM_ES3", "1")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("vao_restore=1")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-dynamic-buffer-reuse",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_dynamic_buffer_reuse"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("reused_vbo=1 reused_ebo=1 draws_before_swap=2 orphan_poison=1")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-content-composite-layer",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_content_composite_layer"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("offscreen_rgba=64x64 subimage=1 indexed=1")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-content-composite-damage",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_content_composite_damage"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("subimage_updates=2 indexed_batches=2 scissor_damage=2")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-state-churn-vao-elements",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_state_churn_vao_elements"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("DD_SHIM_ES3", "1")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("vao_switch=1 ebo_per_vao=1 draw_offset=6")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-state-churn-pipeline",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_state_churn_pipeline"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("viewport=1 scissor=1 blend=1 state_restore=1")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-swap-lifecycle-repeated",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_swap_lifecycle_repeated"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("swaps=10 flush=1 finish=1 optional_fence=1")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-swap-lifecycle-resize-recreate",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_swap_lifecycle_resize_recreate"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("resizes=2 attached=240x150,96x176 surface_recreates=3")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-texture-formats-fbo-readback",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_texture_formats_fbo_readback"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("DD_SHIM_ES3", "1")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("rgba_npot=5x3 subimage=2x1 rgb_alignment=1 row_length=1")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-damage-scissor-readback",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_damage_scissor_readback"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("partial_updates=3 scissor_damage=3 retained_readback=1 default_marker=1 layer=96x64")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-copy-texture-bridge",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_copy_texture_bridge"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("gles2_copytex=1 fbo_to_texture=2 default_to_texture=1 atlas_readback=3 sampled_default=1")
            .xfail(&[Engine::LinuxAarch64]),
            fixture(
                "egl-alpha-composite-readback",
                &[(
                    Engine::LinuxAarch64,
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/guests/gui_matrix/gui_egl_alpha_composite_readback"
                    ),
                )],
            )
            .rootfs("elixir")
            .env("WAYLAND_DISPLAY", "wayland-0")
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env("DD_GPU_EXEC", "/run/user/0/dd-gpu-0")
            .has("premul_alpha=3 readback=3 visible_columns=3 blend_func=one_one_minus_src_alpha composite_to_default=1")
            .xfail(&[Engine::LinuxAarch64]),
        ],
    )]
}
