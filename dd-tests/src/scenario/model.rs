use super::*;

/// Which container engine the scenario runs against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Backend {
    Real,
    Dd,
}

/// A real-software target. Linux targets map to a docker `--platform`; mac is the lighter native path.
/// Linux parity = a scenario runs on BOTH ArmLinux and AmdLinux unless narrowed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    ArmLinux,
    AmdLinux,
    ArmMac,
}

impl Target {
    pub const LINUX: [Target; 2] = [Target::ArmLinux, Target::AmdLinux];
    pub fn platform(self) -> Option<&'static str> {
        match self {
            Target::ArmLinux => Some("linux/arm64"),
            Target::AmdLinux => Some("linux/amd64"),
            Target::ArmMac => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Target::ArmLinux => "arm-linux",
            Target::AmdLinux => "amd-linux",
            Target::ArmMac => "arm-mac",
        }
    }
}

/// Resource class. `Quick` = cache-only/offline-skip, for dev. `Long` = pulls + heavy workloads.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Quick,
    Long,
}

/// How the workload is launched in the container.
pub enum Step {
    /// `docker run --rm [--platform p] <image> <argv…>` — one-shot.
    Run(Vec<String>),
    /// Developer-at-a-shell path: start a detached idle container and `docker exec -i <c> /bin/sh -c
    /// <script>` into it (the `exec -it /bin/bash` workflow).
    ExecIt(String),
    /// Host-orchestrated recipe: the body is bash run on the docker HOST (not inside a guest), free to
    /// drive `docker run -v`, `docker network create`, multiple containers, `docker exec`, `docker stop`,
    /// `-e`/`-w` — the behaviours single-container Run/ExecIt can't express (volumes, user-defined
    /// networks, cross-container reachability, signals). The harness injects these vars and ALWAYS cleans
    /// up afterwards: `$IMG` (the image), `$PLAT` (the `--platform` words, unquoted), `$WORK` (a private
    /// host scratch dir — bind-mount it for volume tests), `$C` (unique container-name PREFIX — name every
    /// container `$C…` so it gets reaped), `$NET` (a unique network name — `docker network rm` is automatic).
    Host(String),
}

/// One expectation against captured stdout+stderr / exit code.
pub enum Check {
    Has(String),
    Eq(String),
    Rc(i32),
}

/// One real-software test.
pub struct Scenario {
    pub id: &'static str, // "category/name" — stable; xfail + count key on it
    pub image: &'static str,
    pub step: Step,
    pub targets: Vec<Target>,
    pub class: Class,
    pub checks: Vec<Check>,
    pub xfail: Vec<Target>, // targets where dd is known-broken (still must pass on Real)
    pub timeout: u64,
    pub tty: bool, // allocate a container PTY (docker -t) → isatty/termios/job-control path
}

pub struct ScenGroup {
    pub name: &'static str,
    pub scenarios: Vec<Scenario>,
}
pub fn sgroup(name: &'static str, scenarios: Vec<Scenario>) -> ScenGroup {
    ScenGroup { name, scenarios }
}

/// Start a scenario: BOTH Linux arches, Quick class by default.
pub fn scen(id: &'static str, image: &'static str) -> Scenario {
    Scenario {
        id,
        image,
        step: Step::Run(vec![]),
        targets: Target::LINUX.to_vec(),
        class: Class::Quick,
        checks: vec![],
        xfail: vec![],
        timeout: 120,
        tty: false,
    }
}

impl Scenario {
    pub fn run(mut self, argv: &[&str]) -> Self {
        self.step = Step::Run(argv.iter().map(|s| s.to_string()).collect());
        self
    }
    pub fn exec(mut self, script: &str) -> Self {
        self.step = Step::ExecIt(script.to_string());
        self
    }
    /// Host-orchestrated recipe (see [`Step::Host`]) — for volumes / networks / multi-container / signals.
    pub fn host(mut self, body: &str) -> Self {
        self.step = Step::Host(body.to_string());
        self
    }
    pub fn has(mut self, s: &str) -> Self {
        self.checks.push(Check::Has(s.into()));
        self
    }
    pub fn eq_(mut self, s: &str) -> Self {
        self.checks.push(Check::Eq(s.into()));
        self
    }
    pub fn rc(mut self, c: i32) -> Self {
        self.checks.push(Check::Rc(c));
        self
    }
    pub fn long(mut self) -> Self {
        self.class = Class::Long;
        self
    }
    pub fn only(mut self, t: &[Target]) -> Self {
        self.targets = t.to_vec();
        self
    }
    pub fn plus_mac(mut self) -> Self {
        if !self.targets.contains(&Target::ArmMac) {
            self.targets.push(Target::ArmMac);
        }
        self
    }
    pub fn xfail(mut self, t: &[Target]) -> Self {
        self.xfail = t.to_vec();
        self
    }
    pub fn timeout(mut self, s: u64) -> Self {
        self.timeout = s;
        self
    }
    /// Allocate a container PTY (`docker run/exec -t`) so the guest sees an interactive TERMINAL —
    /// isatty()==1, termios tcgetattr/tcsetattr, ioctl(TIOCGWINSZ), and job-control signals. The
    /// developer `docker exec -it /bin/bash` path; exercises the JIT's pty/termios/ioctl syscalls.
    pub fn tty(mut self) -> Self {
        self.tty = true;
        self
    }
}
