#!/usr/bin/env python3
"""End-to-end smoke test without models: router + stub worker, auth, routing, provenance, admin isolation and errors."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
API, ADMIN = 'smoke-api-key-0123456789', 'smoke-admin-key-0123456789'


def free_port():
    import socket
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


PORT = free_port()


def call(path, body=None, key=API, headers=None):
    req = urllib.request.Request(f'http://127.0.0.1:{PORT}' + path, data=json.dumps(body).encode() if body is not None else None,
                                 headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json', **(headers or {})})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            return r.status, dict(r.headers), r.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


def main():
    binary = ROOT / 'target/debug/granited'
    with tempfile.TemporaryDirectory() as state:
        config = json.loads((ROOT / 'configs/examples/stub.json').read_text())
        config['state_dir'] = state
        config['listen'] = f'127.0.0.1:{PORT}'
        worker = free_port()
        deployment = config['deployments'][0]
        deployment['base_url'] = f'http://127.0.0.1:{worker}'
        deployment['command'][deployment['command'].index('--port') + 1] = str(worker)
        config['auto'] = {'enabled': True, 'prefer': {'embeddings': [deployment['model']]}}
        path = Path(state) / 'config.json'
        path.write_text(json.dumps(config))
        env = dict(os.environ, GRANITE_API_KEY=API, GRANITE_ADMIN_KEY=ADMIN)
        router = subprocess.Popen([str(binary), 'serve', str(path)], cwd=ROOT, env=env)
        try:
            for _ in range(80):
                try:
                    urllib.request.urlopen(f'http://127.0.0.1:{PORT}/healthz', timeout=1).close()
                    break
                except OSError:
                    time.sleep(0.25)
            model = config['deployments'][0]['model']
            checks = []
            status, _, _ = call('/v1/embeddings', {'model': model, 'input': 'x'}, key='wrong')
            checks.append(('rejects a wrong key', status == 401))
            status, headers, body = call('/v1/embeddings', {'model': model, 'input': ['a', 'b']})
            data = json.loads(body) if status == 200 else {}
            checks.append(('embeds through the stub', status == 200 and len(data.get('data', [])) == 2))
            checks.append(('reports provenance', headers.get('x-granite-backend') == 'embedding_native'
                           and headers.get('x-granite-model-revision') == config['deployments'][0]['revision']))
            status, headers, body = call('/v1/embeddings', {'model': 'auto', 'input': ['a']})
            checks.append(('auto embeds with the pinned model', status == 200
                           and headers.get('x-granite-model-alias') == model and len(json.loads(body)['data']) == 1))
            status, _, body = call('/v1/chat/completions', {'model': 'auto', 'messages': [{'role': 'user', 'content': 'hi'}]})
            checks.append(('auto refuses endpoints it does not cover', status == 400 and b'unsupported_auto' in body))
            status, _, body = call('/v1/models')
            listed = [m for m in json.loads(body)['data'] if m['id'] == 'auto'] if status == 200 else []
            checks.append(('lists auto with its capabilities', listed and listed[0]['capabilities'] == ['embeddings']))
            req = urllib.request.Request(f'http://127.0.0.1:{PORT}/v1/embeddings', json.dumps({'model': 'auto', 'input': 'x'}).encode(),
                                         {'Authorization': 'Bearer ' + API, 'Content-Type': 'application/json', 'x-granite-hop': '1'})
            try:
                urllib.request.urlopen(req, timeout=30); hop = 200
            except urllib.error.HTTPError as e:
                hop = e.code if b'auto_not_forwardable' in e.read() else -1
            checks.append(('never resolves auto for a peer hop', hop == 400))
            status, _, _ = call('/admin/models', key=API)
            checks.append(('keeps admin routes admin-only', status == 401))
            status, _, body = call('/internal/hosts', key=ADMIN)
            checks.append(('lists hosts for admin', status == 200 and json.loads(body)['hosts'][0]['reachable']))
            status, _, body = call('/internal/metrics', key=ADMIN)
            usage = json.loads(body)['usage'] if status == 200 else {}
            checks.append(('counts usage for admin', usage.get('usage_reported') is True
                           and usage.get('models', {}).get(model, {}).get('requests_completed') == 2))
            checks.append(('keeps usage admin-only', call('/internal/metrics', key=API)[0] == 401))
            for label in ['Bad Label!', 'x' * 33] + [f'app-{i:02}' for i in range(33)]:
                call('/v1/embeddings', {'model': model, 'input': 'x'}, headers={'X-LocalFleet-Client': label})
            status, _, body = call('/internal/metrics', key=ADMIN)
            clients = json.loads(body)['clients'] if status == 200 else {}
            seen = clients.get('last_seen_unix_ms', {})
            checks.append(('ignores an invalid client label', 'Bad Label!' not in seen and 'x' * 33 not in seen))
            checks.append(('a 33rd client label evicts the least recently seen', len(seen) == 32
                           and 'app-00' not in seen and 'app-32' in seen and clients.get('window_seconds') == 300
                           and clients.get('active') == {} and clients.get('unidentified_active') == 0))
            status, _, body = call('/internal/hardware', key=ADMIN)
            accelerators = json.loads(body)['accelerators'] if status == 200 else []
            checks.append(('reports power with a scope, and null with a reason when unreadable', all(
                {'watts', 'limit_watts', 'scope', 'source', 'reason'} <= set(a['power'])
                and (a['power']['watts'] is not None or a['power']['reason']) for a in accelerators)))
            status, _, body = call('/v1/embeddings', {'model': 'granite-nonexistent', 'input': 'x'})
            checks.append(('fails unknown models with a structured error', status == 404 and b'unknown_model' in body))
        finally:
            router.terminate()
            router.wait(timeout=20)
        # A graceful stop writes the ledger once more: counts only, readable by its owner alone.
        ledger = Path(state) / 'activity.json'
        stored = json.loads(ledger.read_text()) if ledger.exists() else {}
        checks.append(('the activity ledger is flushed at shutdown with mode 0600', ledger.exists()
                       and ledger.stat().st_mode & 0o777 == 0o600
                       and sum(b['succeeded'] for b in stored.get('buckets', [])) == 2 + 35
                       and len(stored.get('coverage', [])) == 1))
    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
