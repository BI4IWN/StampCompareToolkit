import os, json, math, re
import numpy as np
from PIL import Image

LIB = os.path.expanduser('~/Downloads/印章库')
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'data')
os.makedirs(os.path.join(OUT, 'strips'), exist_ok=True)

files = sorted(f for f in os.listdir(LIB) if re.search(r'\.(png|jpe?g)$', f, re.I))
picks = [files[i] for i in range(0, len(files), max(1, len(files)//20))][:20]

def red_mask(im):
    a = np.asarray(im.convert('RGB')).astype(np.int16)
    r, g, b = a[:, :, 0], a[:, :, 1], a[:, :, 2]
    m = (r > 90) & (r - g > 40) & (r - b > 40)
    return m

def unwrap(im, H=120, W=2000):
    m = red_mask(im)
    ys, xs = np.nonzero(m)
    if len(xs) < 500:
        return None
    cx, cy = xs.mean(), ys.mean()
    d = np.hypot(xs - cx, ys - cy)
    R = np.percentile(d, 99.2)
    r0, r1 = 0.42 * R, 0.905 * R
    ang = np.linspace(0, 2 * np.pi, W, endpoint=False)
    rad = np.linspace(r0, r1, H)
    A, Rr = np.meshgrid(ang, rad)
    X = (cx + Rr * np.cos(A)).round().astype(int).clip(0, im.width - 1)
    Y = (cy + Rr * np.sin(A)).round().astype(int).clip(0, im.height - 1)
    rgb = np.asarray(im.convert('RGB')).astype(np.uint8)
    # 红通道强度当成"墨"，输出黑底白字条带（两路极性都存）
    ink = (rgb[:, :, 0].astype(np.float32) - rgb[:, :, 1].astype(np.float32)).clip(0, 255)
    strip = ink[Y, X]
    strip = (strip / max(1.0, strip.max()) * 255).astype(np.uint8)
    # 极坐标展开后文字是上下翻的，翻回来才是正读方向
    return strip[::-1]

manifest = []
for idx, f in enumerate(picks):
    im = Image.open(os.path.join(LIB, f))
    strip = unwrap(im)
    if strip is None:
        continue
    truth = re.sub(r'(_\d+.*)?\.(png|jpe?g)$', '', f, flags=re.I)
    truth = re.sub(r'[^\u4e00-\u9fa5A-Za-z0-9]', '', truth)
    q = strip.shape[1] // 4
    for k in range(4):
        part = strip[:, k*q:(k+1)*q]
        p = os.path.join(OUT, 'strips', f'{idx:02d}_{k}.png')
        Image.fromarray(part).save(p)
    manifest.append({'i': idx, 'file': f, 'truth': truth})
json.dump(manifest, open(os.path.join(OUT, 'manifest.json'), 'w'), ensure_ascii=False, indent=1)
print('strips:', len(manifest) * 4, 'stamps:', len(manifest))
print(manifest[0])
