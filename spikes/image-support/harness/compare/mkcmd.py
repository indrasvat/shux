import zlib, base64, sys
def cmd(w,h,rgb,iid=1,chunk=4096):
    px=bytes(list(rgb)+[255])*(w*h)
    p=base64.b64encode(zlib.compress(px)).decode()
    cs=[p[i:i+chunk] for i in range(0,len(p),chunk)]
    out=[]
    for i,c in enumerate(cs):
        more=1 if i<len(cs)-1 else 0
        ctrl=(f"a=T,f=32,o=z,s={w},v={h},t=d,i={iid},p=1,C=1,q=2,m={more}" if i==0 else f"m={more}")
        out.append(f"\\033_G{ctrl};{c}\\033\\\\")
    return "".join(out)
def delete(what="A",iid=None):
    k=f"a=d,d={what}" + (f",i={iid}" if iid else "") + ",q=2"
    return f"\\033_G{k}\\033\\\\"
if __name__=="__main__":
    import json; a=json.loads(sys.argv[1])
    print(cmd(**a) if a.get("kind")!="del" else delete(a.get("what","A"),a.get("iid")))
