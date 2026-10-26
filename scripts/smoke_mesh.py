#!/usr/bin/env python3
"""Three routers in a full mesh, one stub model on each: when one node dies, the other two keep
serving without an error or a hang, and the dead node rejoins when it comes back.

B is warmed first, so A and C rank warm B above their own cold workers. B is then killed outright
(SIGKILL, as in a crash). The next request to A still picks B from its snapshot, finds it
unreachable, puts it in backoff and is served by the next host. No quorum is involved: each node
places requests on its own, so any one survivor keeps serving.
"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time

from smoke_fallback import ADMIN, API, call, free_port, profile, wait_up

ROOT = Path(__file__).resolve().parents[1]


def foreign_process_is_never_stopped(binary):
    """A process the manager did not spawn holds a worker's port: the manager refuses the port and
    never signals that process, at request time or when it starts and reaps its ledger, even when
    a ledger entry names the foreign PID with another start time (a reused PID)."""
    checks = []
    with tempfile.TemporaryDirectory() as state:
        port = free_port()
        path, model = profile(state, 'solo', port, 0, [])
        worker = json.loads(path.read_text())['deployments'][0]['base_url'].rsplit(':', 1)[1]
        foreign = subprocess.Popen([sys.executable, '-m', 'http.server', worker, '--bind', '127.0.0.1'], cwd=state,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        owned = Path(state) / 'solo' / 'owned'
        owned.mkdir(parents=True)
        (owned / f'{model}.pid').write_text(f'{foreign.pid}\tMon Jan  1 00:00:00 2001\n')
        env = dict(os.environ, GRANITE_API_KEY=API, GRANITE_ADMIN_KEY=ADMIN)
        try:
            for _ in range(100):  # the foreign server answers 404 on /healthz; wait for its socket
                try:
                    socket.create_connection(('127.0.0.1', int(worker)), 0.2).close()
                    break
                except OSError:
                    time.sleep(0.1)
            router = subprocess.Popen([str(binary), 'serve', str(path)], cwd=ROOT, env=env, start_new_session=True,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_up(port)
            status, _, raw = call(port, '/v1/embeddings', {'model': model, 'input': 'x'})
            checks.append(('a foreign process on a worker port is refused, not adopted',
                           status == 503 and b'worker_port_in_use' in raw))
            os.killpg(router.pid, signal.SIGTERM)
            router.wait(timeout=20)
            checks.append(('the foreign process survives startup reaping, a ledger entry with its PID, and shutdown',
                           foreign.poll() is None and not (owned / f'{model}.pid').exists()))
        finally:
            foreign.terminate()
            foreign.wait(timeout=10)
    return checks


MOCK_ROUTER = """
import http.server, json, sys, time
class H(http.server.BaseHTTPRequestHandler):
    def reply(self, body):
        data = json.dumps(body).encode()
        self.send_response(200); self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(data))); self.end_headers(); self.wfile.write(data)
    def do_GET(self):
        if self.path.startswith('/v1/models'):
            time.sleep(2)  # a router that is slow to list its models, as a busy one is
            return self.reply({'data': [{'id': 'granite-4.2-3b-Q4_K_M', 'status': {'value': 'loaded', 'args': []}}]})
        if self.path.startswith('/props'):
            return self.reply({'default_generation_settings': {'n_ctx': 4096}, 'total_slots': 1, 'build_info': 'mock'})
        self.reply({'status': 'ok'})
    def do_POST(self):
        self.rfile.read(int(self.headers.get('Content-Length', 0)))
        self.reply({'object': 'chat.completion', 'model': 'granite-4.2-3b-Q4_K_M', 'choices': [{'index': 0, 'finish_reason': 'stop',
                    'message': {'role': 'assistant', 'content': 'hi'}}], 'usage': {'prompt_tokens': 3, 'completion_tokens': 1, 'total_tokens': 4}})
    def log_message(self, *a): pass
http.server.ThreadingHTTPServer(('127.0.0.1', int(sys.argv[1])), H).serve_forever()
"""


def external_engine_ready_at_first_request(binary):
    """An operator-owned router is read before the manager listens: the first request after a
    restart is served, not refused with capacity_unavailable while the first sync is pending."""
    with tempfile.TemporaryDirectory() as state:
        engine, port = free_port(), free_port()
        mock = subprocess.Popen([sys.executable, '-c', MOCK_ROUTER, str(engine)], stdout=subprocess.DEVNULL,
                                stderr=subprocess.DEVNULL, start_new_session=True)
        path, _ = profile(state, 'ext', port, 0, [])
        config = json.loads(path.read_text())
        registry = {m['id']: m for m in json.loads((ROOT / 'configs/registry.json').read_text())['models']}
        config['deployments'] = [dict(model='granite-4.2-3b', backend='llama_cpp', base_url=f'http://127.0.0.1:{engine}',
                                      revision=registry['granite-4.2-3b']['hf_revision'], artifact_repo='stub', command=[],
                                      external=True, external_lifecycle=False, shared_accelerator=True, keep_warm=True,
                                      backend_model='granite-4.2-3b-Q4_K_M', memory_bytes=64 << 20, device='cpu',
                                      health_path='/health', identity_path='/v1/models', runtime_manifest='scripts/smoke_mesh.py',
                                      backend_build='mock', context_tokens=4096, max_concurrency=1, load_timeout_seconds=30)]
        path.write_text(json.dumps(config))
        env = dict(os.environ, GRANITE_API_KEY=API, GRANITE_ADMIN_KEY=ADMIN)
        router = None
        try:
            for _ in range(100):
                try:
                    socket.create_connection(('127.0.0.1', engine), 0.2).close()
                    break
                except OSError:
                    time.sleep(0.1)
            router = subprocess.Popen([str(binary), 'serve', str(path)], cwd=ROOT, env=env, start_new_session=True,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            for _ in range(400):  # poll the socket, so the request lands the moment the manager listens
                try:
                    socket.create_connection(('127.0.0.1', port), 0.05).close()
                    break
                except OSError:
                    time.sleep(0.02)
            status, _, raw = call(port, '/v1/chat/completions', {'model': 'granite-4.2-3b', 'reasoning_effort': 'off', 'temperature': 0,
                                                                 'max_tokens': 4, 'messages': [{'role': 'user', 'content': 'hi'}]})
            print('   first request after start:', status, raw[:100])
            return [('an external engine is served on the first request after a restart', status == 200)]
        finally:
            if router:
                os.killpg(router.pid, signal.SIGTERM)
                router.wait(timeout=20)
            os.killpg(mock.pid, signal.SIGTERM)
            mock.wait(timeout=10)


def main():
    binary = ROOT / 'target/debug/granited'
    ports = {n: free_port() for n in 'abc'}
    checks, routers = [], {}
    with tempfile.TemporaryDirectory() as state:
        paths, model = {}, None
        for n in 'abc':
            peers = [dict(id=o, url=f'http://127.0.0.1:{ports[o]}', token_env='GRANITE_PEER', models=['MODEL'])
                     for o in 'abc' if o != n]
            paths[n], model = profile(state, n, ports[n], 0, peers)
        for n in 'abc':
            config = json.loads(paths[n].read_text())
            for peer in config['peers']:
                peer['models'] = [model]
            paths[n].write_text(json.dumps(config))
        env = dict(os.environ, GRANITE_API_KEY=API, GRANITE_ADMIN_KEY=ADMIN, GRANITE_PEER=ADMIN)

        def start(n):
            routers[n] = subprocess.Popen([str(binary), 'serve', str(paths[n])], cwd=ROOT, env=env, start_new_session=True,
                                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            wait_up(ports[n])

        body = {'model': model, 'input': 'x'}
        try:
            for n in 'abc':
                start(n)
            t = time.monotonic()
            status, _, _ = call(ports['b'], '/v1/embeddings', body)
            cold_load = time.monotonic() - t  # this machine's cold start of one stub worker

            def via_b(n):
                # Setup, not the claim: A and C must be cold and routed to warm B, so the kill below
                # exercises failover. A transient miss (a slow snapshot) unloads that node and retries.
                for _ in range(5):
                    call(ports['b'], '/v1/embeddings', body)
                    status, head, _ = call(ports[n], '/v1/embeddings', body)
                    if status == 200 and head.get('x-granite-host') == 'b':
                        return status, head
                    call(ports[n], f'/admin/models/{model}/unload', {}, key=ADMIN)
                    time.sleep(1)
                return status, head

            status_a, head_a = via_b('a')
            status_c, head_c = via_b('c')
            print('   mesh up served by:', head_a.get('x-granite-host'), head_c.get('x-granite-host'), f'(B cold load {cold_load:.2f} s)')
            checks.append(('mesh up: A and C prefer warm B over their own cold workers',
                           status == 200 and status_a == 200 and status_c == 200
                           and head_a.get('x-granite-host') == 'b' and head_c.get('x-granite-host') == 'b'))
            os.killpg(routers['b'].pid, signal.SIGKILL)
            routers['b'].wait(timeout=20)
            served = []
            for n in 'ac':
                t = time.monotonic()
                status, head, _ = call(ports[n], '/v1/embeddings', body)
                served.append((n, status, head.get('x-granite-host'), time.monotonic() - t))
            # A survivor may cold-start its own worker; anything past that plus 5 s is a wait on dead B.
            bound = 2 * cold_load + 5
            checks.append(('B killed: the next request to each survivor is served, not by B, with no wait on B',
                           all(s == 200 and host not in (None, 'b') and secs < bound for _, s, host, secs in served)))
            print('   after the kill:', [(n, s, host, round(secs, 2)) for n, s, host, secs in served],
                  f'(cold load {cold_load:.2f} s, bound {bound:.1f} s)')
            later = [call(ports[n], '/v1/embeddings', body) for n in 'acac']
            checks.append(('B down: every later request to A and C is served too', all(s == 200 for s, _, _ in later)))
            def reachable(n, peer):
                status, _, raw = call(ports[n], '/internal/hosts', key=ADMIN)
                return next((h['reachable'] for h in json.loads(raw)['hosts'] if h['host'] == peer), None) if status == 200 else None
            checks.append(('A and C report B unreachable while it is down', reachable('a', 'b') is False and reachable('c', 'b') is False))
            start('b')
            call(ports['b'], '/v1/embeddings', body)
            # A and C warmed their own workers while B was down, and a warm local or warm C outranks B;
            # unload them so the only warm host is B again, then A must route there after B's backoff.
            for n in 'ac':
                call(ports[n], f'/admin/models/{model}/unload', {}, key=ADMIN)
            deadline, back = time.monotonic() + 60, False
            while time.monotonic() < deadline and not back:
                call(ports['b'], '/v1/embeddings', body)  # the stub idles out fast; keep B warm
                status, head, _ = call(ports['a'], '/v1/embeddings', body)
                back = status == 200 and head.get('x-granite-host') == 'b'
                if os.environ.get('MESH_DEBUG'):
                    print('   rejoin try:', status, head.get('x-granite-host'), 'b reachable from a:', reachable('a', 'b'))
                if not back:
                    for n in 'ac':  # leave B as the only warm host
                        call(ports[n], f'/admin/models/{model}/unload', {}, key=ADMIN)
                    time.sleep(1)
            checks.append(('B restarted: A reports it reachable and routes to it again, within 60 s', back and reachable('a', 'b') is True))
        finally:
            for router in routers.values():
                if router.poll() is None:
                    os.killpg(router.pid, signal.SIGTERM)
                    router.wait(timeout=20)
    checks += foreign_process_is_never_stopped(binary)
    checks += external_engine_ready_at_first_request(binary)
    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if checks and all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
