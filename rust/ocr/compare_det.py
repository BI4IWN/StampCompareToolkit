"""对比 Rust(det+rec) 与 Python(det+cls+rec) 的端到端结果。"""
import json
py = json.load(open('data/baseline_det_py.json'))
rs = json.load(open('data/baseline_detpipe.json'))
P = {r['file']: r for r in py['rows']}
d = []
print(f"{'file':<34}{'py':>6}{'rust':>6}{'Δ':>7}{'bx_py':>7}{'bx_rs':>7}")
for r in rs['rows']:
    a, b = P[r['file']]['recall'], r['recall']
    d.append(b - a)
    print(f"{r['file'][:32]:<34}{a:6.2f}{b:6.2f}{b-a:+7.2f}{P[r['file']]['boxes']:7}{r['boxes']:7}")
print(f"\n平均字覆盖  py {py['avg_recall']:.3f}  rust {rs['avg_recall']:.3f}"
      f"  | 更好 {sum(1 for x in d if x > 0.02)} 持平 {sum(1 for x in d if abs(x) <= 0.02)} 更差 {sum(1 for x in d if x < -0.02)}")
print(f"耗时  py {py['avg_ms_per_image']:.0f}ms/图（det+cls+rec）"
      f"  rust det {rs['avg_ms_det']:.0f} + rec {rs['avg_ms_rec']:.0f} = {rs['avg_ms_det']+rs['avg_ms_rec']:.0f}ms/图")
