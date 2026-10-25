#!/usr/bin/env python3
"""Functional HTTP checks against the running control plane. No model-quality scores."""
import json
from pathlib import Path
import sys
import urllib.request
import urllib.error

sys.path.insert(0, str(Path(__file__).resolve().parent))
from manage import ROOT, environment


def main():
    keys = environment()
    cases = [
        ('health', '/healthz', None, None, 200, None),
        ('api_auth', '/v1/models', None, None, 401, 'unauthorized'),
        ('model_registry', '/v1/models', None, keys['GRANITE_API_KEY'], 200, None),
        ('admin_scope', '/admin/models', None, keys['GRANITE_API_KEY'], 401, 'unauthorized'),
        ('admin_registry', '/admin/models', None, keys['GRANITE_ADMIN_KEY'], 200, None),
        ('unknown_model', '/v1/chat/completions', {'model':'does-not-exist','messages':[{'role':'user','content':'hi'}]}, keys['GRANITE_API_KEY'], 404, 'unknown_model'),
        ('invalid_history', '/v1/forecast', {'model':'granite-timeseries-ttm-r3','history':[],'horizon':0}, keys['GRANITE_API_KEY'], 400, 'invalid_schema'),
        ('unsupported_capability', '/v1/forecast', {'model':'granite-4.2-3b','history':[1,2],'horizon':1}, keys['GRANITE_API_KEY'], 501, 'unsupported_capability'),
        ('unsupported_deployment', '/v1/documents/parse', {'model':'granite-docling-258M','file_base64':'','mime_type':'application/pdf'}, keys['GRANITE_API_KEY'], 503, 'model_unavailable'),
        ('thinking_grammar', '/v1/chat/completions', {'model':'granite-4.2-3b','messages':[{'role':'user','content':'hi'}],'grammar':'root ::= "hi"'}, keys['GRANITE_API_KEY'], 400, 'invalid_schema'),
        ('thinking_schema', '/v1/chat/completions', {'model':'granite-4.2-3b','messages':[{'role':'user','content':'hi'}],'chat_template_kwargs':'bad'}, keys['GRANITE_API_KEY'], 400, 'invalid_schema'),
        ('off_leak_policy', '/v1/chat/completions', {'model':'granite-4.2-3b','messages':[{'role':'user','content':'hi'}],'chat_template_kwargs':{'enable_thinking':False},'temperature':.3}, keys['GRANITE_API_KEY'], 400, 'invalid_schema'),
    ]
    results = []
    for name, path, body, token, expected_status, expected_error in cases:
        headers = {'Content-Type':'application/json'}
        if token:
            headers['Authorization'] = 'Bearer ' + token
        req = urllib.request.Request('http://127.0.0.1:8310' + path, json.dumps(body).encode() if body is not None else None, headers)
        try:
            response = urllib.request.urlopen(req, timeout=15)
        except urllib.error.HTTPError as error:
            response = error
        status, data = response.status, json.load(response)
        assert status == expected_status, (name, status, data)
        if expected_error:
            assert data['error']['code'] == expected_error, (name, data)
        for header in ['x-granite-request-id','x-granite-model-revision','x-granite-backend','server-timing']:
            assert response.headers.get(header), (name, header)
        results.append(dict(name=name, status=status, passed=True))
    output = dict(kind='control_plane_functionality', model_accuracy=None, checks=results)
    (ROOT / 'reports/api-functional.json').write_text(json.dumps(output, indent=2))
    print(f'{len(results)} functional HTTP checks passed; no model accuracy measured')


if __name__ == '__main__':
    main()
