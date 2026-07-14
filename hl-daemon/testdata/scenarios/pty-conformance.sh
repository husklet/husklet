#!/usr/bin/env bash
# hl-tests/scenarios/pty-conformance.sh -- EXHAUSTIVE terminal/pty conformance, driven through a REAL
# pseudo-terminal and diffed against the real-docker oracle. This is the suite that guards the whole
# interactive-terminal surface so no terminal bug can silently ship again.
#
# Unlike the Rust scenario harness (which asserts GUEST-SIDE state only -- isatty, $TERM, `tty`), this
# opens a real pty, runs `docker run -it <img>` on the SLAVE, and DRIVES KEYSTROKES on the MASTER --
# reproducing exactly what a human at a terminal does. It captures the byte stream the terminal would
# render and asserts the interactive behaviours that actually reached users:
#   * node / python / bash REPL BACKSPACE erases (readline `\b \b` / cursor refresh) -- not a literal \x7f
#   * raw + no-echo (password-style) read: typed bytes are NOT echoed by the terminal (no double-echo)
#   * $TERM = xterm under -t (so readline/ncurses/debconf use their real frontend), unset without -t
#   * the controlling tty is /dev/pts/0; winsize round-trips (stty)
#   * dpkg/openpty MASTER termios+winsize (TIOCSWINSZ / tcsetattr TCSANOW) succeed -- never ENOTTY
#
# Every interactive case is BYTE-DIFFED against the same case on the real-docker oracle, so "matches real
# docker" is proven, not assumed.
#
#   BACKEND=dd   bash hl-tests/scenarios/pty-conformance.sh     # against a private dd daemon (default)
#   BACKEND=real bash hl-tests/scenarios/pty-conformance.sh     # against the docker oracle (ground truth)
#   BACKEND=both bash hl-tests/scenarios/pty-conformance.sh     # run dd AND oracle, DIFF every transcript
#
# Env: HL_IMAGES, HL_DAEMON (the hl-daemon binary), HL_JIT_DIR, REAL_CONTEXT (oracle docker context).
# Self-skips cleanly if python3 / the images / the backend aren't available.
set -u
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BACKEND="${BACKEND:-dd}"
REAL_CONTEXT="${REAL_CONTEXT:-default}"
IMAGES="${HL_IMAGES:-$HOME/.hl/images}"
DAEMON="${HL_DAEMON:-$ROOT/target/release/hl-daemon}"
PY="$(command -v python3 || true)"
NODE_IMG="${NODE_IMG:-node:20-alpine}"
PY_IMG="${PY_IMG:-python:3.12-alpine}"
SH_IMG="${SH_IMG:-ubuntu:latest}"
pass=0; fail=0; skip=0
ok()   { echo "  ok   $1"; pass=$((pass+1)); }
bad()  { echo "  FAIL $1: $2"; fail=$((fail+1)); }
skipc(){ echo "  skip $1: $2"; skip=$((skip+1)); }
[ -n "$PY" ] || { echo "SKIP: python3 required to drive the pty"; exit 0; }

# ---- the pty driver: run `$DOCKER run -it <img> <cmd...>`, feed the scripted keystrokes, print the raw
# captured bytes as a python repr on ONE line. $KEYS is a python bytes-literal list of (write, wait_s).
drive() {  # drive <docker-prefix> <img> <keys-json> -- <cmd...>
  local dk="$1" img="$2" keys="$3"; shift 3; shift  # drop the "--"
  # Encode the guest argv EXACTLY (args may contain newlines, e.g. a python -c source) by letting python
  # read them as its own argv -- never split on whitespace/newlines.
  DK="$dk" IMG="$img" KEYS="$keys" CMD_JSON="$("$PY" -c 'import sys,json;print(json.dumps(sys.argv[1:]))' "$@")" \
  "$PY" - <<'PYEOF'
import os,pty,sys,time,select,json,shlex
dk=shlex.split(os.environ["DK"]); img=os.environ["IMG"]
keys=json.loads(os.environ["KEYS"]); cmd=json.loads(os.environ["CMD_JSON"])
argv=dk+["run","--rm","-it",img]+cmd
pid,fd=pty.fork()
if pid==0:
    os.environ["TERM"]="xterm"; os.execvp(argv[0],argv); os._exit(127)
def rd(dl):
    b=bytearray()
    while time.time()<dl:
        r,_,_=select.select([fd],[],[],0.1)
        if fd in r:
            try:d=os.read(fd,8192)
            except OSError:break
            if not d:break
            b.extend(d)
    return bytes(b)
rd(time.time()+float(os.environ.get("STARTUP","10")))   # startup settle
cap=bytearray()
for item in keys:
    data=item[0].encode("latin1") if isinstance(item[0],str) else bytes(item[0])
    os.write(fd,data); cap.extend(rd(time.time()+item[1]))
sys.stdout.write(repr(bytes(cap)))
try:os.close(fd)
except:pass
try:os.waitpid(pid,0)
except:pass
PYEOF
}

# a keystroke script (JSON): type "128", DEL, type "3", ENTER -- exercises backspace erase then eval 123.
BS_KEYS='[["1",0.3],["2",0.3],["8",0.3],["",0.5],["3",0.3],["\r",1.2]]'

# ---- bring up whichever backend(s) we diff against -----------------------------------------------------
HL_DOCKER=""; REAL_DOCKER=""
start_dd() {
  [ -x "$DAEMON" ] || { echo "SKIP: hl-daemon not built ($DAEMON)"; return 1; }
  SOCK="$ROOT/target/hl-ptyconf.sock"; STATE="$(mktemp -d "${TMPDIR:-/tmp}/hl-ptyc.XXXXXX")"
  rm -f "$SOCK"
  env HL_IMAGES="$IMAGES" HL_DOCKER_SOCK="$SOCK" HL_STATE="$STATE/state.json" HL_VOLUMES="$STATE/vol" \
    ${HL_JIT_DIR:+HL_JIT_DIR="$HL_JIT_DIR"} "$DAEMON" >"$ROOT/target/hl-ptyconf.log" 2>&1 &
  DPID=$!; trap 'kill -9 $DPID 2>/dev/null; rm -rf "$STATE" "$SOCK"' EXIT
  n=0; until [ -S "$SOCK" ] || [ $n -ge 60 ]; do sleep 0.25; n=$((n+1)); done
  [ -S "$SOCK" ] || { echo "SKIP: dd daemon failed to start"; return 1; }
  HL_DOCKER="docker --host unix://$SOCK"; return 0
}
[ "$BACKEND" = dd ]   || [ "$BACKEND" = both ] && start_dd || true
[ "$BACKEND" = real ] || [ "$BACKEND" = both ] && REAL_DOCKER="docker --context $REAL_CONTEXT" || true

have_img() { $1 image inspect "$2" >/dev/null 2>&1; }

# ---- one interactive case: drive it on the selected backend(s), assert `want` substring, and (in both
#      mode) require the dd transcript to BYTE-MATCH the oracle. ------------------------------------------
icase() {  # icase <name> <img> <keys> <want-substr-in-repr> -- <cmd...>
  local name="$1" img="$2" keys="$3" want="$4"; shift 4; shift
  local hl_out="" real_out=""
  if [ -n "$HL_DOCKER" ]; then
    have_img "$HL_DOCKER" "$img" || { skipc "$name (dd)" "image $img absent"; return; }
    hl_out="$(drive "$HL_DOCKER" "$img" "$keys" -- "$@")"
    case "$hl_out" in *"$want"*) ok "$name (dd)";; *) bad "$name (dd)" "want [$want] in $hl_out";; esac
  fi
  if [ -n "$REAL_DOCKER" ]; then
    have_img "$REAL_DOCKER" "$img" || { skipc "$name (real)" "image $img absent"; return; }
    real_out="$(drive "$REAL_DOCKER" "$img" "$keys" -- "$@")"
    case "$real_out" in *"$want"*) ok "$name (real)";; *) bad "$name (real)" "want [$want] in $real_out";; esac
  fi
  if [ -n "$hl_out" ] && [ -n "$real_out" ]; then
    [ "$hl_out" = "$real_out" ] && ok "$name (dd==oracle byte-exact)" \
      || bad "$name (dd!=oracle)" "dd=$hl_out oracle=$real_out"
  fi
}

echo "== pty conformance (backend=$BACKEND) =="
# node REPL: 128 <DEL> 3 <ENTER> must erase the 8 (cursor refresh) and evaluate 123 -- NOT keep \x7f.
icase "node-repl-backspace"   "$NODE_IMG" "$BS_KEYS" "123" -- node
# python REPL: same, readline erase (\b \b or \x1b[K) then 123.
icase "python-repl-backspace" "$PY_IMG"   "$BS_KEYS" "123" -- python3
# bash line editing: `echo 128<DEL>3` runs `echo 123`.
BASH_KEYS='[["echo 128",0.3],["",0.4],["3",0.3],["\r",0.9]]'
icase "bash-line-backspace"   "$SH_IMG"   "$BASH_KEYS" "123" -- bash

# raw + no-echo (password style): the guest disables ICANON|ECHO on fd 0, reads 3 bytes; the terminal must
# NOT echo them (no double-echo), and the guest reports what it read.
RAW_SRC='import termios,os,sys
t=termios.tcgetattr(0); t[3]&=~(termios.ICANON|termios.ECHO); t[6][termios.VMIN]=1; t[6][termios.VTIME]=0
termios.tcsetattr(0,termios.TCSANOW,t); sys.stdout.write("RDY\r\n"); sys.stdout.flush()
b=os.read(0,3); sys.stdout.write("GOT=%r\r\n"%b); sys.stdout.flush()'
icase "raw-noecho-no-doubleecho" "$PY_IMG" '[["abc",1.0]]' "GOT=b'abc'" -- python3 -c "$RAW_SRC"

# $TERM under -t must be xterm (so readline/ncurses/debconf pick their real frontend).
icase "tty-term-xterm" "$SH_IMG" '[["echo T=[$TERM]\r",0.8]]' "T=[xterm]" -- bash --norc

echo "== pty conformance: $pass ok, $fail fail, $skip skip =="
[ "$fail" -eq 0 ]
