import json
py=json.load(open('data/baseline_python.json')); tr=json.load(open('data/baseline_tract.json'))
P={r['i']:r for r in py['rows']}; T={r['i']:r for r in tr['rows']}
print(f"{'i':>3} {'py':>5} {'tract':>6} {'Δ':>6}  truth")
for i in sorted(P):
    a,b=P[i]['recall'],T[i]['recall']
    print(f"{i:>3} {a:5.2f} {b:6.2f} {b-a:+6.2f}  {P[i]['truth'][:28]}")
print('avg py', round(sum(r['recall'] for r in py['rows'])/len(py['rows']),3),
      '| tract', round(sum(r['recall'] for r in tr['rows'])/len(tr['rows']),3),
      '| ms/strip py', round(py['sec']/py['strips']*1000,1), 'tract', tr['avg_ms_per_strip'])
