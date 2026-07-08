#!/usr/bin/env python3
# Measure close-freeze time: N panes (slots), each a shell running a foreground sleep, frozen either
# SEQUENTIALLY (old GUI close handler) or CONCURRENTLY (new: spawn all, then join). Reports wall time.
import os, sys, pty, time, select, subprocess, struct, fcntl, termios, signal

WS = "slottest"; DDCLI = "/Users/x/.local/bin/ddcli"; HOME = "/Users/x"
CKPT = f"{HOME}/.dd/workspaces/{WS}/checkpoint"
ENV = {"HOME": HOME, "USER": "x", "LOGNAME": "x", "TERM": "xterm-256color",
       "PATH": "/Users/x/.local/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin", "TMPDIR": "/tmp"}
N = int(sys.argv[1]) if len(sys.argv) > 1 else 3
PROMPT = [b"# ", b"$ ", b":/#"]

def reap():
    subprocess.run("ps -A -o pid,ppid,command | awk '$2==1 && (/ddjit/||/workspace launch/){print $1}' "
                   "| while read p; do kill -9 $p 2>/dev/null; done", shell=True)

def drain(fd, t, until=None):
    buf=b""; end=time.time()+t
    while time.time()<end:
        r,_,_=select.select([fd],[],[],0.1)
        if r:
            try: d=os.read(fd,65536)
            except OSError: break
            if not d: break
            buf+=d
            if until and any(u in buf for u in until): return True
    return False

def spawn(slot):
    pid,fd=pty.fork()
    if pid==0:
        try: os.execvpe(DDCLI,[DDCLI,"workspace","launch",WS,"--slot",slot],ENV)
        except Exception: os._exit(127)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH",40,120,0,0))
    return pid,fd

def launch_panes(slots):
    procs=[]
    for s in slots:
        subprocess.run(["rm","-rf",os.path.join(CKPT,s),os.path.join(CKPT,s+".trigger"),os.path.join(CKPT,s+".pid")])
        pid,fd=spawn(s)
        if not drain(fd,25,PROMPT):
            print(f"  (pane {s} no prompt, retrying)"); os.close(fd)
            try: os.killpg(pid,9); os.waitpid(pid,0)
            except Exception: pass
            pid,fd=spawn(s); drain(fd,25,PROMPT)
        os.write(fd,b"sleep 1000\r"); time.sleep(0.3)
        procs.append((s,pid,fd))
    time.sleep(0.5)
    return procs

def teardown(procs):
    for s,pid,fd in procs:
        try: os.close(fd)
        except OSError: pass
        try: os.killpg(pid,9)
        except OSError: pass
        try: os.waitpid(pid,0)
        except Exception: pass

def freeze_sequential(slots):
    t=time.time()
    for s in slots:
        subprocess.run([DDCLI,"workspace","checkpoint",WS,"--slot",s],env=ENV,
                       stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,timeout=45)
    return time.time()-t

def freeze_concurrent(slots):
    t=time.time()
    kids=[subprocess.Popen([DDCLI,"workspace","checkpoint",WS,"--slot",s],env=ENV,
                           stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL) for s in slots]
    for k in kids: k.wait()
    return time.time()-t

slots=[f"ct{i}" for i in range(N)]
reap()
print(f"Measuring freeze of {N} panes (each a shell + foreground sleep)...")

print("[sequential] launching panes...")
procs=launch_panes(slots);
t_seq=freeze_sequential(slots); teardown(procs)
print(f"[sequential] freeze wall time = {t_seq*1000:.0f} ms")
reap(); time.sleep(1)

print("[concurrent] launching panes...")
procs=launch_panes(slots)
t_con=freeze_concurrent(slots); teardown(procs)
print(f"[concurrent] freeze wall time = {t_con*1000:.0f} ms")
reap()
print(f"\nspeedup: {t_seq/t_con:.2f}x  (sequential {t_seq*1000:.0f}ms -> concurrent {t_con*1000:.0f}ms for {N} panes)")
