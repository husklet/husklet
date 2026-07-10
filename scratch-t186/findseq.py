import sys,struct
def words(fn):
    d=open(fn,"rb").read()
    return d, len(d)//4
def find(fn, seq):
    d=open(fn,"rb").read()
    res=[]
    n=len(d)//4
    W=[struct.unpack_from("<I",d,i*4)[0] for i in range(n)]
    for i in range(n-len(seq)):
        ok=True
        for j,s in enumerate(seq):
            if s is not None and W[i+j]!=s: ok=False;break
        if ok: res.append(i)
    return res,W
# seq: ldp x3,x2,[x29,#-56]; cmp x2,#0xc ; then branch (unknown)
seq=[0xa97c8ba3, 0xf100305f, None]
for tag,fn in [("STITCH","dump.bin"),("NOSTITCH","ns.bin")]:
    res,W=find(fn,seq)
    print("=== %s %s: %d matches ==="%(tag,fn,len(res)))
    for i in res[:6]:
        # print this word and next 3 (the branch and follow)
        print("  off=0x%x: %08x %08x %08x %08x"%(i*4, W[i],W[i+1],W[i+2],W[i+3]))
