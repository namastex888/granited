#!/usr/bin/env python3
"""Exec a managed service with private JSON credentials kept out of process arguments."""
import argparse
import os
from pathlib import Path

from manage import environment

ROOT = Path(__file__).resolve().parents[1]


def main():
    p = argparse.ArgumentParser()
    p.add_argument('service', choices=['router', 'litellm'])
    p.add_argument('--config', default='configs/cpu.json')
    p.add_argument('--listen-host', default='127.0.0.1')
    args = p.parse_args()
    os.chdir(ROOT)
    if args.service == 'router':
        command = [str(ROOT / 'bin/granited'), 'serve', str(ROOT / args.config)]
    else:
        command = [str(ROOT / '.venv-litellm/bin/litellm'), '--config', str(ROOT / 'configs/litellm.yaml'),
                   '--host', args.listen_host, '--port', '8300']
    os.execve(command[0], command, environment())


if __name__ == '__main__':
    main()
