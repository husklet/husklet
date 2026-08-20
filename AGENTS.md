# Husklet instructions

These rules define the durable architecture, safety, coding, testing, and delivery
standards for Husklet and its integrated C execution engine. Apply them to every new package and improve
nearby code when doing so preserves behavior and remains within scope.

The retired C engine in `../engine` is a read-only behavioral and performance oracle
during migration. Husklet is the active repository and owns the integrated C engine, Rust control plane, containers,
workspaces, terminal, and desktop application. Do not add GPU, graphics translation,
surface, compositor, CUDA, OpenGL, Vulkan, or Wayland implementation back into this
repository. Never edit `../engine` while studying it.

Sections 13–340 are process rules, and almost every one exists because it cost
real work. If you are about to **measure** anything, the ones that have voided
results are "Balance the arm order", "A control that merely seems unaffected is
not a control", "`bench --results` is a resumable ledger", "Identical source does
not mean an identical binary", "A C change reaching the binary is a separate
claim, and it has its own check", and "Reading a profile". If you are about to
**commit**, they are "What green means" and its four subsections. Everything from
"Mission" onward is durable architecture and changes rarely.

**A counter governed by the policy cannot decide the policy.** A lane proposed
replacing a flip budget with a threshold on `direct_declined`, and the counters
appeared to confirm it: the plain guest finished at 1 declined site at every
budget, sqlite at 4 to 26. But `malloc` **alone** declines 17 sites on the
plain guest. It reads 1 in the whole sequence only because the budget's hold
has already fired and suppressed direct mode — and suppressed direct mode
cannot guard-fault, which is the sole entrance to the set. The evidence for
the replacement was produced by the thing being replaced.

Implementing it settled what the counter could not: `admitted` fell 255,303 to
1,228, the sqlite phase regressed 1.74x, and both guests latched inside
`malloc` and lost direct authority for the rest of the process. Before keying
a decision on a counter, ask what writes to it and whether the current policy
gates that write.

## Reading code: CodeGraph first

This repository is indexed by CodeGraph (`.codegraph/` at the root). Reach for it
**before** grep, find, or opening files, both to answer a question and before
editing a symbol. One `codegraph_explore` call returns the verbatim,
line-numbered source of the matching symbols grouped by file — safe to edit from,
and equivalent to having read them — plus the call path among them and a blast
radius naming every caller and the tests that cover each symbol. Prefer the MCP
tool `codegraph_explore`; `codegraph explore "<names>"` in a shell prints the same
output when the tool is unavailable.

The blast radius reports **`no covering tests found`** per symbol. Treat that as a
first-class signal: it names the places where a green suite proves nothing, which
is where this codebase has repeatedly hidden defects.

Two failure modes, both observed:

- **Query precise symbol names, two to four at a time. Never bare filenames.**
  A filename matches repo-wide — `pool.rs` pulls in unrelated container and
  launcher files and spends the whole budget on them.
- **Output is budget-truncated and truncation is silent.** A broad query can drop
  the symbol you asked about and leave it visible only in the blast radius. If the
  source you needed is not in the reply, ask again with fewer, narrower names
  rather than assuming it does not exist.

Do not re-open a file whose source CodeGraph already returned.

`no covering tests found` is a lead, not a verdict. It has misfired repeatedly,
including on symbols whose removal reddens both unit and integration tests.
Confirm a gap by mutating the symbol and watching the suite, which is the standard
this repository requires for a coverage claim anyway.

## What "green" means

The corpus is not the gate. The `nix flake check` verification derivation runs
`cargo test --workspace --all-targets`, which covers the Rust library tests and
the native-package integration tests; the corpus reaches neither.
Eleven stale assertions once survived because every lane ran the corpus and a
targeted `cargo test -p <pkg>` and nobody ran the workspace.

Before committing, run `cargo test --workspace --lib --bins` and
`cargo test -p hl-native --all-targets` — about a minute, and it catches that
whole class. Run them in debug or inside `nix flake check`: under `--release`,
`hl-log`'s verbose tests compile out and the daemon tests need
`HL_ALPINE_ARCHIVE`, so both report spurious failures.

### Two green branches can merge to a red tip

`cargo test --workspace --lib --bins` on each branch proves nothing about the merge.
One lane turned `Signal` from a seven-variant enum into a `1..=64` value type
while another added a test spelling `Signal::Kill`; both were green alone and
the tip did not compile. Git reported no conflict, because there was none —
the conflict was semantic.

So run the gate **after** merging, not only before, and run it on the tip you
are about to push. A `cargo check --workspace --all-targets` is a minute and
catches this whole class.

`--bins` is not optional. `cargo test --workspace --lib` alone runs **no** test
in a crate that has no library target, and `testing` is bin-only: `cargo test -p
testing --lib` answers `no library targets found in package testing`, silently
in a workspace-wide run. Every assertion in `testing`'s benchmark and syscall
audit gates was invisible to the command this file told everyone to run.

`--all-targets` does not run a required-features test either

`cargo test -p hl-native --all-targets` reports green while running **zero** of the
tests in a target declared `required-features = ["native-test-hooks"]`. Cargo skips
such a target silently, exactly as it does for the application binaries below.
`tests/namespace_transaction.rs` is one of these, and a lane discovered mid-run that
the gate command this file recommends was not exercising any of it.

That matters twice over: the same feature flag is what makes a symbol's only writer
test-only, which is the defect `c-test-only-state` exists to catch. So the build
configuration that hides the writer is also the one whose tests do not run by
default. Add `--features native-test-hooks` as a second invocation when you touch
anything behind that flag, and say which of the two you ran.

### `--all-targets` does not reach the application

`src/apps/husklet` declares `[[bin]] husklet` with `required-features = ["gui"]`, and its
`runtime` feature is off by default. Cargo **skips a target whose required features are
unmet without printing anything**, so `cargo clippy --workspace --all-targets` covers the
`hl` library's default surface and none of the ~10,800 lines the signed application is
built from. A lane can rename a shared type, watch the workspace go green, and have broken
the product. That is the permanently-armed form of the merge hazard above.

Making it visible cost four lines of `flake.nix`: the Linux dev shell now carries `gtk4`,
`librsvg` and `vte-gtk4` exactly as the Darwin one does, all substitutable (~257 MiB, no
source builds). CI therefore runs the two feature-gated Clippy commands explicitly.
Run the same `cargo clippy -p husklet --all-targets --features runtime` and
`cargo clippy -p husklet --all-targets --features gui` commands inside `nix develop`
when you only want the application answered.

What this does **not** cover, and no Linux run can: linking and running the GTK app,
the `#[cfg(target_os = "macos")]` arms in `bin/host/{environment,pty,appearance}.rs` and
`runtime/process.rs`, the objc2 title-bar and appearance code, and everything in
`.github/workflows/release.yml` — bundling, code signing, notarization, the DMG. A green
CI verification run on Linux means the application *type-checks and lints*; it does not mean it
runs. The macOS arms are still only compiled by CI on `macos-26`.

### Work in your own worktree, and stage by path

Several lanes edit the shared checkout at once. Two things follow, both of which
have already cost work today:

- **`git add -A` in the shared tree stages other lanes' uncommitted files.** One
  lane swept another's `main.c` into its commit and caught it only on the stat.
  Always `git add <path>`, never `-A`, and read the stat before committing.
- **A dirty shared tree breaks everyone's build.** A lane found the tree would
  not compile because of an unrelated in-flight edit, and had to run its gate in
  a clean detached worktree to get a trustworthy answer. If the tree does not
  build and your diff cannot explain it, check `git status` before debugging.

Prefer your own worktree. If you must work in the shared tree, leave it clean.

### Copy a built binary in its own command, then run it once

A lane copied `release/hl-x86_64` inside the same script as its `cargo build`,
immediately after the build returned. The copy was corrupt — and **corrupt
deterministically**, identical sha256 twice, which is what made it credible. A
wrong answer that reproduces looks exactly like a right one.

It then failed as `Engine(Load(Inspect { role: Main, error: WrongArchitecture }))`
— pointing at the guest, not at the copy — and was reported as a tip-wide break
that stopped an amd64 guest loading. Rebuilding from the same commit produced a
working binary; all six commits in the suspected range ran cleanly.

So: copy in a separate command after the build has settled, **run the binary once
before you use it for anything**, and quote its sha256 beside your numbers. The
same lane earlier got 15 phantom `E0560` errors in an untouched file by rebasing
a worktree while clippy was reading it. An artifact taken from a tree or a
directory that is still moving presents as a defect somewhere else entirely.

### Commit before you mutate

Non-vacuity checks work by breaking the fix and confirming the assertion
reddens, so the tree spends time in a deliberately wrong state. Two ways that
has destroyed work here:

- `git checkout -- <path>` reverts to **HEAD**, not to your pre-mutation state.
  A lane lost uncommitted fixes mid-sweep this way. Commit the fix first, then
  mutate, then restore from the commit.
- `git stash` is **repo-global, not per-worktree**. Two lanes stashing
  concurrently interleave, and `stash@{0}` is not necessarily yours. Drop by
  matching the stash message, or avoid stash entirely in favour of a commit.

### A commit message is not evidence

Re-run the suite on the tree you are about to merge, not on the tree the lane
measured. Two claims failed to reproduce on the same day:

- A commit message stated that `alpine_runtime_contracts` passed. Merged with
  tip, it **failed at exactly the assertion the message said it fixed** — a
  `poll` timeout fell through into a blocking `accept()`, so the bounded step
  that the whole design rested on had never been bounded.
- A `comm` fix was reported complete and was red on its own fixture: the
  zero-length write was short-circuited at *two* layers and the lane had found
  one.

Neither lane was careless — both were reporting a state that was true when they
measured it. Evidence ages: a rebase, a sibling merge, or a second defect behind
the first is enough. So verification is a separate job from implementation, and
the verifier re-derives rather than inherits — including non-vacuity, which is
cheap to redo and is the check most likely to have gone stale.

### Clippy and rustfmt only work through the pinned shell

`cargo clippy` invoked directly fails on `hl-native`'s build script with
`error[E0514]: found crate cc compiled by an incompatible version of rustc`. The
shared `target/` was populated by the flake toolchain, and a host-resolved
`cargo` is a different rustc reading the same directory. This is not a defect in
anyone's diff and `cargo clean` is the wrong response — it would discard tens of
gigabytes other lanes are using.

Enter the pinned shell with `nix develop` before running `cargo clippy` or
`cargo fmt`. Two lanes have now reported the E0514 as a mysterious failure of
their own change.

### A real Docker is reachable — use `sudo -n docker`

Several lanes have reported "no live baseline available" and fallen back to the
documented API. The socket is `root:docker` and our uid is not in the group, so
an unprivileged probe genuinely is denied — but **`sudo -n docker` works**, and
Docker 29.1.3 is running on this box.

That matters because Docker's documentation is thinner than its behavior. Probing
it directly produced findings no spec would have given: `"TERM "` with trailing
whitespace is rejected, `SIGRTMIN+16` is refused although signal 50 is valid
under the name `RTMAX-14`, `09` and `+9` parse but `0x9` does not, and the daemon
**unpauses before delivering every signal**, not only `SIGCONT`.

Measure the real daemon before writing a conformance assertion. State plainly if
you could not.

## Checking whether the box is busy

Use `pgrep -cx testing`, which matches the exact process name. Every
pattern-matching form is wrong in one direction or the other:

- `pgrep -cf "target/release/testing"` is **structurally blind**. Lanes set
  `CARGO_TARGET_DIR` under `/var/tmp`, so their binaries live at
  `/var/tmp/<lane>/release/testing` and never match. A measured instance reported
  2 against a true 11. A lane invoking `./target/release/testing` relatively from
  a worktree defeats it too.
- `pgrep -f "testing runtime"` and `pgrep -af "/release/testing"` **over-report**:
  the querying shell's own command line contains the pattern, so they count
  themselves. Measured 2 against a true 1.

Use `pgrep -ax testing` when you need the rows as well as the count.

**One guest is not enough for anything keyed on a guest address.** With the
native write-reservation gate off entirely — same engine binary, same options,
same source — base malloc measured 1,008,823 us on the sqlite guest and
7,031,876 us on the sqlite-free one, reproduced by a second lane at 7.32x with
every other phase between 0.98 and 1.02.

The cause is not the guest binary as such. `allow_direct` is computed per
admission, and whether an entry pc qualifies for direct authority is a property
of the guest's code layout. When admissions alternate, `memory_mode` alternates
with them, and because `hl_native_cache_epoch_matches` folds `memory_mode` into
a **cache-wide** identity, every alternation discards the whole translation
cache: 1,642 epoch and 1,652 direct resets on the slow guest against 38 and 37
on the fast one, all with `mapping`, `instr` and `identity` unchanged. Removing
the flip takes that phase from 441,906 us to 61,710 us.

So a guest can put the engine into a pathological state that has nothing to do
with the phase's own work, and running the phase alone hides it completely —
in isolation both guests measure ~960,000 us. Measure on at least two guests,
run the full sequence rather than one phase, and report **every** phase: a
withdrawn table listed six and omitted a 1.37 string regression in its own data.

**Sampling cannot hold a window; take the lock.** A 120-second all-clear says
nothing about minute three, and a lane lost a measurement to a sibling's gate
that started after its window opened. Widening what the sample sees does not
change that. So a measuring lane takes an exclusive lock for the *duration* of
its run, and anything that loads the box takes it shared:

    # measuring — exclusive, held for the whole run
    flock /var/tmp/husklet-box.lock -c './timing.sh'
    # building or gating — shared, waits for a measurement to finish
    flock -s /var/tmp/husklet-box.lock -c 'cargo test --workspace --lib --bins'

`flock` blocks until it can acquire, so a gate started mid-measurement waits
instead of contending, and a measurement started mid-gate waits instead of
reading a poisoned floor.

Most measuring runs have a cheap phase and an expensive one — controls are
contention-insensitive, candidate arms are not — and the `-c` wrapper would
hold the box exclusively through both. Take the lock around the expensive phase
with the descriptor form instead:

    exec 9>/var/tmp/husklet-box.lock   # once, NOT in a subshell or pipeline
    flock -x 9
    ... candidate arms ...
    flock -u 9

`flock` is per-descriptor, so `exec 9>` inside a `$(...)` or a pipeline locks
nothing and the run merely *appears* protected — the same shape as the
`pgrep -f` self-match, where the mechanism looks like it is working.

**The descriptor is inherited, so killing the script does not release the
lock.** Every child gets fd 9, and the lock lives until the last inheritor
dies: one lane killed its sweep script and the lock stayed held by the sweep
process and its ten workers, surviving SIGTERM and needing SIGKILL on all of
them. This is the same property that makes the lock crash-safe — release is
tied to descriptors, not to the script.

**`flock` has no fairness, so a waiting exclusive starves.** On Linux a pending
exclusive does **not** block new shared acquirers, so while builders keep
arriving faster than they drain, the measurement never gets its turn: one lane
sat on `flock -x` for minutes against 21 shared holders. This is the mirror of
the problem above — there a long exclusive job starved short ones, here the
short ones starve the long one — and the lock answers neither.

Announce intent before requesting, so builders can yield:

    exec 8>/var/tmp/husklet-box.wanted  # measuring lane, before flock -x
    flock -x 8                          # ... then take fd 9 and run

    # builder, before taking flock -s
    while ! (exec 8>/var/tmp/husklet-box.wanted; flock -n -x 8); do sleep 5; done

**Announce before you request, or the announcement is retroactive.** Take fd 8
*then* fd 9. A lane that queued its exclusive request first and announced
afterwards left three and a half minutes in which builders were free to keep
arriving — which is the whole window the announcement exists to close.

**Count lock holders, not processes named `testing`.** The quiet probe reads
`pgrep -cx testing` and will happily report zero while eleven holders are live,
because they are `cargo`, `clippy`, `nix` and a renamed engine. Walk
`/proc/*/fd` for the lock file instead: holder count is the occupancy signal,
and it is immune to every naming blind spot above.

**Signal intent with a second lock, not a plain file.** A `touch`/`rm` marker
has no crash-safety: if the announcing lane dies, is killed, or has its turn
reaped, the file survives and stalls **every** builder on the box
indefinitely — and unlike the lock, nothing releases it. A lane that used the
`touch` form had to arm a detached watchdog polling its own pid to remove it,
which is a workaround for the wrong primitive. Held on a descriptor the marker
inherits the kernel's release-on-death, exactly as the box lock does.

Advisory again, and it inherits every weakness of the lock: a builder that
skips the check is invisible. But it converts starvation into a deliberate
choice rather than an emergent one.

**The lock prevents contention; it does not schedule.** It is strictly
first-come, so it cannot express "the long restartable job yields to the short
irreplaceable one". A 3250-row sweep acquired instantly while a measuring lane
sat in a cheap phase holding nothing, and would have starved minima behind it
for hours. Express priority outside the lock: wait for sustained quiet
*before requesting it*, so the request only forms once the other lane is
genuinely finished. And prefer yielding — restartable work should release for
work that must be re-taken from scratch.

**Bound the wait.** An unbounded `flock -x` plus an unbounded quiet loop hangs
forever, and adoption of an advisory lock is never complete. Cap it, then
proceed and **record the load you actually ran at**: a run reporting "ran at
load 9, stated" is worth more than one that blocks silently.

**A crashed lane cannot wedge the box, and the lockfile is never cleaned up by
hand.** The lock lives on the file descriptor, so the kernel drops it when the
holder dies — SIGKILL and a reaped turn included; verified behaviourally.
There is no stale-lock state and no `rm /var/tmp/husklet-box.lock` recovery
step. **Deleting the path is the one thing that does break it**: it does not
release anything, and the next lane creates a fresh inode and acquires
immediately while the real holder is still measuring.

**Do not tell a lane to idle for a window the lock already governs.** A builder
under `flock -s` blocks only for the exact interval a measurement holds
exclusive, automatically, with no coordination between them — that is the whole
advantage over a granted window. Layering "wait until I say so" on top of it
brings back the serialization cost and keeps the lock's overhead.

This is advisory in the strict sense — it converts a contention problem into a
compliance problem. A builder that forgets the lock is indistinguishable from
one that holds it, so **a granted lock is not proof the box is quiet.** Keep
the name-matched check *behind* the lock, not instead of it.

**`testing` is not the only thing that loads the box, and the others are the
ones we generate constantly.** A `cargo test -p hl-engine` test binary is named
`hl_engine-<hash>`, which `-x testing` cannot match and `-cf "release/testing"`
cannot match either. Engines invoked directly (`hl-aarch64`, `hl-x86_64`) are
equally invisible. One lane held a clean 120-second window for `testing` while
a sibling's gate run sat at 99% CPU throughout it. Since every lane runs the
gate, this is the most common competing load there is.

Check for all of them, and use load average as a second condition rather than
a substitute — it lags, so it confirms a busy box quickly but cannot prove a
quiet one:

    pgrep -cx testing; pgrep -c 'hl_engine-|hl-aarch64|hl-x86_64'; pgrep -c cargo

**A renamed binary is invisible to this check.** `pgrep -cx testing` matches
the exact process name, so a lane that copies the driver to `testing-bin` or
any other name runs unseen for its entire measurement and every other lane
reads the box as free. If you copy the driver, keep the basename `testing`
(`bin/testing` is fine — `-x` matches the name, not the path).

**`pkill -x` kills other lanes' shared tooling.** Cancelling your own build with
`pkill -x flock` matches every lane queued on the box lock, not just yours: one
lane lost nineteen minutes of queueing that way. The name you are killing is
almost never yours alone — `flock`, `cargo`, `rustc`, `sleep` are all shared.
Kill your own process group or a PID you recorded when you started it.

**`pkill -f` kills the caller.** The killer's own argv contains the pattern, so
`pkill -f "bash timing.sh"` matches the cutover script running it and takes
both down — before it can start the replacement. A lane did this one message
after writing the `pgrep -f` self-match warning into this file. Resolve the PID
first and kill that, or match the process name with `pkill -x`. It is the same
hazard as `pgrep -f` with the opposite consequence: there the loop never
clears, here it fires on itself.

**Long measurements must outlive the turn that starts them.** Background jobs
are reaped when a turn pauses: one lane lost a nine-minute arm at the eight
minute mark with no results file ever written. Start anything longer than a
turn under `setsid nohup` so it survives, and have it write results
incrementally rather than only at the end.

**A single zero does not mean the box is free.** A measuring lane runs a
*series* of invocations with brief gaps between them, so a point-in-time
`pgrep` reads zero in every gap while the job is very much alive. Load average
cannot rescue this either: all three figures read ~0.8 while a run was sixteen
seconds underway, because they are decaying averages of an eighteen-core box.
Require **quiet for a sustained interval** — poll every few seconds and only
declare free after ~120 consecutive seconds at zero. A manager granting a
window on one sample will hand it out mid-series, and the lane that accepts it
adds load to someone's minima. `pgrep -ax testing` shows the row's start time,
which is how you tell "just finished" from "sixteen seconds in".

**Do not poll for your own long job — capture its exit code at the call site.**
`cargo clippy; echo EXIT=$?` at the invocation site is the whole technique. A
waiter built on `pgrep -f "cargo clippy"` matches its own command line and blocks
forever on itself: one lane reported lint as still running for nine minutes after
it had finished, then read a `sleep && tail` wrapper's exit code as lint's verdict and
reported a green it had not observed. `-cx` cannot rescue this, because a Cargo
subcommand has no distinct process name. The self-match hazard is generic to
`pgrep -f`, not specific to `testing`.

Timings taken without this check are not evidence. Counter ratios, code-size
deltas and categorical pass/timeout results survive contention; minima do not.

## Know which tree you are standing in

`cd` persists across shell calls. A single `cd` into a worktree silently
relocates every later `git`, `grep` and `cargo` until something changes it back,
and the output looks identical either way. This has produced a merge and a push
against the wrong tree, and a "this symbol no longer exists" conclusion drawn
from a lane's worktree and nearly recorded as a fact about the shared branch.

Prefer `git -C <path>` and absolute paths over `cd`. When a finding depends on
which tree it came from — a grep that found nothing, a test that passed, a file
that is missing — re-run it from the shared tree before recording it, and say
which tree the evidence came from.

CodeGraph resolves against the tree its index was built in, not your current
directory, so a shell that has wandered into a worktree gets structure from
somewhere else. Worktrees carry their own `.codegraph/`; if yours does not, run
`codegraph init -i` there rather than trusting the parent's index.

The same applies to **which commit**, and it has cost two lanes a detour on the
same day. A lane that cuts a worktree, works for an hour, and then compares its
lint or test findings against `main` is comparing against a tree that has moved —
one lane concluded a `loader.rs` line "does not exist upstream" when `main` had
advanced fifteen commits underneath it. Diff findings against **your parent
commit**, not against `main`.

`cargo clippy ... -- -D warnings` makes this sharper, because it denies lints the
crate's configured set only warns about, so it surfaces pre-existing findings as
though they were yours. That does not make it the wrong command — it is
`flake.nix:412`, the repository's own verification gate, and without the flag
clippy exits 0 no matter what is wrong because `workspace.lints` sets everything
to `warn`. But attribute its output against your parent before claiming or
disclaiming a finding.

## The x86 arm of the scheduler lags the arm64 arm

`native_aarch64` and `native_x86` are maintained in parallel by hand and the x86
side is repeatedly the one missing a piece. Two independent lanes found this on
the same day, both in `scheduler/native.rs`:

- `native_x86` never called `mark_productive`, so the entry-productivity set was
  permanently empty on amd64 and every suppressed entry latched forever — an
  ISA-wide regression invisible to any arm64 benchmark.
- `native_x86` never bumped `entries`, `declined_suppressed`, `declined_cold` or
  `declined_executable`, so `hl-native-entry:` printed all zeros on every amd64
  run and dumped the whole probe population into `declined_other`. Every amd64
  admission reading ever taken from that line was meaningless.

Neither would fail a test. Both would ship green, because the gates and the
benchmarks lanes reach for are arm64.

So: when you touch either arm, **enumerate what the other one does** and say
which of the two you checked. Confirm by enumerating the call sites or match
arms, not by reading the surrounding prose — one of the above was found only
because a lane listed which `NativeExit` variants reach a call and compared the
two functions side by side.

## Balance the arm order, or measure a 4% lie

Running base first and candidate second in every round puts a uniform **+4% on
the candidate** on this box. It was caught because the inflation appeared on
`compute`, `branch`, `intdiv` and `atomics` — phases the change under test could
not touch. Alternating (base/cand then cand/base) collapsed those four to 1.003,
0.998, 0.997 and 1.000.

Interleaving alone is not enough; the *order within each pair* must alternate.
A fixed order survives every other precaution — pinning, minima, per-arm `ok=`
verification — and none of them detect it.

The damage is not uniform, so it can invent or hide a specific verdict: `file`
read 1.039 under fixed order and 1.006 balanced, which is the difference between
a disqualifying regression and parity.

Include at least one phase the change provably cannot affect, and check it reads
1.000. If it does not, the harness is lying and nothing else in the table is
evidence.

## `bench --results` is a resumable ledger; never reuse a path

`bench` keys a resumable ledger on `--results`. Point two runs at the same path
and the second **replays the cached rows instead of measuring**, then prints a
clean `PASS`. There is no warning and the table looks perfect.

So give every run a unique results path. A lane reusing one across arms would
produce a plausible A/B table in which one arm was never executed.

Two related harness facts worth knowing before you build your own repeat loop:
each case already runs `repetitions: 3` and reports `min_us`/`median`/`p90` per
phase, so take `min_us` and minimise across your rounds on top of it. And the
guest is built by the harness per arm from the same `main.c`, so both arms share
a source but not necessarily a binary — see below.

## Identical source does not mean an identical binary

Two builds of **byte-identical source**, same tip, same toolchain, worktree paths
of equal length, differed by **152 bytes and a different sha256**. A candidate
build differed from base by 3,520.

That is why a base-versus-base null arm is not ceremony: it measures how much
ratio a phase can show for no reason at all. If the null arm's spread covers the
candidate's effect, the candidate is not evidence however clean the other
controls read. Phases with small absolute times are where this bites — a few
hundred microseconds of drift is percent-level on a 2.6 ms phase.

## A worktree on one host is invisible to the other, and git will prune it

The macOS host and the Linux VM share the repository but **not** `/var/tmp`. A
worktree registered from one host at a `/var/tmp` path does not exist as far as
the other host is concerned, so any `git worktree` command run there — including
the `git worktree prune` that `git worktree add` performs implicitly — treats it
as stale and deletes its administrative directory.

Two lanes lost worktrees to this on the same day. The branch ref and the commits
survive in the shared repository, but the worktree's `HEAD`, `gitdir` and
`commondir` are gone, which surfaces as a `nix develop` failing mid-gate or as a
clippy error with no obvious cause — not as anything that mentions worktrees.

So: **do not run `git worktree` commands on one host while another host holds a
`/var/tmp` worktree**, and if a gate fails for a reason that makes no sense,
check that your worktree still has its administrative directory before believing
the error.

The same asymmetry applies to killing processes by pattern. A lane matched four
`cargo test` processes on the macOS host, killed all of them to unblock its own,
and only one was its own. Resolve a PID and kill that PID.

## A measurement names its host, or it names nothing

The engine runs on two hosts and they are not interchangeable. The same guest
binary doing the same pure-CPU work measured **58x slower under the engine on
the macOS host than under the same engine on the Linux VM** — on the same
physical Mac, so the silicon is identical. `apt-get update` is 7.5 s on the
Linux host and ~119 s on the macOS host. The penalty is code-shape specific:
branchy, pointer-chasing, memory-touching code explodes, while straight-line
NEON reads 3.4x and a large file write reads 2.0x.

This went unnoticed for a long time because the head-to-head that exposed it was
taken on macOS while essentially every attribution in the perf record — per-exec
cost, the fd-scan work, `gencaches`, the floor benchmark — was taken on the Linux
VM. Those numbers were not wrong; they described a host on which the dominant
user-facing defect is absent.

So: **state the host with every number you report**, and treat a result from one
host as silent about the other. When a user-facing complaint is about the macOS
app, a Linux measurement cannot confirm or refute it. Reach for the Linux VM when
you want a fast iteration loop, and re-confirm on macOS before you believe a
ratio describes what the user feels.

## A C change reaching the binary is a separate claim, and it has its own check

The engine is **dlopened, never linked**. `libhl_native_engine.so` is built by
`hl-native`'s build script into its Cargo `OUT_DIR`, and the process loads it at
runtime: on Linux the worker does not contain the engine and does not list it in
`ldd`. Three consequences, each of which has cost a lane real work:

- **A Rust binary's sha256 is unchanged by almost every C edit.** That is the
  designed behaviour, not a symptom. A lane read four byte-identical test-binary
  hashes across base, a patched tree, a reverted tree and a forced relink, and
  concluded its C never reached the build. The hashes proved nothing either way.
- **Copying the test binary does not snapshot the engine.** The same lane saved
  `bin/base`, `bin/fixed` and `bin/fixed2`; all three are one file by sha256 and
  all three resolve to the single mutable `.so` in the target directory — the one
  the *last* build wrote. Arms cannot be separated that way. To hold an arm, copy
  the `.so` too, or give each arm its own `CARGO_TARGET_DIR`.
- **The stale `.so` is real and has fired repeatedly.** A `.so` built into one
  `build/hl-native-<hash>/out` while a binary loads another — a different feature
  set, a different target directory, or a copy taken at another moment — runs the
  previous engine silently.

### What is and is not sufficient evidence

Insufficient, all three demonstrated:

- The **static archive** changing, or containing the new symbol. `libhl_c_backend_target_aarch64.a`
  grew and carried the new symbol in exactly the run that was later believed stale.
- **`strings` on the `.so`.** It answers what is on disk, not what the process mapped.
- The **Rust binary's hash**, in either direction — see above.

A **runtime `fprintf` probe** is sound only if you first prove the line you are
comparing against actually prints. The lane's decisive check was an unconditional
`fprintf` added beside the existing `[ckpt] coordinator pid=` line; the new line
did not appear, and that was read as proof of staleness. Neither line appears in
any of the twenty logs it preserved, because `ckpt_coordinate_and_exit` is not on
that test's path at all. The check compared an absent line to an absent baseline.
If you use a runtime probe, put it somewhere whose output you have already seen.

### The check to run

`cargo test -p hl-native --test build_freshness`

It recomputes a content fingerprint over every file under `src/native` and compares
it to the value Cargo baked into the running executable, and it loads the engine so
the loader's own comparison runs. They diverge exactly when the executable predates
the tree. The same value is compiled into the `.so` and exported as
`hl_c_backend_build_fingerprint`; the loader refuses a shared object whose value
disagrees and names both fingerprints, so a stale artifact is a load failure rather
than a quiet measurement of the previous engine.

Because the fingerprint is a `cargo:rustc-env`, a C edit now also invalidates the
Cargo fingerprint of every Rust artifact built against `hl-native`. Downstream
binaries are rebuilt and their hashes do change — the byte-identical reading that
started this is gone. That is a deliberate cost: a C-only edit now recompiles the
Rust crates above `hl-native` as well, which on this workspace is about two minutes
rather than forty seconds. It buys the property that a saved binary is an arm.

Incremental builds themselves are sound, in the shared tree and in a worktree with
its own `CARGO_TARGET_DIR` alike: the build script's `rerun-if-changed` covers every
file under `src/native`, and it recompiles and relinks unconditionally when it runs.
`cargo clean` is not the remedy for anything here.

## A control that merely seems unaffected is not a control

Disable the code path in both binaries and measure that. Anything weaker is a
guess about which phases are unrelated, and the guess has already been wrong.

A suppression change was rejected on a 5.8% `syscall` regression. With native
execution disabled in **both** builds — so the changed code is unreachable and
the two must measure identically — `syscall` still read **1.057**, the worst
phase in the control, and 13 of 17 phases favoured base. Systematic, not random:
the candidate's engine is simply laid out differently, and `syscall` runs the
most engine host-side code per guest instruction, so layout shows there first.
Corrected, the algorithm's own cost was **1.012**. The change was killed for
~5% of binary layout.

`compute` and `branch` read 1.003 and 0.998 in the same balanced runs and would
have waved the change through. They looked like controls and were not — they
were merely phases the change did not reach, which says nothing about what else
differs between two binaries.

Where disabling the path is impossible, a base-versus-base null arm is the
accepted substitute and must read 1.000. A control derived from the mechanism is
better still: one lane used `compute` at 250 probes against 366,696 on `syscall`,
having first shown the cost scales with probes.

## Reading a profile

High self-time and removable cost are independent properties, and this engine has
produced both failure modes:

- **Misattributed self-time.** `with_execution_memory` compiles to 4032 bytes
  because the guest-slice closure is inlined wholesale into it, so its row credits
  work done by its callees. Disassemble before believing a row; a function whose
  body should be twenty instructions and measures a thousand is reporting someone
  else's cost.
- **Real self-time that is still free to keep.** `ReservationEpochs::invalidate_at`
  is a genuine 112-byte function and its row is honestly its own, but deleting it
  along with all 5.17 billion of its atomics changed nothing measurable, because
  the `ldadd` discards its result and retires without blocking anything.

So a profile row justifies investigating a symbol. Only a mutation justifies
believing the cost can be recovered.

Time the mechanism before sizing a fix for it. The native/host operand round trip
was assumed to cost about a microsecond and to dominate sqlite; measured, it is
105ns and 0.35% of the phase, so an entire direction was worth a tenth of a
percent. A count is not a cost until you have multiplied it by a measured one.

Counters are comparable within a build and not across builds. Adding
instrumentation changes inlining, which changes translation admission: two builds
of the same source reported 892,141 and 1,593,713 for the same counter. Compare a
counter only against itself in the same binary.

## Time-to-evidence and agent utilization

Elapsed time to authoritative compatibility evidence is the primary operational
optimization target. CPU is not a scarce resource for repository work: use every
logical CPU when a test, corpus run, compilation, or independent analysis can
benefit from it. Do not serialize work merely to keep CPU utilization low, and do
not default compatibility runs to one worker when the host can safely run more.

RAM, disk space, process-table health, and source/build ownership remain hard
constraints. Before and during wide execution, monitor available memory, swap,
free disk, output growth, and zombie or escaped descendants. Bound per-worker
captures and timeouts, preserve resumable results, and reduce concurrency only
when measured RAM, disk, thermal, or lifecycle evidence requires it. A slow run
must report whether it is limited by CPU, memory, disk, fixture setup, process
startup, locking, or guest timeouts; unexplained serialization is not acceptable.

Keep all available Codex subscriptions and agent slots productively occupied.
Managers must continuously delegate broad, independent, non-overlapping migration
domains, require direct C-oracle and Rust-source study, and replace a completed
assignment with the next highest-value compatibility gap immediately. Each Codex
manager should use its own subagent capacity fully. Coordinate shared-tree edits
and build ownership so maximum agent utilization does not create conflicting
patches or invalidate evidence. Prefer parallel read-only audits while one owner
performs a shared-tree build or authoritative run.

Keep implementation sessions short-lived and outcome-bounded. A normal lane owns
one coherent capability for at most 20 minutes; an external manager coordinating
several independent subagents has a hard 30-minute lifetime. It must then deliver
one audited commit with exact-tree evidence, or a concise source-backed blocker
report, and exit. Repeated diagnosis, fixture-by-fixture iteration, or widening
the lane after its original capability is exhausted is not progress: stop that
session and give a fresh agent the next bounded domain. Preserve unfinished work
on its branch or worktree; never manufacture a cosmetic commit merely to meet the
deadline.

Compatibility workers receive engine launch options only through the typed
`HL_COMPAT_ENGINE_OPTIONS` setting. Setting an engine option directly in the
inventory supervisor's ambient environment does not configure the guest engine and
must never be cited as evidence of a selected mode.

### There is no native/interpreter switch, and the counters that proved it are gone

This subsection used to instruct lanes to request native execution with
`HL_NATIVE_EXECUTION=1` and to confirm it from `probes`/`entries` on the
`hl-native-entry:` line. **Every part of that is now stale, and following it wastes
a lane.** Re-derived on 2026-08-18:

- `HL_NATIVE_EXECUTION` had no consumer anywhere in the repository — C or Rust. The
  authoritative engine registry is
  `src/runtime/hl-native/src/native/engine/options.c`, and it never defined any
  `HL_NATIVE_*` option. The eight dead entries have been removed from
  `src/containers/hl-engine/src/options.rs`; `retired_native_executor_options_are_not_registered`
  pins their absence.
- `hl-native-entry:`, `probes` and `entries` have **no producer** in the tree. Only
  testing-side parsers mention them. A lane that waits for those counters is waiting
  for output nothing emits, and may conclude its own build is broken.
- There is no interpreter in the production engine. `translator/` and
  `engine/target/{aarch64,x86_64}.c` are the execution path; the native-vs-interpreter
  split belonged to the deleted Rust executor, which the "Historical" subsection below
  already marks as gone. Container workloads were never interpreter numbers.
- The engine workers accept **no `--engine-option` flag at all**. `LaunchArguments`
  (`src/apps/engine/src/lib.rs:55-68`) has only `--guest-isa`, `--report-exit`,
  `--rootfs`, the executable and its arguments, and both construction sites use
  `Options::default()`. Measured: `./target/release/hl-aarch64 --engine-option
  HL_C_DIAGNOSTICS=1 /bin/ls /` exits **2 with no output**, while the same command
  without the flag runs the engine. So an unknown option does not get silently
  ignored — it kills the run before the guest starts.

The durable lesson survives its example: **confirm a mode from a counter, not from the
command you believe you ran** — but first confirm the counter still has a producer.
The instruction above outlived the mechanism it described by long enough to mislead
several lanes.

Retained-C diagnostics remain real and are requested with `HL_C_DIAGNOSTICS`, which is
the one launch effect `Execution::Native` still carries.

A commit may be called stable or buildable only after verification from that exact
committed tree. A passing build in a dirty shared worktree is not evidence for
`HEAD`: uncommitted companion schema, match, generated, test, or composition edits
may be supplying the successful build. Before handing a revision to another lane
or starting an authoritative corpus run, verify it in a clean detached worktree
or equivalent clean checkout and record the tested commit. Do not continue shape-
changing edits until the dependent verification has captured a coherent commit.

## Mission

Provide isolated, reproducible Linux workspaces backed by the production C
execution engine in the Cargo package `src/runtime/hl-native`, with a memory-safe
Rust control plane.
Opening a workspace enters its configured
image with a terminal, filesystem, networking, VPN, and container services.

Preserve exact Linux behavior across AArch64 and x86-64 guests and Linux, macOS,
and Windows hosts. The product composes replaceable engine, container, workspace,
and terminal capabilities; reusable crates contain no Husklet product policy.

Ordinary CLI and terminal applications must run without application-specific engine
workarounds. The final compatibility/performance gate includes container workflows,
interactive terminal workloads, and nested engine execution such as `arm -> amd -> arm`.

Production engine behavior must never branch on an application, language, runtime,
framework, executable name, build-information marker, or vendor identity.  In
particular, Go, V8, JVM, and similar guest internals are not Linux ABI
domains. When inherited C contains such a branch, preserve it as migration evidence
and identify the violated generic invariant (for example non-PIE guest-address
placement or signal semantics); repair that invariant in its authoritative
owner rather than adding runtime-specific detection in C or Rust. Guest-visible
addresses remain ELF/Linux addresses;
host storage placement is an internal mapping detail and must not leak into guest
pointers, symbols, signals, `/proc`, checkpoints, or runtime metadata.

## Production/native ownership study before every engine lane

Reading compatibility fixtures and expected output is necessary but insufficient.
Before changing a runtime domain, the lane owner must inspect the selected
implementation under `src/runtime/hl-native/src/native` and, when provenance or an
independent baseline matters, the corresponding pinned read-only implementation
in `../engine`. Record:

- the exact C and assembly files and entry functions studied;
- state ownership, identity, lifetime, locking, and teardown behavior;
- syscall ordering, partial-result, blocking, cancellation, signal, and errno
  semantics relevant to the lane;
- architecture-specific and host-specific branches;
- the explicit mapping from each observed capability to its current production
  C owner, Rust control-plane boundary, or honest remaining replacement gap.

Record this oracle audit beside the relevant compatibility or performance report
before the lane is accepted.
An agent report that cites only tests, manifests, expected output, or summaries
does not satisfy this requirement. Never edit `../engine` while performing the
audit.

### The oracle is authoritative about intent, not about the kernel

`../engine` shipped, so what it *does* is strong evidence about what guests
depend on. It is not evidence about what Linux does, and where a host
measurement and the C disagree, **the kernel wins**. An oracle comment asserting
kernel behavior is a claim to test, not a fact to port.

Two lanes found this the same day, in unrelated domains:

- `src/linux_abi/syscall/io.c:1384` states that a comm write "drop[s] one
  trailing newline" and implements it, and ignores a zero-length write. The host
  kernel does neither — only NUL terminates, and a zero-length write clears
  comm. The Rust had faithfully reproduced the wrong comment.
- `src/linux_abi/container/state.c:596` initializes the capability sets to
  `HL_CAP_DEFAULT` unconditionally and `HL_UID` never reaches them, so a C
  `--user` container reports the full container set. Linux clears
  permitted/effective across a root-to-non-root transition. The Rust is ahead of
  the oracle here, and following the C would have been a container-escape
  regression.

So: measure the host first, then read the C to learn what the guest-visible
contract is meant to be. When you override the oracle, say so and show the
measurement. A fixture that passes on the bare host kernel as well as in the
engine validates the assertion; one that only passes in the engine validates
nothing.

### Change domains, not failing cases

The integrated `hl-native` C backend is the production implementation. Compatibility
cases are acceptance evidence and prioritization signals; they are not a
substitute for understanding the complete implementation that already works.

Before fixing a corpus cluster, read the complete integrated C domain and its call
graph rather than only the function named by the first failure. Inventory every
entry point, state object, ownership edge, lock, wakeup, error path, architecture
branch, and teardown transition, then compare that inventory mechanically against
the current in-repository owners. Record a dense capability matrix with each
capability marked selected, replacement-candidate, divergent, or missing.
Implement the largest coherent missing mechanism and all of its widths, flags,
lifecycle paths, and error semantics before returning to the corpus.

Walking one executable until it exposes the next unsupported instruction or
patching one fixture-visible branch at a time is forbidden when the integrated C
tables or domain implementation can reveal the complete family in one audit.
Likewise, a narrow passing case does not prove a domain port complete. Acceptance
requires focused cohort evidence after the implementation comparison and later a
full-corpus checkpoint from the exact committed tree.

## Source layers

The source tree separates reusable foundations, engine runtime domains, native
execution, container capabilities, workspace capabilities, and the product root:

```text
src/
  packages/   transferable libraries and repository tool packages
  runtime/    engine runtime domains, including the hl-native Cargo package
  containers/ container services and the integrated hl-engine
  workspaces/ workspace, terminal, and generic GUI capabilities
  apps/husklet/ the product composition root
```

Dependencies point inward:

```text
husklet -> workspaces + containers -> runtime -> packages -> std
                              -> hl-native (Cargo-built C engine)
```

- Production libraries in `packages/` must make sense without an engine, guest,
  syscall, emulator, or container.
- `runtime/hl-native` owns the integrated C execution engine and its narrow Rust FFI boundary.
- `containers/hl-engine` owns validated plans, lifecycle supervision, and product-facing engine composition.
- `apps/husklet` selects product adapters and composes containers, workspaces, terminal, and GUI.
- No package depends on `apps/husklet`.
- Repository tools live as packages under `src/packages/`, but remain build-time
  machinery and never production dependencies. The generic `hl-design` annotation
  package is the only explicitly reviewed exception when used by production crates.

Changing a local Cargo dependency requires explaining the ownership reason and
passing the dependency linter.

### UI ownership

- `hl-gui` owns generic visual primitives, layout, validation display, accessibility,
  and toolkit adapters.
- Husklet owns screens, settings schemas, product view models, navigation, and feature
  composition.
- Generic components receive state and emit typed intent. They do not persist,
  orchestrate, or invoke services.
- Product components such as workspace pickers, image choosers, removal confirmations,
  and terminal settings stay beside the feature that owns them.
- Native toolkit types do not cross the GUI boundary.
- Add a component only for a stable concept, state contract, interaction contract,
  accessibility behavior, or cohesive reuse; keep one-off layout beside its page.

## Package placement

Ask these questions in order:

1. Is it repository-only lint, differential, fixture, or benchmark machinery
   that is forbidden as a production dependency? Put its package in `packages/`
   and keep the tool boundary explicit. Product compatibility probes and audits,
   including syscall admission audits, are owned by the `testing` application;
   they do not become runtime packages.
2. Does the code extend ordinary logging, filesystem, byte I/O, encoding, or
   another standard-library mechanism without engine vocabulary? Put it in
   `packages/`.
3. Does it implement guest execution, Linux ABI behavior, translation, or a
   host adapter for the integrated engine? Put C implementation in the owning
   capability below `runtime/hl-native/src/native`; keep the Rust surface to FFI
   bindings and safe wrappers.
4. Does it connect engine capabilities or select a concrete host adapter? Keep
   that integration inside the C engine below `runtime/hl-native/src/native`.
5. Does it validate engine configuration, expose the engine API/CLI, or
   construct the complete engine? Put it in `containers/hl-engine`.
6. Does it own product configuration, screens, commands, navigation, or cross-domain
   composition? Put it in `apps/husklet`.

Do not add catch-all packages or modules named `core`, `common`, `shared`, `types`,
`utils`, `helpers`, or `misc`. Name code by the entity, capability, algorithm, or
external mechanism it owns.

Do not create an outer directory containing one crate. The source layers are the
meaningful grouping. Engine concepts such as ISA, memory, networking, tasks,
and execution are capability-owned C subtrees inside
`src/runtime/hl-native/src/native/`; they do not become sibling Rust runtime packages.

## Domain ownership

Each native engine capability owns:

- its entities and value types;
- valid-state construction;
- lifecycle and concurrency invariants;
- domain operations and typed errors;
- consumer-owned capability traits;
- pointer-free, bounded snapshot values;
- platform adapters only when the mechanism belongs solely to that domain.

The C engine exposes one small, versioned boundary through `hl-native`; other
packages must not import native implementation headers or reproduce its models.

Cross-capability operations remain inside the integrated C engine:

| Operation | Domains joined |
|---|---|
| file-backed mapping | descriptor + VFS + memory |
| procfs | VFS + task |
| signalfd | event + task + descriptor |
| Unix pathname socket | VFS + network |
| `SCM_RIGHTS` | network + descriptor |
| fork | task + descriptor + memory + execution |
| exec | task + loader + descriptor + memory |
| provider-backed object | provider + receiving domain |
| syscall trap | execution + Linux personality |
| checkpoint | all snapshot-capable domains |

These adapters use public APIs and owned values. They never access private fields.

## Ports and adapters

A port is a narrow trait owned by the consumer that needs the capability. Add a
port only for a real platform, substitution, testing, FFI, or stable domain
boundary.

Current execution boundary:

- `hl-engine` owns validated launch plans, worker supervision, fail-closed
  C-only backend selection, and the coarse `hl-native` FFI;
- Rust loader and memory services own bounded image/projection inputs;
- selected C owns guest scheduling, Linux personality, translation, and execution;
- memory owns `Backing`; runtime adapts a pinned open-file description;
- VFS owns `VfsHost`; the app supplies the selected host adapter;
- network owns `SocketHost`; the app supplies the selected host adapter.

Never introduce a shared `host-api`, service locator, or omnibus platform trait.
Keep traits small and capability-specific.

## Native execution boundary

The production C/assembly engine lives under
`src/runtime/hl-native/src/native` and is compiled by
`src/runtime/hl-native/build.rs`. It is selected through the Rust boundary in
`src/containers/hl-engine/src/runtime/execution.rs`. It currently owns the complete guest
execution closure: translation, scheduling, Linux ABI services, signals,
filesystem and descriptor behavior, and guest lifecycle. Rust owns product
selection, launch-plan validation, worker supervision, and the public product
interfaces. Builds must not read or link `../engine`.

There is no second production engine tree. Differential or oracle material belongs
under testing, and the build must not select code from `../engine` or
`../engine_rust`. Cargo is the package-level build authority; Nix supplies and
pins its toolchain.

The following narrow boundary described the deleted Rust-executor migration and
is retained as historical design evidence, not as the current architecture:

- CPU layouts whose offsets are embedded in machine code;
- assembly entry, block-return, and trampoline code;
- W^X code-cache mutation, publication, lookup, and chaining;
- POSIX signal/ucontext and Windows VEH/CONTEXT entry;
- fault-context reconstruction;
- async-signal-safe and fork-critical repair.

The candidate boundary was not intended to own Linux syscall, filesystem,
descriptor, networking, task, loader, checkpoint, or product policy.

Cross-language operations are coarse. FFI per instruction, guest memory access,
block lookup, or chain transition is forbidden.

CPU layouts shared across the C/Rust boundary live with `hl-native`; both sides
compile size, alignment, and offset assertions. Hand-maintained duplicate layouts
are forbidden.

### Historical: why the deleted Rust executor translated less on arm64

The measurements and symbols in this subsection describe the deleted Rust
executor. They remain useful performance evidence but do not describe current
production routing or ownership.

arm64 still retires far less translated code than amd64 on short programs, but the
figures this section used to carry are stale and one of its three limiters has been
fixed. Re-measured at 4d2fe7777 in release, runner sha256 `05ad308c…`, one case per
invocation with `--jobs 1` (`testing runtime --case <id> --isa <isa> --jobs 1`); the
suite already sets `diagnostics: true`, so the counters below come from that build
alone and are not comparable with any other.

`runtime/syscalls/gettid`:

- arm64: `interp instructions=96982`, `runs=1 builds=36 hits=82 fallbacks=0`,
  `probes=39 entries=1 declined_cold=38`, `completed=258`.
- amd64: `interp instructions=107996`, `builds=86`, `completed=25558`,
  `x86_cold_builds=86`, `relocation_cold_targets=138`.

Two limiters remain, and the third is gone:

- **The first entry no longer takes direct authority; it earns it.** The old reading
  was correct for its tree: `native_slice` derived `allow_direct` from `direct_holds`
  and `direct_declined`, both empty on a process's first probe, so the first arm64 run
  took direct mode, which carries no operand resolver, and ended at ~8 instructions on
  `a64_fallback_guard_write=1`. `direct_earned` (`scheduler/pool.rs`) now also requires
  a `direct_modes` entry, which only a completed run creates, so the first run spends
  the resolver. That is what moved the 22-row signature `builds=1 hits=2 sites=1
  entries=1 completed=8` to `builds=36 … completed=258`: commit 271a6e86e, not drift.
  `scheduler.rs::direct_authority_is_earned_by_a_completed_run` pins it. x86 still has
  no direct authority at all — `run_x86_lease` takes no `allow_direct`, `run_x86_inner`
  always passes an operand resolver, and the x86 `RunStatistics` hardcode
  `direct: false` and `direct_guard: false`. Its sixth positional argument is
  `interrupt`, once a literal `false` and now `run.interrupt.is_set()`; it was read as
  `allow_direct` more than once, so read the signature, not the call site.
- **The run still exits on a cold branch target instead of building it.** The surviving
  arm64 gettid run ends at `a64_branch_exhaustion=1`, `a64_branch_cold_relocation=27`.
  amd64 builds through the same targets inside one lease.
- **Re-entry still never happens.** `observe(key) < 2` requires the same
  `(process, generation, version, pc)` to be probed twice, but native is probed at most
  once per 4096-instruction interpreter slice; gettid gets 39 probes over 38 slices and
  38 are `declined_cold`. amd64 escapes this only because, once in, it does not come
  back out.

So fixing the first limiter bought one resolver run per process — ~259 instructions of
one slice out of ~97,000 — and did not make a short program run translated.
`signals/folded-fault-registers` and `signals/implicit-null-pc` still fault interpreted
and do not test the mechanism their sources claim. Making them honest requires changing
the arm64 warm-up gate, which is an admission change and carries the full guard.

`process/sysinfo` is a member of that same arm64 set (arm64 `builds=37 completed=259`,
`interp instructions=16989`), not a separate weak row. The claim that its amd64 side was
complete native coverage was an artefact of a broken counter: `run_x86_slice` built no
`InterpreterTally`, so `hl-interp: instructions=` printed 0 on every amd64 run and amd64
coverage read as a flat 100%. Fixed in 1cb4a1287. sysinfo amd64 now reads
`builds=22 completed=79` against `interp instructions=23818`, so its native share is
under 1%, and gettid amd64 is 25558 native against 107996 interpreted. Never quote an
amd64 `interp instructions=0` from before 1cb4a1287.

Do not read `a64_fallback_form_memory` as a form classification. It is incremented
both by the word classifier and, unconditionally, by the guard-fault path, so it is
at least the guard-fault count by construction and can never be a minority. The
claim that it was 278,672 of 278,672 on sqlite is that identity, not evidence that
the operand-resolver memory path is the whole fallback population.

## Unsafe code

Workspace code forbids unsafe by default.

Unsafe is permitted only in reviewed modules that implement:

- platform system calls;
- the native execution ABI;
- the external C ABI;
- memory mapping and fault entry that cannot be expressed safely.

Every unsafe block states:

1. the validity, lifetime, alignment, and aliasing assumptions;
2. which owner keeps referenced storage alive;
3. why concurrent access is valid;
4. why failure cannot unwind across FFI.

No allocation, lock acquisition, logging, panic, unwinding, or Rust destructor walk
may occur in a signal, VEH, or fork-critical callback.

## Types and ownership

- Make invalid states unrepresentable with constructors, enums, and meaningful
  newtypes.
- Do not wrap primitives or collections without an invariant, identity boundary, or
  cohesive behavior.
- Borrow for observation and transfer ownership for storage.
- Clone only when the ownership model requires independent ownership.
- Use checked arithmetic where overflow is invalid and saturating arithmetic only
  where clamping is the contract.
- Guest-provided lengths, counts, offsets, command batches, and resource requests
  must be bounded before allocation or expensive host work.
- A descriptor, OFD, mapping, task, subscription, provider handle, and translated
  block each have one explicit owner and generation/lifetime model.
- Do not use process-global mutable state for engine instances.

## Errors and Linux behavior

Libraries return typed domain errors. Linux errno conversion happens at the Linux
personality boundary.

Preserve:

- exact `EAGAIN`, `EWOULDBLOCK`, `EINTR`, and partial-I/O behavior;
- shared OFD offsets and descriptor-local flags;
- epoll edge, level, oneshot, timeout, cancellation, and wakeup ordering;
- `SCM_RIGHTS` ownership;
- shared mapping visibility and protection ordering;
- futex deadlines and wakeups;
- fork/exec descriptor, signal, task, and mapping transitions.

Do not panic for guest input or recoverable host failures. No panic or unwind may
cross a C boundary.

## Concurrency and performance

- Avoid global locks across unrelated engines, processes, descriptors, mappings, or
  translated blocks.
- Do not hold table locks across host calls.
- Unrelated OFDs must not serialize.
- Define task ownership, cancellation, shutdown, and wakeup ordering.
- Backpressure blocks or rejects predictably; it never busy-spins.
- Do not log every syscall or translated instruction in normal operation.
- Do not introduce synchronous full-frame, device-wide, or whole-engine waits in a
  hot path.
- Preserve explicit bounds for caches, commands, memory, threads, handles, logs, and
  retained resources.

Every hot-path migration compares against a pinned C baseline. Nested engine
benchmarks measure compounding overhead.

## Application boundaries

`src/containers/hl-engine` is the engine composition root. It owns:

- public configuration and validation;
- CLI and environment capture;
- platform and execution-backend selection;
- concrete adapter construction;
- the supported Rust API;
- the opaque C ABI;
- packaging and target-specific linkage.

The engine wires capabilities and delegates behavior. It must not become the owner of
filesystem, descriptor, syscall, task, or execution algorithms.

`src/apps/husklet` is the product composition root. It owns product configuration,
GUI/CLI behavior, backend selection, and cross-domain orchestration. It must delegate
container, workspace, terminal, filesystem, and engine behavior to their owners rather
than becoming a service locator or god object.

## Tests

- Unit tests live beside the owning source.
- Crate `tests/` exercise only that crate's public contract.
- Repository `tests/` contains multi-package, process, hardware, application, and
  engine-in-engine tests.
- Tests are deterministic, isolated, bounded, and responsible for their resources.
- Fixes begin with a failing behavioral test when feasible.
- Differential tests run the same operation against C and Rust and compare results,
  errno, state, ownership, ordering, and serialized data.

A directory under `src/` must not exist only to aggregate detached test fragments.
When two or more Rust files in a source directory are all test-only, move each test
beside the production noun it exercises and prefer an inline `#[cfg(test)]` module.
Test code must not import behavior or fixtures from a sibling test module. Put
genuinely shared, behavior-free fixtures behind one explicitly declared
`test_support` module owned by the production boundary instead.

Required migration gates are:

1. formatting, design lint, Clippy with warnings denied, unit and documentation
   tests;
2. C/Rust ABI and differential tests;
3. both guest ISA compatibility and production tests;
4. checkpoint and cross-checkpoint;
5. native ARM64 macOS/Linux, AMD64 Linux, and AMD64 Windows target checks;
6. nested engine and performance tests;
7. ordinary container and interactive terminal workflows through Husklet.

### Reproducible Nix driver

`flake.lock` pins the development and verification toolchain. Use the flake as
the repository-level entry point:

```text
nix develop
nix build -L --option cores 0 --max-jobs auto
nix flake check -L --option cores 0 --max-jobs auto
```

Run Clippy and rustfmt inside `nix develop`, or let `nix flake check` run the
complete locked verification derivation. A bare `cargo clippy` on a host whose
`cargo`/`rustc` come from a distribution package but whose
`clippy-driver` comes from Nix fails with `error[E0514]: found crate ... compiled by
an incompatible version of rustc` even though both report the same version string,
because the two builds hash crate metadata differently.

The default shell exposes both Linux guest compilers and the retained
`*_LINUX_CC`, `*_LINUX_STATIC_CC`, `*_DYNAMIC_LOADER`, and `*_DYNAMIC_LIBC`
contracts. Interactive verification must override conservative environment
defaults and size `CARGO_BUILD_JOBS` and `HL_COMPAT_JOBS` to the host's logical
CPU count unless measured RAM, disk, thermal, or lifecycle pressure requires a
lower bound. The named flake checks alias one comprehensive verification
derivation deliberately; use its internal parallelism rather than launching
duplicate full Cargo builds that contend for the same dependency graph.
The derivation must remain offline, locked, warning-strict, and responsible for
format, design lint, lint cases, workspace and documentation tests, and checked
compatibility metadata. Do not reintroduce retained-tree CMake, Ninja,
clang-tidy, or cppcheck command pipelines. Native C compilation is owned by
`hl-native/build.rs`; portable source analysis is embedded in `hl-design-lint`.

## Design lint

`src/packages/hl-design-lint` is the repository architecture linter. Run it in
the pinned shell:

```text
cargo run --locked --offline -q -p hl-design-lint -- --policy lint.toml src tests
cargo run --locked --offline -q -p hl-design-lint -- --policy lint.toml --cases lint src tests
```

It enforces dependency direction and cycles, source ownership, ambient environment
access, platform-command boundaries, catch-all modules, oversized files, ceremonial
structure, and other reviewed design rules.

`lint/errors/` contains unclassified generated findings. `lint/check/` contains
temporarily classified findings. Both are review queues, not suppressions.

`lint/examples/positive.md` contains approved transformations.
`lint/examples/negative.md` contains rejected transformations and their failure
modes. The corpus began from Husklet's reviewed examples; engine-specific decisions
must be added as the rewrite exposes real cases.

### Lint-case protocol

Before resolving a generated lint case, read:

- this entire `AGENTS.md`;
- all of `lint/examples/positive.md`;
- all of `lint/examples/negative.md`;
- the current source, callers, sibling behavior, owning manifest, and nearby tests.

A generated case is evidence and may be stale. Refactor into the correct entity,
package, port, adapter, or inline behavior when ownership is clear. Do not add a
classification, allowance, dependency, wrapper, or empty abstraction merely to
make the queue pass.

Append a positive or negative example only after user approval. Preserve the
reasoning, not only the final code.

## Style

- Use precise nouns and domain vocabulary.
- Avoid `Manager`, `Helper`, `Util`, `Impl`, vague abbreviations, and repeated
  module prefixes.
- A trait or type is already a namespace; method names do not repeat it.
- Every word in a name must earn its place: drop any word the module, type, or
  trait already supplies. There is no word budget, because a name that spells an
  external mechanism — `oom_score_adj`, `write_life_hint`, `unix_socket_path` —
  spends its words on one concept, and that spelling is worth more than brevity.
  Never shorten or paraphrase a name that maps to a kernel, ABI, wire, or vendor
  spelling; the exact match is the documentation.
- Prefer standard conversion, parsing, formatting, and iterator traits when they
  express the complete contract.
- Keep the happy path shallow.
- Public APIs are minimal and document invariants, errors, safety, ownership, and
  non-obvious performance contracts.
- Comments explain contracts and reasons; names explain mechanics.
- Lint allowances are local and justified.
- Delete obsolete implementations after their migration and parity window passes.

## Delivery

Refactor incrementally. Every migration leaves an acyclic package graph and a
working production path. Temporary dependency cycles, permanent compatibility
shims, application-specific engine hacks, and parallel abandoned implementations
are not accepted migration strategies.
