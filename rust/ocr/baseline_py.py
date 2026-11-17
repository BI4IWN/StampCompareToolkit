import os, json, time
from collections import Counter
import numpy as np
from PIL import Image
import yaml
from rapidocr_onnxruntime.ch_ppocr_rec import TextRecognizer

ROOT=os.path.dirname(os.path.abspath(__file__)); D=os.path.join(ROOT,'data')
mani=json.load(open(os.path.join(D,'manifest.json')))
cfg=yaml.safe_load(open(os.path.expanduser('~/Downloads/印章比对/StampCompareToolkit/desktop/runtime/python-mac-arm64/lib/python3.12/site-packages/rapidocr_onnxruntime/config.yaml')))
rec=TextRecognizer({**cfg['Rec'], 'rec_batch_num':6, 'rec_img_shape':[3,48,320], 'model_path':os.path.expanduser('~/Downloads/印章比对/StampCompareToolkit/desktop/runtime/python-mac-arm64/lib/python3.12/site-packages/rapidocr_onnxruntime/models/ch_PP-OCRv4_rec_infer.onnx')})

def recall(truth,pred):
    t,p=Counter(truth),Counter(pred)
    return sum(min(t[c],p[c]) for c in t)/max(1,len(truth))

rows=[]; t0=time.time(); n=0
for m in mani:
    parts=[]
    for k in range(4):
        p=os.path.join(D,'strips',f'{m["i"]:02d}_{k}.png')
        if not os.path.exists(p): continue
        img=np.array(Image.open(p).convert('RGB'))[:,:,::-1]  # RGB->BGR，与 rapidocr 内部一致
        out,_=rec(img); n+=1
        parts.append(out[0][0] if out else '')
    pred=''.join(parts)
    rows.append({'i':m['i'],'truth':m['truth'],'pred':pred,'recall':round(recall(m['truth'],pred),3)})
el=time.time()-t0
json.dump({'engine':'rapidocr-python','rows':rows,'strips':n,'sec':round(el,2)},open(os.path.join(D,'baseline_python.json'),'w'),ensure_ascii=False,indent=1)
avg=sum(r['recall'] for r in rows)/len(rows)
print(f"rapidocr(python)  平均字覆盖 {avg:.3f}  平均每条 {el/n*1000:.0f}ms  共 {n} 条")
for r in rows[:6]: print(f"  {r['i']:02d} {r['recall']:.2f}  {r['truth']}  ->  {r['pred'][:40]}")
