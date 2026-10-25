#!/usr/bin/env python3
"""Register an existing llama router without adopting or restarting its process."""
import argparse
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parents[1]


def main():
    p = argparse.ArgumentParser()
    for option in ('config', 'model', 'base', 'metadata', 'artifact', 'build', 'container', 'digest'):
        p.add_argument('--' + option, required=True)
    p.add_argument('--memory-bytes', type=int, required=True)
    p.add_argument('--context', type=int, required=True)
    p.add_argument('--slots', type=int, required=True)
    p.add_argument('--control-lifecycle', action='store_true')
    args = p.parse_args()
    actual = subprocess.check_output(['docker', 'inspect', args.container, '--format', '{{.Image}}'], text=True).strip()
    if actual != args.digest:
        raise SystemExit('Container image digest differs from the supplied pin')
    info = json.loads(Path(args.metadata).read_text())
    artifact = Path(args.artifact).resolve()
    entry = next(f for f in info['siblings'] if f['rfilename'] == artifact.name)
    manifest = ROOT / ('locks/' + args.model + '-external-runtime.json')
    manifest.write_text(json.dumps(dict(container=args.container, image_digest=actual, backend_build=args.build,
                                       process_owner='external', lifecycle_delegated=args.control_lifecycle), indent=2)+'\n')
    config = json.loads(Path(args.config).read_text())
    config['deployments'] = [d for d in config['deployments'] if d['model'] != args.model]
    config['deployments'].append(dict(model=args.model, backend='llama_cpp', base_url=args.base,
        revision=info['sha'], artifact_repo=info['id'], command=[], external=True,
        external_lifecycle=args.control_lifecycle, shared_accelerator=True, keep_warm=True,
        enabled_capabilities=['adapters'] if 'switch' in args.model else ['chat'],
        backend_model=artifact.stem, artifacts={str(artifact):entry['lfs']['sha256']},
        memory_bytes=args.memory_bytes, device='cuda', health_path='/health', identity_path='/v1/models',
        runtime_manifest=str(manifest), backend_build=args.build, context_tokens=args.context,
        max_concurrency=args.slots, load_timeout_seconds=300))
    Path(args.config).write_text(json.dumps(config, indent=2)+'\n')
    print(f'{args.model}: registered external process; model lifecycle={args.control_lifecycle}')


if __name__ == '__main__':
    main()
