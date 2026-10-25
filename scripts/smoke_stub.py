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


def call(path, body=None, key=API):
    req = urllib.request.Request(f'http://127.0.0.1:{PORT}' + path, data=json.dumps(body).encode() if body is not None else None,
                                 headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
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
            status, _, _ = call('/admin/models', key=API)
            checks.append(('keeps admin routes admin-only', status == 401))
            status, _, body = call('/internal/hosts', key=ADMIN)
            checks.append(('lists hosts for admin', status == 200 and json.loads(body)['hosts'][0]['reachable']))
            status, _, body = call('/v1/embeddings', {'model': 'granite-nonexistent', 'input': 'x'})
            checks.append(('fails unknown models with a structured error', status == 404 and b'unknown_model' in body))
        finally:
            router.terminate()
            router.wait(timeout=20)
    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
