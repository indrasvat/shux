import sys, zlib, struct
def load(p):
    d=open(p,'rb').read(); pos=8; w=h=None; idat=b''; ct=6
    while pos < len(d):
        ln=struct.unpack('>I', d[pos:pos+4])[0]; typ=d[pos+4:pos+8]
        if typ==b'IHDR': w,h,bd,ct=struct.unpack('>IIBB', d[pos+8:pos+18])
        elif typ==b'IDAT': idat+=d[pos+8:pos+8+ln]
        pos+=12+ln
    raw=zlib.decompress(idat); bpp=4 if ct==6 else 3; stride=w*bpp
    out=bytearray(); prev=bytearray(stride); i=0
    for _ in range(h):
        f=raw[i]; i+=1; line=bytearray(raw[i:i+stride]); i+=stride
        for x in range(stride):
            a=line[x-bpp] if x>=bpp else 0; b=prev[x]; c=prev[x-bpp] if x>=bpp else 0
            if f==1: line[x]=(line[x]+a)&255
            elif f==2: line[x]=(line[x]+b)&255
            elif f==3: line[x]=(line[x]+(a+b)//2)&255
            elif f==4:
                p=a+b-c; pa,pb,pc=abs(p-a),abs(p-b),abs(p-c)
                pr=a if (pa<=pb and pa<=pc) else (b if pb<=pc else c)
                line[x]=(line[x]+pr)&255
        out+=line; prev=line
    return w,h,bpp,bytes(out)
# HN's header bar is a wide band of #ff6600. mturk has none.
def hn_orange(path):
    w,h,bpp,px=load(path); n=0
    for y in range(0,h,2):
        base=y*w*bpp
        for x in range(0,w,2):
            o=base+x*bpp
            if px[o]>235 and 85<px[o+1]<125 and px[o+2]<40: n+=1
    return n
for p in sys.argv[1:]:
    n=hn_orange(p)
    print(f"{n:7d} orange px  {'HN' if n>500 else 'NOT HN'}   {p}")

def urlbar_diff(a_path, b_path):
    wa,ha,bpa,pa = load(a_path); wb,hb,bpb,pb = load(b_path)
    if (wa,ha) != (wb,hb): return -1
    n=0
    for y in range(4, 26):                 # the URL-bar strip only
        for x in range(0, wa, 2):
            oa=y*wa*bpa+x*bpa; ob=y*wb*bpb+x*bpb
            if abs(pa[oa]-pb[ob])>40 or abs(pa[oa+1]-pb[ob+1])>40: n+=1
    return n
