import os, pty, sys, time, select, subprocess

DDCLI = "/Users/x/dd/dd/target-mac/release/ddcli"
WS = "slottest"
HOME = os.environ.get("HOME", "/Users/x")
STORAGE = f"{HOME}/.dd/workspaces/{WS}"
CLEANENV = {
    "TERM": "xterm-256color",
    "PATH": "/Users/x/.local/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin",
    "HOME": HOME,
}

def spawn(args):
    pid, fd = pty.fork()
    if pid == 0:
        try:
            os.execve(args[0], args, CLEANENV)
        except Exception as e:
            os.write(2, ("execve failed: %r" % e).encode())
            os._exit(127)
    return pid, fd

def drain(fd, secs):
    out = b""
    end = time.time() + secs
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.3)
        if fd in r:
            try:
                d = os.read(fd, 65536)
            except OSError:
                break
            if not d:
                break
            out += d
    return out.decode(errors="replace")

def send(fd, s):
    os.write(fd, s.encode())

pid, fd = spawn([DDCLI, "workspace", "launch", WS, "--slot", "insp"])
print("launched pid", pid)
banner = drain(fd, 12)
print("=== banner ===")
print(banner[-1500:])
send(fd, "touch /root/HELLO_HOST; echo TOUCHED_$?\n")
print("=== after touch ===")
print(drain(fd, 5)[-600:])
# find on host
r = subprocess.run(["bash", "-c", f"find {STORAGE} -name 'HELLO_HOST' 2>/dev/null; echo ---; ls -la {STORAGE}"], capture_output=True, text=True)
print("=== host find ===")
print(r.stdout)
# cleanup: kill the process group
try:
    os.killpg(pid, 9)
except Exception as e:
    print("kill err", e)
os.close(fd)
