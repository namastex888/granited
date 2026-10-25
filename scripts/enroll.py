#!/usr/bin/env python3
"""Enroll a trusted node through an explicit model allowlist and private token file."""
import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main():
    p = argparse.ArgumentParser()
    for name in ('config', 'id', 'url', 'token-file'):
        p.add_argument('--' + name, required=True)
    p.add_argument('--models', nargs='+', required=True)
    args = p.parse_args()
    if not args.id.replace('-', '').isalnum():
        raise SystemExit('Use an alphanumeric node id')
    token = Path(args.token_file).read_text().strip()
    if len(token) < 24:
        raise SystemExit('Node token is too short')
    config_path = Path(args.config)
    config = json.loads(config_path.read_text())
    registry = json.loads(Path(config['registry']).read_text())
    if set(args.models) - {m['id'] for m in registry['models']}:
        raise SystemExit('Node model allowlist contains unregistered models')
    key = 'GRANITE_PEER_' + args.id.upper().replace('-', '_')
    secret_path = ROOT / 'state/secrets/keys.json'
    secrets = json.loads(secret_path.read_text())
    secrets[key] = token
    secret_path.write_text(json.dumps(secrets))
    secret_path.chmod(0o600)
    config['peers'] = [peer for peer in config.get('peers', []) if peer['id'] != args.id]
    config['peers'].append(dict(id=args.id, url=args.url, token_env=key, models=args.models))
    config_path.write_text(json.dumps(config, indent=2)+'\n')
    print(f'{args.id} enrolled; restart the coordinator to apply the environment and configuration')


if __name__ == '__main__':
    main()
