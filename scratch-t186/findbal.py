import struct
def load(fn):
    d=open(fn,"rb").read(); n=len(d)//4
    return [struct.unpack_from("<I",d,i*4)[0] for i in range(n)]
def scan(fn, condval):
    W=load(fn); hits=[]
    for i in range(len(W)-1):
        if W[i]==0xf9000060 and (W[i+1]&0xFF00001F)==(0x54000000|condval):
            hits.append((i, W[i+1]))
    return hits
for tag,fn,c,name in [("STITCH","dump.bin",0x0F,"b.nv"),("NOSTITCH","ns.bin",0x0E,"b.al"),
                      ("STITCH-any-bal","dump.bin",0x0E,"b.al"),("NOSTITCH-any-bnv","ns.bin",0x0F,"b.nv")]:
    h=scan(fn,c)
    print("%-16s str x0,[x3] + %s : %d hits"%(tag,name,len(h)), [(hex(i*4),hex(w)) for i,w in h[:5]])
# also: count ALL emitted b.nv (cond 0xF) vs b.al (cond 0xE) branches in each
for tag,fn in [("STITCH","dump.bin"),("NOSTITCH","ns.bin")]:
    W=load(fn)
    nv=sum(1 for w in W if (w&0xFF000010)==0x54000000 and (w&0xF)==0xF)
    al=sum(1 for w in W if (w&0xFF000010)==0x54000000 and (w&0xF)==0xE)
    print("%-10s total emitted b.nv=%d  b.al=%d"%(tag,nv,al))
