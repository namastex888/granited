#!/usr/bin/env python3
"""Regression for real native router startup, including concurrent first requests."""
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import sys
import time
import urllib.request
import urllib.error
from manage import environment, request

key = environment()['GRANITE_API_KEY']
results = []
for cycle in range(3):
    op = request('/admin/models/granite-4.2-3b/unload', {})
    while (operation := request('/admin/operations/' + op['id']))['status'] == 'running':
        time.sleep(.1)
    if operation['status'] != 'succeeded':
        raise RuntimeError(operation)

    def infer(index):
        body = {'model':'granite-4.2-3b','messages':[{'role':'user','content':'What is 2+2? Reply only with the number.'}],
                'temperature':0,'max_tokens':32,'reasoning_effort':'off','stream':True}
        req = urllib.request.Request('http://127.0.0.1:8310/v1/chat/completions', json.dumps(body).encode(),
                                     {'Content-Type':'application/json','Authorization':'Bearer '+key})
        started = time.perf_counter()
        try:
            with urllib.request.urlopen(req, timeout=120) as response:
                text = response.read().decode()
                return {'cycle':cycle,'request':index,'status':response.status,'completed':'[DONE]' in text,
                        'cold_stream_seconds':time.perf_counter()-started,'headers':dict(response.headers)}
        except urllib.error.HTTPError as error:
            return {'cycle':cycle,'request':index,'status':error.code,'completed':False,
                    'error':error.read().decode(),'headers':dict(error.headers)}

    with ThreadPoolExecutor(max_workers=2) as pool:
        results.extend(pool.map(infer, range(2)))
failed = [r for r in results if r['status'] != 200 or not r['completed']]
Path('reports/language-cold-regression.json').write_text(json.dumps({'kind':'native_cold_load_regression','results':results,'failed':len(failed),'model_accuracy':None},indent=2))
print(f'{len(results)-len(failed)}/{len(results)} concurrent cold streams completed')
raise SystemExit(bool(failed))
