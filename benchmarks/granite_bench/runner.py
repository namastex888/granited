"""HTTP measurements against native workers, Rust, and LiteLLM. Smoke is not accuracy."""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import datetime
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import statistics
import sys
import time
import urllib.error
import urllib.request
from . import metrics as m


def evaluate(family, expected, response):
    if family == 'language':
        return {'exact_match': float(m.norm(response.get('text', '')) == m.norm(expected['answer'])),
                'reasoning_overhead_chars': len(response.get('reasoning', '')),
                'reasoning_overhead_tokens': response.get('reasoning_tokens')}
    if family == 'vision':
        return {'structural_table_f1': None, 'unsupported_metric': 'Structural table evaluator is not implemented'}
    if family == 'speech':
        return {'wer': m.wer(response.get('text', ''), expected['transcript']),
                'language_correct': float(response['language'] == expected['language']) if response.get('language') is not None else None}
    if family == 'timeseries':
        if len(response['forecast']) != len(expected['future']):
            raise ValueError('Forecast/target lengths differ')
        result = {'mase': m.mase(response['forecast'], expected['future'], expected['history'], expected.get('seasonality', 1))}
        if 'quantiles' in response:
            if any(len(values) != len(expected['future']) for values in response['quantiles'].values()):
                raise ValueError('Quantile/target lengths differ')
            result.update(wql=m.wql(response['quantiles'], expected['future']), empirical_coverage=m.calibration(response['quantiles'], expected['future']))
        return result
    if family in ('embedding', 'rerank'):
        return {'ndcg@10': m.ndcg(response.get('ranked_ids', []), expected['relevance']),
                'recall@10': m.recall(response.get('ranked_ids', []), expected['relevance'])}
    if family == 'documents':
        return {'structural_table_f1': None, 'reading_order_exact': float(response.get('reading_order', []) == expected.get('reading_order', []))}
    if family == 'guardian':
        return {'predicted': int(bool(response['flagged'])), 'truth': int(bool(expected['flagged']))}
    if family == 'geospatial':
        predicted, target = response['mask'], expected['mask']
        if len(predicted) != len(target):
            raise ValueError('Raster shapes differ')
        intersection = sum(a == 1 and b == 1 for a, b in zip(predicted, target))
        union = sum(a == 1 or b == 1 for a, b in zip(predicted, target))
        return {'binary_iou': intersection / union if union else 1.0, 'pixels': len(target)}
    raise ValueError(f'Unsupported scoring family {family}')


def read_sse(stream, dispatch_start):
    first = first_answer = None
    text, reasoning, events, data_lines = [], [], [], []
    completion_tokens = reasoning_tokens = None
    done = False

    def consume(lines):
        nonlocal first, first_answer, completion_tokens, reasoning_tokens, done
        if not lines:
            return
        data = '\n'.join(lines)
        if data == '[DONE]':
            done = True
            return
        obj = json.loads(data)
        if 'error' in obj:
            raise ValueError(f'SSE error: {obj["error"]}')
        events.append(obj)
        for choice in obj.get('choices', []):
            delta = choice.get('delta') or {}
            answer = delta.get('content') or ''
            thought = delta.get('reasoning_content') or delta.get('reasoning') or ''
            if answer or thought:
                now = time.perf_counter() - dispatch_start
                if first is None:
                    first = now
                if answer and first_answer is None:
                    first_answer = now
                text.append(answer)
                reasoning.append(thought)
        usage = obj.get('usage') or {}
        completion_tokens = usage.get('completion_tokens', completion_tokens)
        reasoning_tokens = (usage.get('completion_tokens_details') or {}).get('reasoning_tokens', reasoning_tokens)

    for raw in stream:
        line = raw.decode('utf-8').rstrip('\r\n')
        if not line:
            consume(data_lines)
            data_lines = []
            if done:
                break
        elif line.startswith('data:'):
            data_lines.append(line[5:].lstrip(' '))
    consume(data_lines)
    if not done:
        raise ValueError('SSE stream ended without [DONE]')
    return {'text': ''.join(text), 'reasoning': ''.join(reasoning), 'completion_tokens': completion_tokens,
            'reasoning_tokens': reasoning_tokens, 'first_answer_s': first_answer, 'raw_events': events}, first


def request(base, record, timeout, token):
    body = dict(record['request'])
    if record.get('stream'):
        body['stream'] = True
        body.setdefault('stream_options', {'include_usage': True})
    headers = {'Content-Type': 'application/json'}
    if token:
        headers['Authorization'] = 'Bearer ' + token
    req = urllib.request.Request(base.rstrip('/') + '/' + record['path'].lstrip('/'), json.dumps(body).encode(), headers, method=record.get('method', 'POST'))
    start = time.perf_counter()
    with urllib.request.urlopen(req, timeout=timeout) as response:
        if record.get('stream'):
            result, first = read_sse(response, start)
        else:
            data = json.load(response)
            first = None
            if 'choices' in data:
                message = data['choices'][0].get('message') or {}
                usage = data.get('usage') or {}
                result = {'text': message.get('content') or '', 'reasoning': message.get('reasoning_content') or message.get('reasoning') or '',
                          'completion_tokens': usage.get('completion_tokens'),
                          'reasoning_tokens': (usage.get('completion_tokens_details') or {}).get('reasoning_tokens'), 'raw': data}
            else:
                result = data
        result['response_headers'] = {key.lower(): value for key, value in response.headers.items()
                                      if key.lower().startswith('x-granite-') or key.lower() == 'server-timing'}
    return result, time.perf_counter() - start, first


def percentile(values, q):
    ordered = sorted(values)
    return ordered[max(0, math.ceil(q * len(ordered)) - 1)] if ordered else None


def mean_ci(values):
    if len(values) < 2:
        return None
    rng = random.Random(0)
    means = [statistics.mean(rng.choices(values, k=len(values))) for _ in range(1000)]
    return [percentile(means, .025), percentile(means, .975)]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--base-url', required=True)
    parser.add_argument('--fixture', required=True)
    parser.add_argument('--output', default='results.json')
    parser.add_argument('--repeat', type=int, default=30)
    parser.add_argument('--warmup', type=int, default=1)
    parser.add_argument('--concurrency', type=int, default=1)
    parser.add_argument('--timeout', type=float, default=600)
    parser.add_argument('--token-env', default='BENCH_API_KEY')
    parser.add_argument('--stage', choices=['direct', 'rust', 'litellm'], required=True)
    parser.add_argument('--kind', choices=['smoke', 'dataset'], default='smoke')
    parser.add_argument('--typed-prefix', default='')
    parser.add_argument('--dataset-manifest')
    parser.add_argument('--hardware-file')
    args = parser.parse_args()
    if args.repeat < 1 or args.concurrency < 1 or args.warmup < 0:
        parser.error('repeat/concurrency must be positive and warmup nonnegative')
    manifest = json.loads(Path(args.dataset_manifest).read_text()) if args.dataset_manifest else None
    if args.kind == 'dataset':
        if not manifest or any(not manifest.get(field) for field in ['repo', 'revision', 'preprocessing_sha256', 'split']):
            parser.error('Dataset scoring requires repo, immutable revision, split and preprocessing_sha256')
        revision = manifest['revision']
        if len(revision) != 40 or any(c not in '0123456789abcdef' for c in revision):
            parser.error('Dataset revision must be a resolved 40-character commit')
    fixture = Path(args.fixture)
    records = [json.loads(line) for line in fixture.read_text().splitlines() if line.strip()]
    for record in records:
        if args.typed_prefix and record['path'].lstrip('/') not in ['v1/chat/completions', 'v1/embeddings']:
            record['path'] = args.typed_prefix.rstrip('/') + '/' + record['path'].lstrip('/')
    token = os.environ.get(args.token_env)

    def run(record, repetition):
        result = {'id': record['id'], 'family': record['family'], 'stage': args.stage, 'repetition': repetition,
                  'kind': args.kind, 'profile': record.get('profile'), 'model': record['request'].get('model')}
        try:
            response, elapsed, first = request(args.base_url, record, args.timeout, token)
            metrics = evaluate(record['family'], record['expected'], response) if args.kind == 'dataset' else None
            thought = response.get('reasoning', '')
            content = response.get('text', '')
            result.update(status='ok', latency_s=elapsed, ttft_s=first, first_answer_s=response.get('first_answer_s'), response=response,
                          quality_metrics=metrics, reasoning_chars=len(thought), reasoning_tokens=response.get('reasoning_tokens'),
                          visible_think_tags='<think>' in content or '</think>' in content,
                          unwanted_reasoning=bool(thought or '<think>' in content or '</think>' in content) if record.get('profile') == 'reasoning_off' else None,
                          output_tokens_per_second_e2e=response['completion_tokens'] / elapsed if response.get('completion_tokens') is not None else None)
        except urllib.error.HTTPError as error:
            raw = error.read().decode('utf-8', errors='replace')
            try:
                detail = json.loads(raw)
            except ValueError:
                detail = raw
            result.update(status='error', http_status=error.code, error=detail)
        except Exception as error:
            result.update(status='error', error=f'{type(error).__name__}: {error}')
        return result

    for record in records:
        for _ in range(args.warmup):
            result = run(record, -1)
            if result['status'] != 'ok':
                print(f'Warmup failed: {result}', file=sys.stderr)
                break
    raw_path = Path(args.output).with_suffix('.requests.jsonl')
    results = []
    started = time.perf_counter()
    with raw_path.open('w') as raw, ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        jobs = [pool.submit(run, record, i) for record in records for i in range(args.repeat)]
        for job in as_completed(jobs):
            result = job.result()
            results.append(result)
            raw.write(json.dumps(result, ensure_ascii=False) + '\n')
            raw.flush()
    wall = time.perf_counter() - started
    successful = [r for r in results if r['status'] == 'ok']
    latencies = [r['latency_s'] for r in successful]
    summary = dict(total=len(results), successful=len(successful), failed=len(results) - len(successful),
                   wall_seconds=wall, achieved_requests_per_second=len(successful)/wall,
                   throughput_kind='achieved at configured concurrency; not a saturation claim',
                   latency_p50_s=percentile(latencies, .5), latency_p95_s=percentile(latencies, .95),
                   latency_p99_s=percentile(latencies, .99), mean_latency_95ci_s=mean_ci(latencies),
                   model_accuracy=None if args.kind == 'smoke' else 'see per-request quality_metrics')
    output = dict(schema_version=2, kind=args.kind, stage=args.stage, timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  platform=platform.platform(), python=sys.version, hostname=platform.node(), base_url=args.base_url,
                  fixture=str(fixture), fixture_sha256=hashlib.sha256(fixture.read_bytes()).hexdigest(), dataset_manifest=manifest,
                  concurrency=args.concurrency, repeat=args.repeat, warmup=args.warmup,
                  hardware=json.loads(Path(args.hardware_file).read_text()) if args.hardware_file else None,
                  summary=summary, raw_requests=str(raw_path))
    Path(args.output).write_text(json.dumps(output, indent=2, ensure_ascii=False))
    print(json.dumps(summary, indent=2))
    return int(bool(summary['failed']))


if __name__ == '__main__':
    raise SystemExit(main())
