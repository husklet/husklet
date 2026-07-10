import subprocess, struct, sys

RWBASE=0x111aa8000; RXBASE=0x115aa8000; CP=0x112de9784
DELTA=RXBASE-RWBASE
SPIN_RX=0x116c3dd5c
SPIN_RW=SPIN_RX-DELTA
print("SPIN_RW=0x%x  offset_in_cache=0x%x"%(SPIN_RW, SPIN_RW-RWBASE))

# parse map
heads=[]
for line in open("dump.map"):
    p=line.split()
    if p[0]!="MAP": continue
    gpc=int(p[1],16); host=int(p[2],16); body=int(p[3],16)
    heads.append((host,body,gpc))
heads.sort()
# find region head with largest host <= SPIN_RW
import bisect
hosts=[h[0] for h in heads]
i=bisect.bisect_right(hosts, SPIN_RW)-1
head=heads[i]
nexthost=heads[i+1][0] if i+1<len(heads) else CP
print("REGION head: host=0x%x body=0x%x gpc=0x%x  next_head=0x%x  region_size=%d"%(head[0],head[1],head[2],nexthost, nexthost-head[0]))
print("SPIN is at region+0x%x (from head host)"%(SPIN_RW-head[0]))

# load bin
data=open("dump.bin","rb").read()
def rw_to_off(a): return a-RWBASE
start=head[0]; end=nexthost
off0=rw_to_off(start); off1=rw_to_off(end)
chunk=data[off0:off1]
# write raw and disassemble
open("region.bin","wb").write(chunk)
# disassemble with objdump, annotate addresses as RW addrs
out=subprocess.run(["objdump","-D","-b","binary","-m","aarch64","--adjust-vma=0x%x"%start,"region.bin"],capture_output=True,text=True).stdout
# print lines, mark SPIN
lines=out.splitlines()
for ln in lines:
    mark=""
    s=ln.strip()
    if s and s[0:1].isdigit() or (s[:2]=="10" or s[:2]=="11"):
        pass
    if ("%x"%SPIN_RW)[-6:] in ln and ":" in ln:
        mark=" <==== SPIN PC"
    print(ln+mark)
