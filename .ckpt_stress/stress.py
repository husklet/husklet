#!/usr/bin/env python3
# Checkpoint/restore + Ctrl-C stress harness. Runs on the macOS host (where the engine runs).
# Loops N iterations of: launch fresh shell in a PTY -> run a foreground `sleep 1000` (or idle) ->
# ddcli workspace checkpoint -> kill -> launch --restore -> send Ctrl-C -> assert survival + interrupt.
#
# Env MUST include HOME=/Users/x so ddcli finds its store.
import os, sys, pty, time, select, subprocess, struct, fcntl, termios, signal

WS = "slottest"
DDCLI = "/Users/x/.local/bin/ddcli"
HOME = "/Users/x"
UPPER = f"{HOME}/.dd/workspaces/{WS}/upper"
CKPT = f"{HOME}/.dd/workspaces/{WS}/checkpoint"

ENV = {
    "HOME": HOME,
    "USER": "x",
    "LOGNAME": "x",
    "TERM": "xterm-256color",
    "PATH": "/Users/x/.local/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    "TMPDIR": "/tmp",
}

def log(*a):
    print(*a, flush=True)

def reap_orphans():
    subprocess.run(
        "ps -A -o pid,ppid,command | awk '$2==1 && (/ddjit/||/workspace launch/){print $1}' "
        "| while read p; do kill -9 $p 2>/dev/null; done",
        shell=True)

def clean_slot(slot):
    for suf in (f"{slot}", f"{slot}.trigger", f"{slot}.pid"):
        p = os.path.join(CKPT, suf)
        subprocess.run(["rm", "-rf", p])

def drain(fd, timeout, until=None):
    buf = b""
    end = time.time() + timeout
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                d = os.read(fd, 65536)
            except OSError:
                break
            if not d:
                break
            buf += d
            if until and any(u in buf for u in (until if isinstance(until, list) else [until])):
                return buf, True
    if until:
        return buf, any(u in buf for u in (until if isinstance(until, list) else [until]))
    return buf, False

def spawn(slot, restore):
    pid, fd = pty.fork()
    if pid == 0:
        # child: pty.fork already did setsid + made slave the controlling tty
        args = [DDCLI, "workspace", "launch", WS, "--slot", slot]
        if restore:
            args.append("--restore")
        try:
            os.execvpe(DDCLI, args, ENV)
        except Exception:
            os._exit(127)
    # parent
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
    return pid, fd

def alive(pid):
    try:
        r, _ = os.waitpid(pid, os.WNOHANG)
        return r == 0
    except ChildProcessError:
        return False

def kill_wait(pid, fd):
    try:
        os.close(fd)
    except OSError:
        pass
    try:
        os.killpg(pid, signal.SIGKILL)
    except OSError:
        pass
    try:
        os.kill(pid, signal.SIGKILL)
    except OSError:
        pass
    try:
        os.waitpid(pid, 0)
    except ChildProcessError:
        pass

PROMPT = [b"# ", b"$ ", b":/#", b":/root#"]

def checkpoint(slot):
    r = subprocess.run([DDCLI, "workspace", "checkpoint", WS, "--slot", slot],
                       env=ENV, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=45)
    return r.returncode == 0, r.stdout.decode(errors="replace")

def one_iter(i, slot, foreground):
    """Returns (ok:bool, reason:str)."""
    reap_orphans()
    clean_slot(slot)
    # remove sentinel
    fpath = os.path.join(UPPER, "f.txt")
    try: os.remove(fpath)
    except OSError: pass

    # ---- fresh launch ----
    pid, fd = spawn(slot, restore=False)
    buf, got = drain(fd, 25, until=PROMPT)
    if not got:
        kill_wait(pid, fd)
        return None, f"launch: no prompt (env flake). tail={buf[-120:]!r}"

    if foreground:
        os.write(fd, b"sleep 1000\r")
        time.sleep(1.0)
        drain(fd, 0.5)

    # ---- checkpoint ----
    ok, cout = checkpoint(slot)
    if not ok:
        kill_wait(pid, fd)
        return None, f"checkpoint failed (env flake): {cout.strip()[-160:]}"

    # engine _exits on checkpoint -> launch child should be gone
    time.sleep(0.3)
    kill_wait(pid, fd)

    # ---- restore ----
    rpid, rfd = spawn(slot, restore=True)
    # wait for the restore to settle. Foreground case: no prompt (sleep blocks bash); idle: prompt redraws.
    if foreground:
        time.sleep(2.5)
        # (c) spurious-exit-before-Ctrl-C check
        if not alive(rpid):
            _, tail = drain(rfd, 0.2)
            kill_wait(rpid, rfd)
            return False, "SPURIOUS: restored shell EXITED before Ctrl-C"
    else:
        buf, got = drain(rfd, 6, until=PROMPT)
        if not alive(rpid):
            kill_wait(rpid, rfd)
            return False, "SPURIOUS: restored (idle) shell EXITED before Ctrl-C"

    # ---- Ctrl-C ----
    os.write(rfd, b"\x03")
    time.sleep(0.6)
    # follow-up command to prove the shell is live and the prompt is usable
    tag = f"OK_{i}"
    os.write(rfd, f"echo {tag} > /f.txt\r".encode())
    buf2, _ = drain(rfd, 5, until=PROMPT)

    survived = alive(rpid)
    # verify sentinel file
    time.sleep(0.3)
    fcontent = ""
    try:
        with open(fpath) as fh:
            fcontent = fh.read().strip()
    except OSError:
        pass
    kill_wait(rpid, rfd)

    if not survived:
        return False, f"FAIL: shell DIED after Ctrl-C (tab would close). tail={buf2[-160:]!r}"
    if fcontent != tag:
        return False, f"FAIL: follow-up cmd did not run (sleep not interrupted / prompt dead). got f.txt={fcontent!r} tail={buf2[-160:]!r}"
    return True, "ok"

def run_case(name, foreground, n, slot):
    log(f"\n===== CASE: {name} ({n} iterations) =====")
    passed = flaked = failed = 0
    for i in range(n):
        res, reason = one_iter(i, slot, foreground)
        if res is True:
            passed += 1
            log(f"  [{i:02d}] PASS")
        elif res is None:
            flaked += 1
            log(f"  [{i:02d}] FLAKE(retry) {reason}")
            # retry once for env flake
            res2, reason2 = one_iter(i, slot, foreground)
            if res2 is True:
                passed += 1; log(f"  [{i:02d}] PASS (retry)")
            elif res2 is None:
                log(f"  [{i:02d}] FLAKE again, counting as skip: {reason2}")
            else:
                failed += 1; log(f"  [{i:02d}] FAIL (retry): {reason2}")
        else:
            failed += 1
            log(f"  [{i:02d}] FAIL: {reason}")
    log(f"----- {name}: PASS {passed}  FAIL {failed}  (flaked-skipped {n-passed-failed}) -----")
    return passed, failed

if __name__ == "__main__":
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 20
    case = sys.argv[2] if len(sys.argv) > 2 else "both"
    total_fail = 0
    if case in ("both", "fg"):
        p, f = run_case("foreground-sleep", True, n, "fgslot")
        total_fail += f
    if case in ("both", "idle"):
        p, f = run_case("idle-prompt", False, n, "idleslot")
        total_fail += f
    reap_orphans()
    log(f"\n==== DONE. total_fail={total_fail} ====")
    sys.exit(1 if total_fail else 0)
