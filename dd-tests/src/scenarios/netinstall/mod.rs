//! Network package-install — the developer flow the base `distros` lane deliberately skips: actually
//! REFRESH apt metadata over the network, INSTALL a real package (htop), and RUN it. This is the exact
//! sequence a user drives after `ddcli ubuntu` (`apt-get update` → `apt-get install htop` → `htop`), and
//! the one that regressed in the field (apt `Ign:`/`Err:` lines, install warnings, TUI not rendering).
//! It is the regression net for that whole path. Owner: netinstall agent. Edit ONLY this folder.
//!
//! GATING: every case is `.long()` (Long class) so the fast, offline-deterministic Quick lane never
//! depends on outbound reachability — exactly like `networking/outbound-*` and `distros/*-apt-update`.
//! They run under `--long` (the full compatibility sweep), which assumes the network is up; a runner
//! with no egress simply omits `--long`. Verified against the Real docker oracle (orbstack, linux/arm64).
//!
//! ARCH: arm64-first (GA target). The x86 guest's gpgv `divq` SIGFPE during Release verification
//! (#232/#331) is RESOLVED — apt update/install + gpgv RSA verify complete byte-clean on dd amd — so the
//! amd targets are no longer xfailed and run on both arches. Determinism: markers are fixed strings the
//! harness greps out of the guest output.

use crate::scenario::{scen, sgroup, ScenGroup};

// Shell snippet: run `apt-get update`, then emit a single deterministic verdict token. Success = the
// InRelease/Release metadata lists actually landed on disk AND apt printed no `Ign:`/`Err:`/`W:`/`E:`
// line (the field-reported failure mode). `2>&1 | tee` keeps stderr diagnostics in the grepped stream.
const APT_UPDATE_CLEAN: &str = "\
set -o pipefail 2>/dev/null || true
apt-get update 2>&1 | tee /tmp/aptu.log
if grep -Eq '^(Ign|Err|W|E):' /tmp/aptu.log; then echo APT_UPDATE_DIRTY; \
elif ls /var/lib/apt/lists/ 2>/dev/null | grep -q Release; then echo APT_UPDATE_CLEAN; \
else echo APT_UPDATE_NOLISTS; fi";

pub fn group() -> ScenGroup {
    sgroup("netinstall", vec![
        // ---- apt-get update: must fetch cleanly, NO Ign:/Err: (the exact field regression) ----------
        scen("netinstall/ubuntu-apt-update-clean", "ubuntu:22.04")
            .exec(APT_UPDATE_CLEAN).has("APT_UPDATE_CLEAN")
            .long().timeout(180),
        scen("netinstall/debian-apt-update-clean", "debian:bookworm-slim")
            .exec(APT_UPDATE_CLEAN).has("APT_UPDATE_CLEAN")
            .long().timeout(180),

        // ---- apt-get install <pkg>: fetch + unpack + configure a real package end-to-end. htop pulls a
        // dep (libnl) too, so this exercises multi-package unpack/config + the overlay copy-up path that
        // regressed the `chmod .../archives/partial` (a lower-only dir metadata op). Marker: the binary is
        // present AND dpkg records it as fully installed.
        scen("netinstall/ubuntu-apt-install-htop", "ubuntu:22.04")
            .exec("export DEBIAN_FRONTEND=noninteractive
apt-get update >/dev/null 2>&1
apt-get install -y htop >/dev/null 2>&1
rc=$?
command -v htop >/dev/null && dpkg -s htop 2>/dev/null | grep -q '^Status: install ok installed' && [ $rc -eq 0 ] && echo HTOP_INSTALLED")
            .has("HTOP_INSTALLED").long().timeout(240),

        // ---- run htop: it must not just launch but RENDER a full, correct frame from live /proc data:
        // the CPU/Mem meter header, the `PID USER … Command` column header, REAL process rows (the shell +
        // the sleeps we spawn), and the F1..F10 function bar. ncurses reads $LINES/$COLUMNS, so a fixed size
        // gives htop a full screen to lay the process table into even without a client PTY; `echo q` quits it
        // cleanly (rc 0). Strip the control bytes, then assert every region of the drawn frame is present.
        scen("netinstall/ubuntu-htop-render", "ubuntu:22.04")
            .exec("export DEBIAN_FRONTEND=noninteractive TERM=xterm LINES=45 COLUMNS=200
apt-get update >/dev/null 2>&1
apt-get install -y htop >/dev/null 2>&1
# a couple of uniquely-named processes so a REAL process row is guaranteed in the table.
sleep 4242 & sleep 4243 &
echo q | htop -d 10 > /tmp/htop.frame 2>&1
rc=$?
f=$(tr -cd '[:print:]\\n' < /tmp/htop.frame)   # drop control bytes, keep the on-screen glyphs
echo \"$f\" | grep -q 'Tasks' \\
  && echo \"$f\" | grep -q 'Load average' \\
  && echo \"$f\" | grep -q 'PID' && echo \"$f\" | grep -q 'USER' && echo \"$f\" | grep -q 'Command' \\
  && echo \"$f\" | grep -q 'Help' && echo \"$f\" | grep -q 'Quit' \\
  && echo \"$f\" | grep -q 'sleep 4242' \\
  && [ $rc -eq 0 ] && echo HTOP_RENDER_OK")
            .has("HTOP_RENDER_OK").long().timeout(240),

        // ---- run top (procps): batch mode renders one full sample — the summary header (uptime/load,
        // Tasks:, %Cpu, Mem) AND the process table with a real row. `top -b -n1` is plain text (no PTY), so
        // this is a clean content assertion that the /proc-backed process accounting is correct end-to-end.
        scen("netinstall/ubuntu-top-render", "ubuntu:22.04")
            .exec("export DEBIAN_FRONTEND=noninteractive
apt-get update >/dev/null 2>&1
apt-get install -y procps >/dev/null 2>&1
sleep 5252 &
top -bn1 > /tmp/top.out 2>&1
rc=$?
grep -q 'Tasks:' /tmp/top.out \\
  && grep -Eq '%Cpu' /tmp/top.out \\
  && grep -q 'PID USER' /tmp/top.out \\
  && grep -q 'sleep' /tmp/top.out \\
  && [ $rc -eq 0 ] && echo TOP_RENDER_OK")
            .has("TOP_RENDER_OK").long().timeout(240),
    ])
}
