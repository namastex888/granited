#!/usr/bin/env python3
"""End-to-end check of `model: "auto"` for chat, with two stub chat models and no real weights.

Each scenario starts the router on a fresh profile: the stubs stand in for Granite 4.2 8B and 3B,
sharing one exclusive device (a GPU-like rule: one resident model) or the CPU (no such rule).
The checks cover the auto v0 spec: warm before cold, no load that evicts a ready model, visible
fallbacks, the temperature clamp, sticky conversations, the closed aggregate refusal, and effort.
"""
import concurrent.futures
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
API, ADMIN = 'smoke-api-key-0123456789', 'smoke-admin-key-0123456789'
REGISTRY = {m['id']: m for m in json.loads((ROOT / 'configs/registry.json').read_text())['models']}
B8, B3, VISION = 'granite-4.2-8b', 'granite-4.2-3b', 'granite-vision-4.1-4b'


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


class Router:
    def __init__(self, device, hold=False, extra=(), models=None, peers=(), host='auto-smoke'):
        self.state = tempfile.TemporaryDirectory()
        self.port = free_port()
        deployments = []
        for model in (models or (B8, B3)) + tuple(extra):
            port = free_port()
            deployments.append(dict(
                model=model, backend='language_native', base_url=f'http://127.0.0.1:{port}',
                revision=REGISTRY[model]['hf_revision'], artifact_repo='stub',
                command=['python3', 'workers/stub.py', '--family', 'language', '--model', model,
                         '--revision', REGISTRY[model]['hf_revision'], '--port', str(port)],
                memory_bytes=64 << 20, device=device, health_path='/healthz', identity_path='/internal/metadata',
                runtime_manifest='workers/stub.py', backend_build='stub', max_concurrency=1, load_timeout_seconds=60,
                **({'hold_reason': 'held for the test'} if hold else {})))
        config = dict(listen=f'127.0.0.1:{self.port}', host_id=host, registry='configs/registry.json',
                      state_dir=self.state.name, api_key_env='GRANITE_API_KEY', admin_key_env='GRANITE_ADMIN_KEY',
                      ram_reserve_bytes=0, accelerator_reserve_bytes=0, deployments=deployments, peers=list(peers),
                      auto=dict(enabled=True, prefer=dict(chat=[B8, B3])))
        path = Path(self.state.name) / 'config.json'
        path.write_text(json.dumps(config))
        env = dict(os.environ, GRANITE_API_KEY=API, GRANITE_ADMIN_KEY=ADMIN, PEER_TOKEN=ADMIN)
        self.process = subprocess.Popen([str(ROOT / 'target/debug/granited'), 'serve', str(path)], cwd=ROOT, env=env,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(80):
            try:
                urllib.request.urlopen(f'http://127.0.0.1:{self.port}/healthz', timeout=1).close()
                return
            except OSError:
                time.sleep(0.25)
        raise SystemExit('router did not start')

    def chat(self, model='auto', text='hi', headers=None, **body):
        data = json.dumps(dict(model=model, messages=[dict(role='user', content=text)], **body)).encode()
        req = urllib.request.Request(f'http://127.0.0.1:{self.port}/v1/chat/completions', data,
                                     {'Authorization': 'Bearer ' + API, 'Content-Type': 'application/json', **(headers or {})})
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                status, head, raw = r.status, {k.lower(): v for k, v in r.headers.items()}, r.read()
        except urllib.error.HTTPError as e:
            status, head, raw = e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()
        body = json.loads(raw)
        route = json.loads(head['x-granite-route']) if 'x-granite-route' in head else {}
        seen = json.loads(body['choices'][0]['message']['content']) if status == 200 else {}
        return status, head, body, route, seen

    def admin(self, path):
        req = urllib.request.Request(f'http://127.0.0.1:{self.port}{path}', b'{}',
                                     {'Authorization': 'Bearer ' + ADMIN, 'Content-Type': 'application/json'})
        return json.loads(urllib.request.urlopen(req, timeout=60).read())

    def states(self):
        req = urllib.request.Request(f'http://127.0.0.1:{self.port}/internal/hosts', headers={'Authorization': 'Bearer ' + ADMIN})
        hosts = json.loads(urllib.request.urlopen(req, timeout=10).read())['hosts']
        return {w['model']: w['state'] for w in hosts[0]['hardware']['managed_workers']}

    def close(self):
        self.process.terminate()
        self.process.wait(timeout=20)
        self.state.cleanup()


def main():
    checks = []
    check = lambda name, ok: checks.append((name, bool(ok)))

    # One exclusive device: only one of the two can be resident.
    r = Router('unified')
    try:
        status, head, _, route, seen = r.chat()
        check('both cold: 8B loads and serves as the first preference',
              status == 200 and head.get('x-granite-model-alias') == B8 and route.get('source') == 'rule'
              and route.get('why') == 'cold_free' and seen.get('chat_template_kwargs', {}).get('enable_thinking') is False)
        r.chat(model=B3, reasoning_effort='off', temperature=0)  # an explicit 3B evicts 8B, as today
        status, head, _, route, seen = r.chat()
        check('3B warm, 8B cold on the same device: 3B serves and 8B is not loaded',
              status == 200 and head.get('x-granite-model-alias') == B3 and r.states().get(B8) != 'ready')
        check('the fallback is visible with the preferred model',
              route.get('source') == 'fallback' and route.get('preferred') == B8
              and route.get('fallback_reason') == 'preferred_unavailable' and route.get('why') == 'warm')
        check('3B with effort off and no temperature is clamped to 0',
              seen.get('temperature') == 0 and route.get('clamps') == ['temperature_zero'])
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            slow = pool.submit(r.chat, model=B3, text='SLOW', reasoning_effort='off', temperature=0)
            time.sleep(0.5)
            status, head, body, _, _ = r.chat()
            slow.result()
        check('3B full and 8B would evict it: 429 auto_unavailable/busy, Retry-After 2, nothing loaded',
              status == 429 and body['error'].get('code') == 'auto_unavailable' and body['error'].get('reason') == 'busy'
              and head.get('retry-after') == '2' and r.states().get(B8) != 'ready')
        status, head, _, route, _ = r.chat(headers={'x-granite-conversation': 'c1'})
        r.chat(model=B8, reasoning_effort='off')  # 8B evicts 3B
        status, head, _, route, _ = r.chat(headers={'x-granite-conversation': 'c1'})
        check('a sticky 3B conversation moves to warm 8B when 3B would need a load over it',
              status == 200 and head.get('x-granite-model-alias') == B8 and route.get('why') == 'sticky_moved'
              and r.states().get(B3) != 'ready')
        status, _, body, _, _ = r.chat(headers={'x-granite-conversation': 'not valid!'})
        check('a malformed conversation id is a 400', status == 400 and body['error']['code'] == 'invalid_schema')
        status, _, body, _, _ = r.chat(model=B3, reasoning_effort='off')
        check('an explicit 3B with effort off and no temperature is still a 400, never clamped',
              status == 400 and body['error']['code'] == 'invalid_schema')
        status, _, body, _, _ = r.chat(headers={'x-granite-hop': '1'})
        check('a peer hop never resolves auto', status == 400 and body['error']['code'] == 'auto_not_forwardable')
    finally:
        r.close()

    # CPU: both can be resident.
    r = Router('cpu')
    try:
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            slow = pool.submit(r.chat, model=B8, text='SLOW', reasoning_effort='off')
            time.sleep(0.8)
            status, head, _, route, _ = r.chat(headers={'x-granite-conversation': 'c2'})
            slow.result()
        check('8B full: 3B loads beside it (no eviction on CPU) and serves',
              status == 200 and head.get('x-granite-model-alias') == B3 and route.get('why') == 'cold_free')
        status, head, _, route, _ = r.chat(headers={'x-granite-conversation': 'c2'})
        check('a conversation stays on 3B while 8B is free: sticky, no fallback_reason',
              status == 200 and head.get('x-granite-model-alias') == B3 and route.get('source') == 'sticky'
              and route.get('preferred') == B8 and 'fallback_reason' not in route)
        status, head, _, route, seen = r.chat(thinking_budget_tokens=300, max_tokens=100)
        check('a thinking budget alone means full effort with that budget',
              status == 200 and route.get('effort') == 'full' and seen.get('thinking_budget_tokens') == 300
              and seen.get('max_tokens') == 400)
        status, _, _, route, seen = r.chat(reasoning_effort='low')
        check('an explicit effort wins', status == 200 and route.get('effort') == 'low'
              and seen.get('chat_template_kwargs', {}).get('low_effort') is True)
    finally:
        r.close()

    # A third model, not in auto's list, sits idle on the same device: auto must not evict it.
    r = Router('unified', extra=(VISION,))
    try:
        r.chat(model=VISION)
        status, head, body, _, _ = r.chat()
        check('an idle model outside the list is never evicted: 503 auto_unavailable/no_room, Retry-After 30',
              status == 503 and body['error'].get('reason') == 'no_room' and head.get('retry-after') == '30'
              and r.states().get(VISION) == 'ready' and r.states().get(B8) != 'ready')
        status, head, _, _, _ = r.chat(model=B8, reasoning_effort='off')
        check('an explicit model id still loads by evicting, as before', status == 200 and r.states().get(VISION) != 'ready')
    finally:
        r.close()

    # Peers: B serves 8B, A has 3B locally. A peer counts only while warm, and is told so.
    b = Router('cpu', models=(B8,), host='peer-b')
    a = Router('cpu', models=(B3,), host='node-a',
               peers=[dict(id='peer-b', url=f'http://127.0.0.1:{b.port}', token_env='PEER_TOKEN', models=[B8])])
    try:
        status, head, _, _, _ = b.chat(headers={'x-granite-load': 'warm-only'}, model=B8, reasoning_effort='off')
        check('a peer asked for warm-only never loads a cold model',
              status == 503 and head.get('x-granite-error-code') == 'model_not_warm' and b.states().get(B8) != 'ready')
        status, head, _, route, _ = a.chat()
        check('a cold peer is not tried: the local 3B loads and serves',
              status == 200 and head.get('x-granite-model-alias') == B3 and route.get('source') == 'fallback'
              and b.states().get(B8) != 'ready')
        b.chat(model=B8, reasoning_effort='off')  # warm 8B on B
        time.sleep(2.2)  # let A's snapshot of B expire
        status, head, _, route, _ = a.chat()
        check('a warm peer 8B serves as the first preference',
              status == 200 and head.get('x-granite-model-alias') == B8 and head.get('x-granite-host') == 'peer-b'
              and route.get('source') == 'rule')
        b.admin(f'/admin/models/{B8}/unload')
        for _ in range(40):
            if b.states().get(B8) != 'ready':
                break
            time.sleep(0.1)
        status, head, _, route, _ = a.chat()  # A's snapshot still says warm for up to 2 s
        check('a peer that went cold refuses model_not_warm and the next candidate serves',
              status == 200 and head.get('x-granite-model-alias') == B3 and route.get('why') == 'refused_next'
              and b.states().get(B8) != 'ready')
    finally:
        a.close()
        b.close()

    # Both held: nothing may load.
    r = Router('unified', hold=True)
    try:
        status, head, body, _, _ = r.chat()
        check('both held: 503 auto_unavailable/held with no Retry-After',
              status == 503 and body['error'].get('reason') == 'held' and 'retry-after' not in head
              and 'name a model' not in body['error']['message'])
    finally:
        r.close()

    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
