#!/bin/bash
set -uo pipefail
export DISPLAY=:99
Xvfb :99 -screen 0 1000x800x24 -nolisten tcp >/dev/null 2>&1 & XV=$!
sleep 2
python3 - > /tmp/kp3.sh <<'PY'
import base64
W,H=720,456
rgba=bytes([0,220,120,255])*(W*H)
b64=base64.b64encode(rgba).decode()
def emit(out, cid, size, row):
    cs=[b64[i:i+size] for i in range(0,len(b64),size)]
    out.append('printf "\\033[%d;2H"'%row)
    out.append('printf "\\033_Ga=t,q=2,f=32,t=d,i=%d,s=%d,v=%d,m=1;%s\\033\\\\\\\\"'%(cid,W,H,cs[0]))
    for k,c in enumerate(cs[1:],1):
        out.append('printf "\\033_Gq=2,m=%d;%s\\033\\\\\\\\"'%(0 if k==len(cs)-1 else 1,c))
    out.append('printf "\\033_Ga=p,q=2,i=%d,p=1,x=0,y=0,w=%d,h=%d,c=30,r=8,z=0,C=1\\033\\\\\\\\"'%(cid,W,H))
out=[]
emit(out, 2001, 4096, 2)    # kitty's stated max
emit(out, 2002, 3800, 12)   # what my working test used
out.append('sleep 10')
print('\n'.join(out))
PY
LIBGL_ALWAYS_SOFTWARE=1 kitty --config NONE -o font_size=11 \
  -o initial_window_width=980 -o initial_window_height=780 \
  -e bash /tmp/kp3.sh >/dev/null 2>&1 & KP=$!
sleep 8
import -window root -display :99 /tmp/kproto3.png 2>/dev/null
kill $KP 2>/dev/null; sleep 1; kill -9 $KP 2>/dev/null
kill $XV 2>/dev/null; sleep 1; kill -9 $XV 2>/dev/null
echo "bytes: $(stat -c%s /tmp/kproto3.png)"
