#!/usr/bin/env python3
"""Resolve official cards/configuration at immutable Hub revisions; never load code."""
import concurrent.futures
import datetime
import json
from pathlib import Path
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
CANDIDATES = json.loads((ROOT / 'benchmarks/configs/models.json').read_text())
CAPABILITIES = {
    'language': ['chat'], 'vision': ['chat', 'vision'], 'speech': ['transcription'],
    'time_series': ['forecast'], 'embedding': ['embeddings', 'retrieval'],
    'rerank': ['rerank'], 'document': ['documents'], 'guardian': ['guardian'],
    'switch': ['chat', 'adapters'], 'geospatial': ['geospatial'],
}

def fetch(url):
    with urllib.request.urlopen(urllib.request.Request(url, headers={'User-Agent': 'granited/0.1'}), timeout=45) as response:
        return response.read()

def resolve(item):
    family, repo = item
    entry = dict(id=repo.split('/')[-1], hf_repo=repo, family=family,
                 capabilities=CAPABILITIES[family], hf_revision=None, architecture=None,
                 license=None, context_limit=None, processor=None, memory_bytes=None,
                 idle_timeout_seconds=300, runtime_candidates=[], validation={
                     'model_card_checked': False, 'backend_smoke_test_passed': False,
                     'benchmarked': False}, incompatibilities=[])
    if 'tspulse' in repo:
        entry['capabilities'] = ['anomalies', 'imputation', 'classification']
    try:
        meta = json.loads(fetch('https://huggingface.co/api/models/' + repo))
        sha = meta['sha']
        assert len(sha) == 40 and all(c in '0123456789abcdef' for c in sha)
        directory = ROOT / 'reports/model-cards' / entry['id'] / sha
        directory.mkdir(parents=True, exist_ok=True)
        (directory / 'hub.json').write_text(json.dumps(meta, indent=2))
        files = {f['rfilename'] for f in meta.get('siblings', [])}
        for name in ('README.md', 'config.json', 'preprocessor_config.json', 'chat_template.jinja', 'generation_config.json'):
            if name in files:
                (directory / name).write_bytes(fetch(f'https://huggingface.co/{repo}/resolve/{sha}/{name}'))
        cfg = json.loads((directory / 'config.json').read_text()) if 'config.json' in files else {}
        entry.update(hf_revision=sha, architecture=cfg.get('architectures') or cfg.get('model_type'),
                     license=(meta.get('cardData') or {}).get('license'),
                     context_limit=cfg.get('max_position_embeddings'),
                     processor=(meta.get('transformersInfo') or {}).get('processor'),
                     card_path=str(directory.relative_to(ROOT)),
                     parameter_count=(meta.get('safetensors') or {}).get('total'))
        entry['validation']['model_card_checked'] = 'README.md' in files
        if cfg.get('auto_map'):
            entry['incompatibilities'].append('Custom code mapping present; remote code disabled pending source review.')
        entry['runtime_candidates'] = {
            'language': ['llama_cpp', 'mlx', 'vllm'], 'vision': ['vllm'],
            'speech': ['transformers', 'mlx_audio'], 'time_series': ['tsfm'],
            'embedding': ['sentence_transformers', 'onnx'], 'document': ['docling'],
            'rerank': ['transformers'], 'guardian': ['transformers', 'vllm'],
            'switch': ['vllm_switch_isolated'], 'geospatial': ['task_specific'],
        }[family]
        if family == 'switch':
            entry['incompatibilities'].append('Separate environment and explicit adapter activation required; card constraints must be checked against resolved runtime.')
        if family == 'geospatial':
            entry['incompatibilities'].append('Discovery only: bands, normalization, task head and runtime not validated; execution disabled.')
    except Exception as exc:
        entry['incompatibilities'].append(f'{type(exc).__name__}: {exc}')
    return entry

def main():
    candidates = [(family, repo) for family, repos in CANDIDATES.items() for repo in repos]
    # Inventory the official organization rather than guessing a geospatial repository.
    official = json.loads(fetch('https://huggingface.co/api/models?author=ibm-granite&limit=1000'))
    (ROOT / 'reports/official-inventory.json').write_text(json.dumps(official, indent=2))
    known = {repo for _, repo in candidates}
    for model in official:
        repo = model['id']
        if any(term in repo.lower() for term in ('prithvi', 'geospatial')) and repo not in known:
            candidates.append(('geospatial', repo))
    with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
        models = list(pool.map(resolve, candidates))
    output = {'resolved_at': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'models': models}
    (ROOT / 'configs/registry.json').write_text(json.dumps(output, indent=2) + '\n')
    print(json.dumps([{'id': m['id'], 'revision': m['hf_revision'], 'architecture': m['architecture'], 'issues': m['incompatibilities']} for m in models], indent=2))

if __name__ == '__main__':
    main()
