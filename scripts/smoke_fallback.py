#!/usr/bin/env python3
"""Two routers, no models: a request routed to a peer that filled up after the last snapshot is
served by the next-ranked host instead of failing, and the refusal is counted.

Router A has the model locally (cold) and peer B (warm, one slot, every call takes DELAY). Placement
ranks warm B first. A request to A first records B as free; a direct call to B then holds B's only
slot, and the next request to A, still within A's 2 s snapshot of B, is refused by B before running
and falls back to A's own worker.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
API, ADMIN = 'smoke-api-key-0123456789', 'smoke-admin-key-0123456789'
DELAY_MS = 900


def free_port():
    import socket
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def call(port, path, body=None, key=API):
    req = urllib.request.Request(f'http://127.0.0.1:{port}' + path, data=json.dumps(body).encode() if body is not None else None,
                                 headers={'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
    try:
        with urllib.request.urlopen(req, timeout=180) as r:
            return r.status, dict(r.headers), r.read()
    except urllib.error.HTTPError as e:
        return e.code, dict(e.headers), e.read()


def profile(state, name, port, delay_ms, peers):
    config = json.loads((ROOT / 'configs/examples/stub.json').read_text())
    config.update(state_dir=str(Path(state) / name), listen=f'127.0.0.1:{port}', host_id=name, peers=peers)
    worker = free_port()
    d = config['deployments'][0]
    d.update(base_url=f'http://127.0.0.1:{worker}', max_concurrency=1)
    d['command'][d['command'].index('--port') + 1] = str(worker)
    d['command'] += ['--delay-ms', str(delay_ms)]
    path = Path(state) / f'{name}.json'
    path.write_text(json.dumps(config))
    return path, d['model']


def wait_up(port):
    for _ in range(120):
        try:
            urllib.request.urlopen(f'http://127.0.0.1:{port}/healthz', timeout=1).close()
            return
        except OSError:
            time.sleep(0.25)
    raise SystemExit(f'router on {port} did not start')


def main():
    binary = ROOT / 'target/debug/granited'
    a_port, b_port = free_port(), free_port()
    checks, routers = [], []
    with tempfile.TemporaryDirectory() as state:
        b_path, model = profile(state, 'b', b_port, DELAY_MS, [])
        a_path, _ = profile(state, 'a', a_port, 0, [dict(id='b', url=f'http://127.0.0.1:{b_port}',
                                                         token_env='GRANITE_PEER_B', models=[model])])
        env = dict(os.environ, GRANITE_API_KEY=API, GRANITE_ADMIN_KEY=ADMIN, GRANITE_PEER_B=ADMIN)
        try:
            for path in (b_path, a_path):
                routers.append(subprocess.Popen([str(binary), 'serve', str(path)], cwd=ROOT, env=env))
            wait_up(b_port)
            wait_up(a_port)
            body = {'model': model, 'input': 'x'}
            status, _, _ = call(b_port, '/v1/embeddings', body)  # warm B so placement ranks it first
            checks.append(('peer warms up', status == 200))
            status, headers, _ = call(a_port, '/v1/embeddings', body)
            checks.append(('warm peer is preferred over a cold local worker', status == 200 and headers.get('x-granite-host') == 'b'))
            holder = threading.Thread(target=call, args=(b_port, '/v1/embeddings', body))
            holder.start()
            time.sleep(0.15)
            status, headers, _ = call(a_port, '/v1/embeddings', body)
            holder.join()
            checks.append(('a peer that filled up hands the request to the next host',
                           status == 200 and headers.get('x-granite-host') == 'a'))
            status, _, raw = call(a_port, '/internal/metrics', key=ADMIN)
            rejections = json.loads(raw).get('rejections', {}) if status == 200 else {}
            checks.append(('the refusal is counted', sum(rejections.get(model, {}).values()) >= 1))
        finally:
            for router in routers:
                router.terminate()
                router.wait(timeout=20)
    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if checks and all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
