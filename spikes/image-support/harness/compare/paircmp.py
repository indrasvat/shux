import sys, glob, struct
exec(open('/tmp/verdict.py').read().split("# HN's header bar")[0])   # provides load()

def grab(path, crop=None, n=48):
    w,h,bpp,px = load(path)
    x0,y0,x1,y1 = crop or (0,0,w,h)
    out=[]
    for j in range(n):
        for i in range(n):
            x = x0 + (x1-x0)*i//n
            y = y0 + (y1-y0)*j//n
            o = y*w*bpp + x*bpp
            out.append((px[o],px[o+1],px[o+2]))
    return out

def dims(p):
    return struct.unpack('>II', open(p,'rb').read()[16:24])

run = sys.argv[1]
ts = sorted(glob.glob(f'/tmp/sync2/{run}/t*.png'))
as_ = sorted(glob.glob(f'/tmp/sync2/{run}/a*.png'))
bad = 0
for i,(t,a) in enumerate(zip(ts,as_),1):
    tw,th = dims(t); aw,ah = dims(a)
    # attach frame carries shux chrome: 1px border ring + status bar row
    inner = (9, 19, aw-9, ah-38)
    A = grab(t); B = grab(a, inner)
    d = sum(abs(p[0]-q[0])+abs(p[1]-q[1])+abs(p[2]-q[2]) for p,q in zip(A,B))/(len(A)*3)
    flag = '' if d < 12 else '   <== DIVERGED'
    if d >= 12: bad += 1
    print(f'  {i:2d}  mean|diff| = {d:6.2f}{flag}')
print(f'\n{run}: {bad} of {len(ts)} pairs diverged')
