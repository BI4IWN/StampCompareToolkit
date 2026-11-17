"""Python 全流程基线：rapidocr det+cls+rec，用于与 Rust det+rec 对比。"""
import json, os, time
from rapidocr_onnxruntime import RapidOCR

rows = json.load(open('data/detest.json'))
lib = os.path.expanduser('~/Downloads/印章库')
eng = RapidOCR()
out = []
t_all = 0.0
for r in rows:
    p = os.path.join(lib, r['file'])
    t0 = time.time()
    result, _ = eng(p)
    dt = (time.time() - t0) * 1000
    t_all += dt
    text = ' '.join(x[1] for x in result) if result else ''
    tt = {}
    for ch in r['truth']:
        tt[ch] = tt.get(ch, 0) + 1
    hit = 0
    for ch in text:
        if tt.get(ch, 0) > 0:
            tt[ch] -= 1
            hit += 1
    recall = hit / max(1, len(r['truth']))
    out.append({'file': r['file'], 'truth': r['truth'], 'pred': text, 'recall': round(recall, 3), 'boxes': len(result or [])})
    print(f"{len(out)-1:>2} boxes={len(result or []):<3} recall={recall:.2f} {r['truth'][:22]} -> {text[:40]}")
avg = sum(o['recall'] for o in out) / len(out)
json.dump({'engine': 'python rapidocr(det+cls+rec)', 'rows': out, 'images': len(out),
           'avg_ms_per_image': round(t_all / len(out), 1), 'avg_recall': round(avg, 3)},
          open('data/baseline_det_py.json', 'w'), ensure_ascii=False, indent=1)
print(f"\n平均字覆盖 {avg:.3f}  单图 {t_all/len(out):.1f}ms  ({len(out)} 张)")
