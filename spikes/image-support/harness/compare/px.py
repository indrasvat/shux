import zlib,struct,sys
def load(p):
    d=open(p,'rb').read(); i=8; idat=b''
    while i<len(d):
        ln=struct.unpack('>I',d[i:i+4])[0]; t=d[i+4:i+8]; b=d[i+8:i+8+ln]; i+=12+ln
        if t==b'IHDR': w,h,bd,ct=struct.unpack('>IIBB',b[:10])
        elif t==b'IDAT': idat+=b
    raw=zlib.decompress(idat); ch={0:1,2:3,4:2,6:4}[ct]; stride=w*ch
    prev=bytearray(stride); pos=0; rows=[]
    for y in range(h):
        f=raw[pos]; pos+=1; line=bytearray(raw[pos:pos+stride]); pos+=stride
        for x in range(stride):
            a=line[x-ch] if x>=ch else 0; bb=prev[x]; c=prev[x-ch] if x>=ch else 0
            if f==1: line[x]=(line[x]+a)&255
            elif f==2: line[x]=(line[x]+bb)&255
            elif f==3: line[x]=(line[x]+(a+bb)//2)&255
            elif f==4:
                pp=a+bb-c; pa,pb,pc=abs(pp-a),abs(pp-bb),abs(pp-c)
                pr=a if (pa<=pb and pa<=pc) else (bb if pb<=pc else c)
                line[x]=(line[x]+pr)&255
        rows.append(bytes(line)); prev=line
    return w,h,ch,rows
def count(p,rgb):
    w,h,ch,rows=load(p); n=0
    for y in range(h):
        r=rows[y]
        for x in range(w):
            if tuple(r[x*ch:x*ch+3])==tuple(rgb): n+=1
    return n,w,h
