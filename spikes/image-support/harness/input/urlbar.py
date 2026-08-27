import sys
src = open('/tmp/verdict.py').read().split('# HN\'s header bar')[0]
exec(src)
def urlbar_diff(a_path, b_path):
    wa,ha,bpa,pa = load(a_path); wb,hb,bpb,pb = load(b_path)
    if (wa,ha) != (wb,hb): return -1
    n=0
    for y in range(4, 26):
        for x in range(0, wa, 2):
            oa=y*wa*bpa+x*bpa; ob=y*wb*bpb+x*bpb
            if abs(pa[oa]-pb[ob])>40 or abs(pa[oa+1]-pb[ob+1])>40: n+=1
    return n
if __name__ == '__main__':
    base = sys.argv[1]
    for f in sys.argv[2:]:
        print(f"{urlbar_diff(base,f):6d}  url-bar px changed vs baseline   {f}")
