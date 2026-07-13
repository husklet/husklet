use super::arch::{store_arch, target_arch};
use super::daemon::{run_script, sh_quote};
use super::*;
use std::path::PathBuf;
use std::time::Instant;

mod cache;
use cache::{cell_key, ensure_cache, oracle_cache};

/// Verdict for one (scenario, target).
pub enum Status {
    Pass,
    Fail(String),
    Skip(String),
    Xfail(String),
    Xpass,
}

/// Runner config.
pub struct Cfg {
    pub backend: Backend,
    pub class: Class,
    pub targets: Vec<Target>,
    pub category: Option<String>,
    pub offline: bool,
    pub count: bool,
    pub images: PathBuf,
    pub daemon_bin: PathBuf,
}
impl Cfg {
    pub fn includes(&self, s: &Scenario) -> bool {
        self.class == Class::Long || s.class == Class::Quick
    }
}

/// Make a string safe to embed in a filename (image refs carry `/`, `:`, `@`).
fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}
/// Phase timing is opt-in (`DD_SCEN_PROFILE=1`) so it never pollutes normal output.
fn profiling() -> bool {
    std::env::var_os("DD_SCEN_PROFILE").is_some()
}

// ---- generated per-operation scripts -------------------------------------------------------------
fn header(host: Option<&str>) -> String {
    match host {
        Some(h) => format!("#!/bin/bash\nexport DOCKER_HOST={}\n", sh_quote(h)),
        None => "#!/bin/bash\n".into(),
    }
}
fn ensure(d: &Daemon, cfg: &Cfg, image: &str) -> bool {
    // Image availability is invariant for the whole run → memoize per image. This is the single biggest
    // bridge-call saver: a category that reuses one image across N scenarios used to inspect it N times.
    if let Some(&ok) = ensure_cache().lock().unwrap().get(image) {
        return ok;
    }
    let dir = d.run_dir();
    // Per-image filename so concurrent first-touches of DIFFERENT images don't clobber one script.
    let f = dir.join(format!("ensure-{}.sh", slug(image)));
    let body = format!("{}docker image inspect {img} >/dev/null 2>&1 && exit 0\n{}\ndocker pull {img} >/dev/null 2>&1\n",
        header(d.docker_host()), if cfg.offline { "exit 1" } else { "" }, img = sh_quote(image));
    if std::fs::write(&f, body).is_err() {
        return false;
    }
    // Run unlocked (a pull can take minutes); two threads racing the SAME image just inspect twice — the
    // op is idempotent. Record the verdict so every later cell using this image is a pure cache hit.
    let ok = run_script(&f, d.bridged, if cfg.offline { 20 } else { 180 })
        .status
        .success();
    ensure_cache().lock().unwrap().insert(image.to_string(), ok);
    ok
}

fn drive(d: &Daemon, s: &Scenario, t: Target, cfg: &Cfg) -> (String, i32) {
    // Oracle output is deterministic ground truth → serve repeats of an identical cell from cache.
    let key = (cfg.backend == Backend::Real).then(|| cell_key(s, t));
    if let Some(k) = &key {
        if let Some(v) = oracle_cache().lock().unwrap().get(k) {
            return v.clone();
        }
    }
    let dir = d.run_dir();
    // Per-(scenario,target) filename so the two arches of one scenario can run concurrently without
    // racing on a shared op script.
    let f = dir.join(format!("op-{}-{}.sh", s.id.replace('/', "_"), t.label()));
    let plat = t
        .platform()
        .map(|p| format!("--platform {p} "))
        .unwrap_or_default();
    let tt = if s.tty { "-t " } else { "" }; // run: allocate a container PTY
    let xt = if s.tty { "-t" } else { "-i" }; // exec: PTY vs plain stdin (no client TTY needed)
    let sh = if s.image.contains("alpine") || s.image.starts_with("busybox") {
        "/bin/sh"
    } else {
        "/bin/bash"
    };
    let body = match &s.step {
        Step::Run(argv) => {
            let a = argv
                .iter()
                .map(|x| sh_quote(x))
                .collect::<Vec<_>>()
                .join(" ");
            format!(
                "{}docker run --rm {tt}{plat}{img} {a}\n",
                header(d.docker_host()),
                img = sh_quote(s.image)
            )
        }
        Step::ExecIt(script) => {
            // idle container we exec into (mirrors `docker exec -it`); fall back to one-shot run for
            // images with no keep-alive shell (distroless). Embed the user script verbatim via a
            // quoted heredoc so arbitrary quotes/heredocs inside it survive.
            let name = format!(
                "ddx-{}-{}-{}",
                std::process::id(),
                s.id.replace('/', "-"),
                t.label()
            );
            // Speed: the container name is unique (pid·id·target), so the old pre-run `docker rm -f` was
            // a guaranteed no-op bridge round-trip — dropped (a stale same-name container, only possible
            // after a hard-killed run with pid reuse, just falls through to the one-shot `run --rm`). And
            // `-d --rm` + a `docker kill` trap keeps teardown OFF the critical path: `kill` only signals
            // PID 1 and returns, while the daemon reaps + removes asynchronously (no leak, no wait — this
            // matters for loaded servers like postgres that otherwise SIGKILL-stop synchronously). The
            // trap fires on EXIT *and* INT/TERM so the harness's outer `timeout` can't orphan the idle
            // container; the fallback one-shot is `--name $N` too so the same trap reaps it.
            format!(
"{hdr}N={name}
trap 'docker kill $N >/dev/null 2>&1' EXIT INT TERM
if docker run -d --rm --name $N {plat}{img} {sh} -c 'while true; do sleep 3600; done' >/dev/null 2>&1; then
  docker exec {xt} $N {sh} -c \"$(cat <<'DDEOF'
{script}
DDEOF
)\"
  rc=$?
else
  docker run --rm --name $N {tt}{plat}{img} {sh} -c \"$(cat <<'DDEOF'
{script}
DDEOF
)\"
  rc=$?
fi
exit $rc
", hdr = header(d.docker_host()), name = sh_quote(&name), plat = plat, tt = tt, xt = xt, img = sh_quote(s.image), sh = sh, script = script)
        }
        Step::Host(body) => {
            // Host-orchestrated: inject the unique resources + a guaranteed teardown trap, then run the
            // author's recipe verbatim. $C is a name PREFIX (a `^$C` filter reaps every `$C…` container),
            // $NET a unique network, $WORK a private host scratch dir under the shared run dir (so a
            // `-v $WORK:/x` bind mount is visible to the docker host). $PLAT is unquoted → word-splits.
            let base = format!(
                "ddh-{}-{}-{}",
                std::process::id(),
                s.id.replace('/', "-"),
                t.label()
            );
            let net = format!(
                "ddnet-{}-{}-{}",
                std::process::id(),
                s.id.replace('/', "-"),
                t.label()
            );
            let work = dir.join(format!("work-{}-{}", s.id.replace('/', "_"), t.label()));
            let _ = std::fs::create_dir_all(&work);
            format!(
"{hdr}IMG={img}
PLAT={plat}
C={c}
NET={net}
WORK={work}
mkdir -p \"$WORK\"
cleanup() {{ docker rm -f $(docker ps -aq -f name=\"^${{C}}\") >/dev/null 2>&1; docker network rm \"$NET\" >/dev/null 2>&1; rm -rf \"$WORK\"; }}
trap cleanup EXIT INT TERM
{body}
", hdr = header(d.docker_host()), img = sh_quote(s.image), plat = sh_quote(plat.trim()),
   c = sh_quote(&base), net = sh_quote(&net), work = sh_quote(&work.to_string_lossy()), body = body)
        }
    };
    if std::fs::write(&f, body).is_err() {
        return ("(failed to write op script)".into(), -1);
    }
    let o = run_script(&f, d.bridged, s.timeout + 10);
    let mut out = String::from_utf8_lossy(&o.stdout).into_owned();
    out.push_str(&String::from_utf8_lossy(&o.stderr));
    let res = (out, o.status.code().unwrap_or(-1));
    if let Some(k) = key {
        oracle_cache().lock().unwrap().insert(k, res.clone());
    }
    res
}

/// Run one scenario on one target and classify (xfail-aware). xfail only applies to the Dd backend —
/// on Real, a fail is always a real (test) failure.
pub fn run_one(d: &Daemon, s: &Scenario, t: Target, cfg: &Cfg) -> Status {
    if !s.targets.contains(&t) {
        return Status::Skip("n/a for target".into());
    }
    let prof = profiling();
    let t0 = Instant::now();
    let cached = ensure_cache().lock().unwrap().contains_key(s.image);
    if !ensure(d, cfg, s.image) {
        return Status::Skip(format!("image {} unavailable", s.image));
    }
    // Single-arch store: don't manufacture a false gap by serving a wrong-arch rootfs under the Dd
    // daemon — skip the cell whose arch the store provably can't serve (Real pulls the right arch).
    if cfg.backend == Backend::Dd {
        if let Some(a) = store_arch(cfg, s.image) {
            if a != target_arch(t) {
                return Status::Skip(format!(
                    "store holds {a} only (no {} rootfs for {})",
                    target_arch(t),
                    s.image
                ));
            }
        }
    }
    let ensure_ms = t0.elapsed().as_millis();
    let xfail = cfg.backend == Backend::Dd && s.xfail.contains(&t);
    let t1 = Instant::now();
    let (out, code) = drive(d, s, t, cfg);
    if prof {
        eprintln!(
            "[prof] id={} tgt={} ensure_ms={} ensure_cached={} drive_ms={} total_ms={}",
            s.id,
            t.label(),
            ensure_ms,
            cached as u8,
            t1.elapsed().as_millis(),
            t0.elapsed().as_millis()
        );
    }
    let bad: Option<String> = if code == 124 {
        Some(format!("timeout >{}s", s.timeout))
    } else {
        s.checks.iter().find_map(|chk| match chk {
            Check::Has(sub) => {
                (!out.contains(sub.as_str())).then(|| format!("lacks [{sub}] in [{}]", clip(&out)))
            }
            Check::Eq(want) => (out.trim() != want.as_str())
                .then(|| format!("got [{}] want [{want}]", clip(out.trim()))),
            Check::Rc(want) => (code != *want).then(|| format!("rc {code} != {want}")),
        })
    };
    match (bad, xfail) {
        (None, true) => Status::Xpass,
        (None, false) => Status::Pass,
        (Some(m), xf) => {
            // Self-explaining failure: scrape the container output + daemon-log tail for engine signals
            // (missing syscall / UNIMPL opcode / crash / loader) and attach a one-line diagnosis.
            let diag = dd_tests::diag::diagnose(m, code, &out, &d.log_tail(25));
            if xf {
                Status::Xfail(diag.summary())
            } else {
                Status::Fail(diag.summary())
            }
        }
    }
}

fn clip(s: &str) -> String {
    s.replace('\n', "|").chars().take(180).collect()
}
