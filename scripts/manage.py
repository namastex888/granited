#!/usr/bin/env python3
"""Start and stop only this checkout's recorded services; keep tokens off argv."""
import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time
import urllib.request

ROOT = Path(__file__).resolve().parents[1]


def environment():
    env = os.environ.copy()
    env.update(json.loads((ROOT / 'state/secrets/keys.json').read_text()))
    env['GRANITE_AUTHORIZATION'] = 'Bearer ' + env['GRANITE_API_KEY']
    return env


def request(path, body=None, base='http://127.0.0.1:8310', admin=True):
    env = environment()
    key = env['GRANITE_ADMIN_KEY' if admin else 'GRANITE_API_KEY']
    req = urllib.request.Request(base + path, data=json.dumps(body).encode() if body is not None else None,
                                 headers={'Authorization':'Bearer ' + key, 'Content-Type':'application/json'})
    with urllib.request.urlopen(req, timeout=900) as response:
        return json.load(response)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('action', choices=['start', 'stop', 'status', 'request', 'load', 'unload', 'prefetch'])
    parser.add_argument('service', nargs='?', default='router')
    parser.add_argument('--config', default='configs/mac.json')
    parser.add_argument('--body-file')
    parser.add_argument('--base', default='http://127.0.0.1:8310')
    parser.add_argument('--listen-host', default='127.0.0.1')
    args = parser.parse_args()
    os.chdir(ROOT)
    if args.action in ('load', 'unload', 'prefetch'):
        operation = request(f'/admin/models/{args.service}/{args.action}', {}, args.base)
        for _ in range(1200):
            result = request('/admin/operations/' + operation['id'], base=args.base)
            if result['status'] != 'running':
                print(json.dumps(result, indent=2))
                raise SystemExit(0 if result['status'] == 'succeeded' else 1)
            time.sleep(.5)
        raise SystemExit('Operation still running; poll /admin/operations/' + operation['id'])
    if args.action == 'request':
        body = json.loads(Path(args.body_file).read_text()) if args.body_file else None
        print(json.dumps(request(args.service, body, args.base), indent=2))
        return
    pid_file = ROOT / f'state/{args.service}.process.json'
    if args.action == 'start':
        if pid_file.exists():
            old = json.loads(pid_file.read_text())
            try:
                os.kill(old['pid'], 0)
                state = subprocess.run(['ps', '-p', str(old['pid']), '-o', 'stat='], capture_output=True, text=True).stdout.strip()
                if state and not state.startswith('Z'):
                    raise SystemExit('Recorded service is still alive; inspect it before restarting')
            except ProcessLookupError:
                pass
        binary = ROOT / 'bin/granited'
        if not binary.exists():
            binary = ROOT / 'target/debug/granited'
        if args.service == 'router':
            command = [str(binary), 'serve', str(ROOT / args.config)]
        elif args.service == 'litellm':
            command = [str(ROOT / '.venv-litellm/bin/litellm'), '--config', str(ROOT / 'configs/litellm.yaml'), '--host', args.listen_host, '--port', '8300']
        else:
            raise SystemExit('Use router or litellm')
        log = open(ROOT / f'state/{args.service}.log', 'ab')
        process = subprocess.Popen(command, cwd=ROOT, env=environment(), stdout=log, stderr=log, start_new_session=True)
        identity = subprocess.check_output(['ps', '-p', str(process.pid), '-o', 'lstart='], text=True).strip()
        pid_file.write_text(json.dumps(dict(pid=process.pid, command=command, start_identity=identity)))
        port = 8300 if args.service == 'litellm' else int(json.loads((ROOT / args.config).read_text())['listen'].rsplit(':', 1)[1])
        for _ in range(60):
            if process.poll() is not None:
                raise SystemExit(f'Service exited {process.returncode}; inspect state/{args.service}.log')
            try:
                urllib.request.urlopen(f'http://{args.listen_host}:{port}/health/liveliness' if args.service == 'litellm' else f'http://127.0.0.1:{port}/healthz', timeout=1).close()
                break
            except Exception:
                time.sleep(.5)
        else:
            raise SystemExit(f'Service PID {process.pid} has not become healthy; inspect its log')
        print(json.dumps(dict(service=args.service, pid=process.pid, log=str(ROOT / f'state/{args.service}.log'))))
    elif args.action == 'stop':
        saved = json.loads(pid_file.read_text())
        current = subprocess.run(['ps', '-p', str(saved['pid']), '-o', 'lstart='], capture_output=True, text=True).stdout.strip()
        if current != saved['start_identity']:
            raise SystemExit('PID identity differs; refusing to signal it')
        os.kill(saved['pid'], signal.SIGTERM)
        for _ in range(60):
            state = subprocess.run(['ps', '-p', str(saved['pid']), '-o', 'stat='], capture_output=True, text=True).stdout.strip()
            if not state or state.startswith('Z'):
                break
            time.sleep(.25)
        print(f'SIGTERM sent to owned PID {saved["pid"]}')
    else:
        print(pid_file.read_text() if pid_file.exists() else 'No recorded service')


if __name__ == '__main__':
    try:
        main()
    except urllib.error.HTTPError as error:
        print(error.read().decode(), file=sys.stderr)
        raise SystemExit(1)
    except urllib.error.URLError as error:
        print(f'Service unavailable: {error.reason}', file=sys.stderr)
        raise SystemExit(1)
