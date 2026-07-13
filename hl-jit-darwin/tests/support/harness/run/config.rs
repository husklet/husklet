use super::*;

/// Assemble the `SpawnConfig` for a case+engine EXCEPT `argv` (callers set `argv`: `run()` may use a
/// jailed in-rootfs copy path, perf uses the plain guest path). `rootfs_str` is "" when the case has no
/// rootfs. Needs no `Ctx`. Preserves the exact overlay guard, the untrusted push, and the env loop.
pub(crate) fn build_cfg(c: &Case, e: Engine, rootfs_str: &str) -> hl_jit::SpawnConfig {
    let mut cfg = hl_jit::SpawnConfig::new(String::new(), rootfs_str.to_string());
    cfg.lowers = c.lowers.clone();
    // .overlay(): inject the rootfs as its own lower so g_nlower>0 turns on the overlay open/lseek path
    // (linux engines only; darwin has no overlayfs). Reproduces overlay-only bugs like in the matrix.
    if c.overlay && !rootfs_str.is_empty() && e != Engine::DarwinAarch64 {
        cfg.lowers.push(rootfs_str.to_string());
    }
    cfg.mem_max = c.mem_max;
    cfg.cpus = c.cpus;
    cfg.read_only = c.read_only;
    cfg.ulimits = c.ulimits.clone();
    // Untrusted-guest SENTRY split: bake DDJIT_UNTRUSTED=1 into the engine's launch env (via SpawnConfig's
    // `env`, which serializes into the `exec env …` prefix of the launch script — so it survives the `mac`
    // bridge that drops ambient env). `.sandbox()` additionally arms DDJIT_SANDBOX=1 for the PUBLIC sandbox
    // mode (Seatbelt worker confinement on macOS) — the exact combo the public `.sandbox(true)` builder emits.
    if c.untrusted || c.sandbox {
        cfg.env.push(("DDJIT_UNTRUSTED".into(), "1".into()));
    }
    if c.sandbox {
        cfg.env.push(("DDJIT_SANDBOX".into(), "1".into()));
    }
    for (k, v) in &c.env {
        cfg.env.push((k.clone(), v.clone()));
    }
    cfg
}

/// Build the guest `argv` from `argv0` (an `InRootfs` case runs `c.args` verbatim; otherwise `argv0`
/// is prepended). `run()` passes its jailed in-rootfs copy path, perf passes the plain guest path.
pub(crate) fn guest_argv(c: &Case, argv0: String) -> Vec<String> {
    match &c.bin {
        Bin::InRootfs => c.args.clone(),
        _ => std::iter::once(argv0)
            .chain(c.args.iter().cloned())
            .collect(),
    }
}
