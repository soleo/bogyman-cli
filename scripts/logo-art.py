#!/usr/bin/env python3
"""Render bogyman.svg as terminal art for `bogyman version`.

1. Rasterize the SVG to a darkness grid. Any browser works; in Chrome's console
   on https://soleo.github.io/bogyman-web/ draw the SVG onto a 240px-wide canvas,
   read getImageData, and save rows of digits 0-9 (0 = white, 9 = black) as JSON:
   {"rows": ["000...", ...]}.
2. python3 scripts/logo-art.py grid.json braille 24 14 4
   modes: braille <cols> <rows> [threshold], half <cols> <rows> [threshold],
   ascii <cols> <rows>. The current ART in version.rs is braille 24 14 4.
"""
import json, sys
src = sys.argv.pop(1)
rows = json.load(open(src))
rows = rows['rows'] if isinstance(rows, dict) else rows
H=len(rows); W=len(rows[0])
g=[[int(c) for c in r] for r in rows]
# crop to bounding box of ink
ys=[y for y in range(H) if any(v>2 for v in g[y])]; xs=[x for x in range(W) if any(g[y][x]>2 for y in range(H))]
y0,y1,x0,x1=min(ys),max(ys)+1,min(xs),max(xs)+1
g=[r[x0:x1] for r in g[y0:y1]]; H=len(g); W=len(g[0])
def sample(cols, rows_, pool='max'):
    # resample g to cols x rows_ with pooling
    out=[]
    for j in range(rows_):
        ya=int(j*H/rows_); yb=max(ya+1,int((j+1)*H/rows_))
        row=[]
        for i in range(cols):
            xa=int(i*W/cols); xb=max(xa+1,int((i+1)*W/cols))
            vals=[g[y][x] for y in range(ya,yb) for x in range(xa,xb)]
            row.append(max(vals) if pool=='max' else sum(vals)/len(vals))
        out.append(row)
    return out
def braille(cols, rows_, thr=4):
    px=sample(cols*2, rows_*4)
    lines=[]
    for j in range(rows_):
        s=''
        for i in range(cols):
            bits=0
            dots=[(0,0,0x01),(0,1,0x02),(0,2,0x04),(1,0,0x08),(1,1,0x10),(1,2,0x20),(0,3,0x40),(1,3,0x80)]
            for dx,dy,b in dots:
                if px[j*4+dy][i*2+dx]>=thr: bits|=b
            s+=chr(0x2800+bits)
        lines.append(s.rstrip())
    return lines
def ascii_ramp(cols, rows_):
    px=sample(cols, rows_, 'avg')
    ramp=" .:-=+*#%@"
    return [''.join(ramp[min(9,int(v/9*9.99))] for v in r).rstrip() for r in px]
def halfblock(cols, rows_, thr=4):
    px=sample(cols, rows_*2)
    lines=[]
    for j in range(rows_):
        s=''
        for i in range(cols):
            t=px[j*2][i]>=thr; b=px[j*2+1][i]>=thr
            s+= '█' if t and b else '▀' if t else '▄' if b else ' '
        lines.append(s.rstrip())
    return lines
mode=sys.argv[1]; cols=int(sys.argv[2]); rows_=int(sys.argv[3]); thr=int(sys.argv[4]) if len(sys.argv)>4 else 4
f={'braille':braille,'ascii':ascii_ramp,'half':halfblock}[mode]
out=f(cols,rows_,thr) if mode!='ascii' else f(cols,rows_)
print('\n'.join(out))
