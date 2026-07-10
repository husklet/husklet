import subprocess,sys,bisect
RWBASE=0x111aa8000; RXBASE=0x115aa8000; CP=0x112de9784
DELTA=RXBASE-RWBASE
data=open("dump.bin","rb").read()
heads=[]
gpcof={}
for line in open("dump.map"):
    p=line.split()
    if p[0]!="MAP": continue
    gpc=int(p[1],16); host=int(p[2],16); body=int(p[3],16)
    heads.append((host,body,gpc))
heads.sort()
hosts=[h[0] for h in heads]
def region_of(addr):
    i=bisect.bisect_right(hosts,addr)-1
    if i<0: return None
    return heads[i], (heads[i+1][0] if i+1<len(heads) else CP)
def disasm(addr,n=40):
    head,nh=region_of(addr)
    print("### addr 0x%x in region head=0x%x body=0x%x gpc=0x%x size=%d"%(addr,head[0],head[1],head[2],nh-head[0]))
    off=addr-RWBASE; end=min(off+n*4, nh-RWBASE)
    open("_t.bin","wb").write(data[off:end])
    out=subprocess.run(["objdump","-D","-b","binary","-m","aarch64","--adjust-vma=0x%x"%addr,"_t.bin"],capture_output=True,text=True).stdout
    for ln in out.splitlines():
        if ":\t" in ln: print(ln)
for a in sys.argv[1:]:
    disasm(int(a,16))
    print()
