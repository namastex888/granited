#!/usr/bin/env python3
"""Write host profiles from resolved Hub metadata and installed native runtimes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import subprocess

ROOT = Path(__file__).resolve().parents[1]
GIB = 1024 ** 3


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as f:
        for chunk in iter(lambda: f.read(1 << 20), b''):
            digest.update(chunk)
    return digest.hexdigest()


# One source for the language pool's parallelism: llama.cpp splits -c across -np slots, oMLX runs
# --max-concurrent-requests (default 8), granited admits max_concurrency requests, and each slot must
# hold context_tokens. Change these together.
LANGUAGE_SLOTS = 2
SLOT_CONTEXT = 8192


def write(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--host', choices=['mac', 'cuda', 'cpu'], required=True)
    parser.add_argument('--native', action='store_true')
    parser.add_argument('--port', type=int, default=8310)
    parser.add_argument('--llama-runtime', help='Validated llama-server executable; enables the language pool')
    parser.add_argument('--llama-device', choices=['metal', 'vulkan', 'cuda', 'rocm', 'cpu'],
                        help='Accelerator for the language pool (default: metal on mac, vulkan otherwise)')
    parser.add_argument('--languages', default='3b,8b', help='Granite 4.2 sizes to deploy, comma separated, or none')
    parser.add_argument('--native-models', default='embedding,timeseries,speech',
                        help='Native CPU worker families to deploy with --native, comma separated')
    parser.add_argument('--models-dir', default=str(ROOT / 'models'))
    parser.add_argument('--pid-registry', help='Optional file another supervisor reads to track spawned llama PIDs')
    parser.add_argument('--profile', help='Profile name; writes configs/PROFILE.json and keeps receipts under state/receipts')
    parser.add_argument('--host-id', help='Node identity reported to peers (default: profile or host)')
    parser.add_argument('--ram-reserve-gib', type=float, default=8)
    parser.add_argument('--omlx', help='oMLX executable; serves the language pool from official Granite MLX builds instead of llama.cpp')
    args = parser.parse_args()
    profile = args.profile or args.host
    receipts = ROOT / ('state/receipts' if args.profile else 'locks')
    receipts.mkdir(parents=True, exist_ok=True)
    device = args.llama_device or ('metal' if args.host == 'mac' else 'vulkan')
    sizes = [s for s in args.languages.split(',') if s and s != 'none']
    models_dir = Path(args.models_dir).resolve()
    os.chdir(ROOT)
    (ROOT / 'state').mkdir(exist_ok=True)
    (ROOT / 'state/secrets').mkdir(exist_ok=True, mode=0o700)
    secret_file = ROOT / 'state/secrets/keys.json'
    if not secret_file.exists():
        secret_file.write_text(json.dumps({key: secrets.token_urlsafe(32) for key in ['GRANITE_API_KEY', 'GRANITE_ADMIN_KEY', 'LITELLM_MASTER_KEY']}))
        secret_file.chmod(0o600)
    models = {m['id']: m for m in json.loads((ROOT / 'configs/registry.json').read_text())['models']}
    config = dict(listen=f'127.0.0.1:{args.port}', host_id=args.host_id or profile, registry=str(ROOT / 'configs/registry.json'),
                  state_dir=str(ROOT / 'state/service'), api_key_env='GRANITE_API_KEY', admin_key_env='GRANITE_ADMIN_KEY',
                  ram_reserve_bytes=int(args.ram_reserve_gib*GIB), accelerator_reserve_bytes=GIB, deployments=[], peers=[])
    if sizes and args.omlx:
        version = subprocess.check_output([args.omlx, '--version'], text=True).strip().split()[-1]
        # Measured peak phys_footprint at LANGUAGE_SLOTS concurrent ~6.5k-token prompts on an M5 Pro, plus
        # 15%: 3B 5.07 GB, 8B 10.24 GB (reports/2026-10-25-council-performance-concurrency.md). oMLX holds
        # weights and per-request buffers in counted memory, so it needs more than llama.cpp's mmap.
        for offset, (size, budget) in enumerate([(s, b) for s, b in [('3b', int(5.5*GIB)), ('8b', 11*GIB)] if s in sizes]):
            alias = f'granite-4.2-{size}'
            info = json.loads((ROOT / f'reports/mlx-{size}-files.json').read_text())
            weights = models_dir / 'mlx' / info['id'].split('/')[1]
            # oMLX names models after their directory; a per-alias folder keeps one model per process.
            serve = models_dir / 'mlx' / f'{alias}-serve'
            serve.mkdir(parents=True, exist_ok=True)
            link = serve / alias
            if not link.exists():
                link.symlink_to(weights, target_is_directory=True)
            port = args.port + 15 + offset
            config['deployments'].append(dict(model=alias, backend='omlx', base_url=f'http://127.0.0.1:{port}',
                revision=info['sha'], artifact_repo=info['id'], backend_model=alias,
                command=[args.omlx, 'serve', '--model-dir', str(serve), '--base-path', str(ROOT / 'state/omlx' / alias),
                         '--host', '127.0.0.1', '--port', str(port), '--headless', '--no-hf-cache',
                         '--max-concurrent-requests', str(LANGUAGE_SLOTS),
                         # The prefix cache on disk defaults to 100 GB per model; 2.9 GB after one benchmark.
                         '--paged-ssd-cache-max-size', '10GB'],
                env={'OMLX_HEADLESS': '1'},
                prefetch_command=[shutil.which('hf') or str(ROOT / '.venv-native/bin/hf'), 'download', info['id'],
                                  '--revision', info['sha'], '--local-dir', str(weights)],
                memory_bytes=budget, device='unified', health_path='/health', identity_path='/v1/models',
                runtime_manifest=str(Path(args.omlx).resolve()), backend_build=f'omlx-{version}', context_tokens=SLOT_CONTEXT,
                artifacts={str(weights / f['path']): sha256_file(weights / f['path']) for f in info['files']},
                load_timeout_seconds=300, max_concurrency=LANGUAGE_SLOTS, keep_warm=False))
    elif sizes and (args.host == 'mac' or args.llama_runtime):
        llama = str(Path(args.llama_runtime).resolve()) if args.llama_runtime else shutil.which('llama-server')
        if not llama:
            raise SystemExit('llama-server not found; install a validated build first')
        version = subprocess.check_output([llama, '--version'], stderr=subprocess.STDOUT, text=True)
        match = re.search(r'build (\d+), commit ([0-9a-f]+)', version)
        if not match:
            raise SystemExit('Cannot identify installed llama.cpp build')
        build = f'b{match[1]}-{match[2]}'
        digest = sha256_file(llama)
        runtime_files = {llama: digest}
        # Release archives keep their libraries beside the executable; Homebrew splits them out.
        libraries = [Path(llama).resolve().parent, Path(llama).resolve().parents[1] / 'lib']
        if args.host == 'mac':
            libraries += [Path('/opt/homebrew/opt/ggml/lib'), Path('/opt/homebrew/opt/openssl@4/lib')]
        backend_device = {'vulkan': 'Vulkan0', 'cuda': 'CUDA0', 'rocm': 'ROCm0'}.get(device)
        if backend_device:
            devices = subprocess.check_output([llama, '--list-devices'], stderr=subprocess.STDOUT, text=True)
            if backend_device not in devices:
                raise SystemExit(f'The selected runtime did not report {backend_device}; no language deployment created')
        for directory in libraries:
            for library in directory.glob('*') if directory.is_dir() else []:
                if library.is_file() and ('.so' in library.name or library.suffix in ('.dylib', '.metallib')):
                    file = library.resolve()
                    runtime_files[str(file)] = sha256_file(file)
        manifest = (receipts / f'{profile}-llama-runtime.json' if args.profile else
                    ROOT / ('locks/macos-llama-runtime.json' if args.host == 'mac' else 'locks/hx-vulkan-runtime.json'))
        write(manifest, dict(backend_build=build, executable=llama, executable_sha256=digest, version_output=version, files=runtime_files))
        preset = ROOT / f'configs/{profile}-models.ini'
        preset.write_text('\n'.join(f'[granite-4.2-{size}]\nmodel = {models_dir}/granite-4.2-{size}-Q4_K_M.gguf\n' for size in sizes))
        command = [llama, '--models-preset', str(preset), '--models-max', '1', '--no-models-autoload',
                   '--host', '127.0.0.1', '--port', str(args.port + 11), '-c', str(LANGUAGE_SLOTS * SLOT_CONTEXT), '-np', str(LANGUAGE_SLOTS), '-ngl', '0' if device == 'cpu' else '99',
                   '--jinja', '--reasoning-format', 'deepseek', '--metrics']
        if backend_device:
            command += ['--device', backend_device]
        for size, budget in [(s, b) for s, b in [('3b', 5*GIB), ('8b', 9*GIB)] if s in sizes]:
            alias = f'granite-4.2-{size}'
            info = json.loads((ROOT / f'reports/gguf-{size}-files.json').read_text())
            name = f'{alias}-Q4_K_M.gguf'
            file = next(f for f in info['siblings'] if f['rfilename'] == name)
            config['deployments'].append(dict(model=alias, backend='llama_cpp', base_url=f'http://127.0.0.1:{args.port + 11}',
                revision=info['sha'], artifact_repo=info['id'], command=command,
                prefetch_command=[shutil.which('hf') or str(ROOT / '.venv-native/bin/hf'), 'download', info['id'], name, '--revision', info['sha'], '--local-dir', str(models_dir)],
                memory_bytes=budget, device='cpu' if device == 'cpu' else 'unified', health_path='/health', identity_path='/v1/models',
                runtime_manifest=str(manifest), backend_build=build,
                context_tokens=SLOT_CONTEXT,
                pid_registry=args.pid_registry,
                artifacts={str(models_dir / name): file['lfs']['sha256'], **runtime_files},
                load_timeout_seconds=300, max_concurrency=LANGUAGE_SLOTS, keep_warm=False))
    if args.native:
        python = ROOT / '.venv-native/bin/python'
        packages = subprocess.check_output([str(python), '-c',
            'import importlib.metadata,json;print(json.dumps({d.metadata["Name"].lower():d.version for d in importlib.metadata.distributions()}))'], text=True)
        packages = json.loads(packages)
        for name in ('torch', 'transformers', 'sentence-transformers', 'granite-tsfm'):
            if name not in packages:
                raise SystemExit(f'Missing installed runtime: {name}')
        manifest = receipts / f'{profile}-native-runtime.json' if args.profile else ROOT / f'locks/{args.host}-native-runtime.json'
        write(manifest, dict(python=subprocess.check_output([str(python), '--version'], text=True).strip(), packages=packages,
                             requirements_lock=str(ROOT / ('locks/macos-arm64-native.txt' if args.host == 'mac' else 'locks/linux-x86_64-cpu.txt'))))
        for alias, family, port, budget in [
            ('granite-embedding-97m-multilingual-r2', 'embedding', args.port + 12, 2*GIB),
            ('granite-timeseries-ttm-r3', 'timeseries', args.port + 13, 2*GIB),
            ('granite-speech-5.0-470m-turboctc', 'speech', args.port + 14, 4*GIB),
        ]:
            if family not in args.native_models.split(','):
                continue
            model = models[alias]
            config['deployments'].append(dict(model=alias, backend=family+'_native', base_url=f'http://127.0.0.1:{port}',
                revision=model['hf_revision'], artifact_repo=model['hf_repo'],
                command=[str(python), str(ROOT / 'workers/native.py'), '--model', alias, '--repo', model['hf_repo'],
                         '--revision', model['hf_revision'], '--family', family, '--device', 'cpu', '--threads', '4', '--port', str(port)],
                memory_bytes=budget, device='cpu', health_path='/healthz', identity_path='/internal/metadata',
                runtime_manifest=str(manifest), backend_build=f'torch-{packages["torch"]}_transformers-{packages["transformers"]}',
                prefetch_command=[str(python.parent / 'hf'), 'download', model['hf_repo'], '--revision', model['hf_revision'],
                                  '--include', '*.json', '*.safetensors', '*.txt', '*.model', '*.jinja'],
                env={'TOKENIZERS_PARALLELISM':'false', 'OMP_NUM_THREADS':'4', 'HF_HUB_DISABLE_XET':'1'},
                load_timeout_seconds=600, max_concurrency=1, artifacts={}))
    if args.host == 'cuda':
        model = models['granite-vision-4.1-4b']
        image = json.loads((ROOT / 'locks/vllm-vision-image.json').read_text())
        digest = next(x['digest'] for x in image['manifest']['manifests'] if x['platform']['architecture']=='amd64')
        cache = str(ROOT / 'state/huggingface')
        # The sidecar remains unloaded until measured free VRAM passes admission.
        config['deployments'].append(dict(model=model['id'], backend='vllm', base_url='http://127.0.0.1:8325',
            hold_reason='GPU shared with another workload; native loads require an agreed handover window.',
            revision=model['hf_revision'], artifact_repo=model['hf_repo'],
            command=['docker','run','--rm','--name','granited-vision','--device','nvidia.com/gpu=all',
                     '-p','127.0.0.1:8325:8000','--shm-size','2g','-v',cache+':/root/.cache/huggingface',
                     '--cidfile',str(ROOT / 'state/vision.cid'),'--label','org.granited.owner=granited-cuda',
                     image['image']+'@'+digest,model['hf_repo'],'--revision',model['hf_revision'],
                     '--served-model-name',model['id'],'--max-model-len','4096','--gpu-memory-utilization','0.60',
                     '--limit-mm-per-prompt','{"image":1}'],
            memory_bytes=11000000000, device='cuda', health_path='/health', identity_path='/v1/models',
            container_id_file=str(ROOT / 'state/vision.cid'),
            runtime_manifest=str(ROOT / 'locks/vllm-vision-image.json'), backend_build='vllm-'+image['tag'],
            context_tokens=4096,
            load_timeout_seconds=900, max_concurrency=1, artifacts={}))
    previous_path = ROOT / f'configs/{profile}.json'
    if previous_path.exists():
        previous = json.loads(previous_path.read_text())
        config['peers'] = previous.get('peers', [])
        config['host_id'] = previous.get('host_id', config['host_id'])
        old = {d['model']: d for d in previous.get('deployments', [])}
        for deployment in config['deployments']:
            for key in ('keep_warm', 'hold_reason'):
                if key in old.get(deployment['model'], {}):
                    deployment[key] = old[deployment['model']][key]
        config['deployments'] += [d for d in old.values() if d.get('external') and d['model'] not in {w['model'] for w in config['deployments']}]
    write(previous_path, config)
    print(f'configs/{profile}.json; memory budgets are conservative configured bounds, not measured footprints')


if __name__ == '__main__':
    main()
