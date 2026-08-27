#!/bin/bash
set -uo pipefail
export DISPLAY=:99
Xvfb :99 -screen 0 900x700x24 -nolisten tcp >/dev/null 2>&1 & XV=$!
sleep 2
python3 - > /tmp/kproto_payload.sh <<'PY'
import base64
W,H=120,60
rgba=bytes([0,220,120,255])*(W*H)
b64=base64.b64encode(rgba).decode()
def chunks(s,n=3800):
    return [s[i:i+n] for i in range(0,len(s),n)]
out=[]
def emit_transmit(i):
    cs=chunks(b64); out.append('printf "\\033_Ga=t,q=2,f=32,t=d,i=%d,s=%d,v=%d,m=%d;%s\\033\\\\\\\\"' % (i,W,H,1 if len(cs)>1 else 0,cs[0]))
    for k,c in enumerate(cs[1:],1):
        out.append('printf "\\033_Gq=2,m=%d;%s\\033\\\\\\\\"' % (0 if k==len(cs)-1 else 1, c))
# case 1: a=p with NO c/r  (Zellij's form)
out.append('printf "\\033[2;3H"')
emit_transmit(1001)
out.append('printf "\\033_Ga=p,q=2,i=1001,p=1,x=0,y=0,w=%d,h=%d,z=0,C=1\\033\\\\\\\\"' % (W,H))
# case 2: a=p WITH c/r
out.append('printf "\\033[12;3H"')
emit_transmit(1002)
out.append('printf "\\033_Ga=p,q=2,i=1002,p=1,x=0,y=0,w=%d,h=%d,c=14,r=6,z=0,C=1\\033\\\\\\\\"' % (W,H))
# case 3: plain a=T (known good)
out.append('printf "\\033[22;3H"')
cs=chunks(b64)
out.append('printf "\\033_Ga=T,f=32,t=d,i=1003,s=%d,v=%d,q=2,m=%d;%s\\033\\\\\\\\"' % (W,H,1 if len(cs)>1 else 0,cs[0]))
for k,c in enumerate(cs[1:],1):
    out.append('printf "\\033_Gm=%d;%s\\033\\\\\\\\"' % (0 if k==len(cs)-1 else 1, c))
out.append('sleep 12')
print('\n'.join(out))
PY
chmod 755 /tmp/kproto_payload.sh
LIBGL_ALWAYS_SOFTWARE=1 kitty --config NONE -o font_size=11 \
  -o initial_window_width=880 -o initial_window_height=680 \
  -e bash /tmp/kproto_payload.sh >/dev/null 2>&1 & KP=$!
sleep 9
import -window root -display :99 /tmp/kproto.png 2>/dev/null
kill $KP 2>/dev/null; sleep 1; kill -9 $KP 2>/dev/null
kill $XV 2>/dev/null; sleep 1; kill -9 $XV 2>/dev/null
echo "captured $(stat -c%s /tmp/kproto.png 2>/dev/null || echo MISSING) bytes"
