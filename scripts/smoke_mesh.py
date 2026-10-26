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
    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if checks and all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
