#!/usr/bin/env python3
"""Maintain an explicitly selected SSH transport; record ownership for scoped cleanup."""
import argparse
import json
from pathlib import Path
import signal
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def identity(pid):
    return subprocess.run(['ps', '-p', str(pid), '-o', 'lstart='], capture_output=True, text=True).stdout.strip()


def main():
    p = argparse.ArgumentParser()
    p.add_argument('action', choices=['start', 'stop'])
    p.add_argument('name')
    p.add_argument('--target')
    p.add_argument('--reverse', action='store_true')
    p.add_argument('--listen-port', type=int)
    p.add_argument('--worker-port', type=int, default=8310)
    args = p.parse_args()
    if not args.name.replace('-', '').isalnum():
        raise SystemExit('Use an alphanumeric tunnel name')
    path = ROOT / f'state/{args.name}.process.json'
    if path.exists():
        saved = json.loads(path.read_text())
        alive = 'start_identity' in saved and identity(saved['pid']) == saved['start_identity']
        if args.action == 'stop':
            if alive:
                subprocess.run(['kill', '-'+str(signal.SIGTERM.value), str(saved['pid'])], check=True)
            print('Recorded tunnel stopped' if alive else 'Recorded tunnel is already gone')
            return
        if alive:
            raise SystemExit('Recorded tunnel is still running')
    if args.action == 'stop':
        raise SystemExit('No recorded tunnel')
    if not args.target or not args.listen_port:
        raise SystemExit('--target and --listen-port are required')
    command = ['ssh', '-N', '-o', 'BatchMode=yes', '-o', 'ExitOnForwardFailure=yes',
               '-o', 'ServerAliveInterval=15', '-o', 'ServerAliveCountMax=3',
               '-R' if args.reverse else '-L',
               f'127.0.0.1:{args.listen_port}:127.0.0.1:{args.worker_port}', args.target]
    with open(ROOT / f'state/{args.name}.log', 'ab') as log:
        child = subprocess.Popen(command, stdin=subprocess.DEVNULL, stdout=log, stderr=log, start_new_session=True)
    try:
        # ExitOnForwardFailure exits promptly when the forward cannot bind; do not record a dead tunnel.
        code = child.wait(timeout=3)
        raise SystemExit(f'Tunnel exited {code} during setup; inspect state/{args.name}.log')
    except subprocess.TimeoutExpired:
        pass
    path.write_text(json.dumps(dict(pid=child.pid, command=command, start_identity=identity(child.pid))))
    print(json.dumps(dict(tunnel=args.name, pid=child.pid, listen_port=args.listen_port)))


if __name__ == '__main__':
    main()
