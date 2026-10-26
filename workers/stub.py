#!/usr/bin/env python3
"""Deterministic stand-in worker for tests and development. It loads no model and serves no real inference."""
import argparse
import hashlib
import json
import math
import time

from native import Unsupported, serve


class Stub:
    def __init__(self, args):
        self.args = args
        self.started = time.perf_counter()

    def metadata(self):
        return dict(model=self.args.model, repo=self.args.repo, revision=self.args.revision,
                    family=self.args.family, backend=self.args.family + '_native', device='cpu',
                    versions={}, load_seconds=0.0, stub=True, capabilities=dict(dimensions=8))

    def vector(self, text):
        digest = hashlib.sha256(text.encode()).digest()
        values = [b - 127.5 for b in digest[:8]]
        norm = math.sqrt(sum(v * v for v in values)) or 1.0
        return [v / norm for v in values]

    def infer(self, path, data):
        if data.get('model') != self.args.model:
            raise ValueError('Model alias differs from loaded artifact')
        if self.args.delay_ms:
            time.sleep(self.args.delay_ms / 1000)
        if path == '/v1/embeddings':
            texts = data.get('input')
            texts = [texts] if isinstance(texts, str) else texts
            if not isinstance(texts, list) or not texts:
                raise ValueError('input requires 1..1024 texts')
            return dict(object='list', model=self.args.model,
                        data=[dict(object='embedding', index=i, embedding=self.vector(t)) for i, t in enumerate(texts)],
                        usage=dict(prompt_tokens=0, total_tokens=0))
        if path == '/v1/forecast':
            history = data['history']
            return dict(model=self.args.model, forecast=[history[-1]] * int(data.get('horizon', 1)))
        if path == '/v1/chat/completions' and self.args.family == 'language':
            text = ' '.join(str(m.get('content', '')) for m in data.get('messages', []))
            if 'SLOW' in text:
                time.sleep(2)
            # Echo what reached the engine, so tests can check effort, budget and sampling.
            seen = dict(model=data['model'], temperature=data.get('temperature'), max_tokens=data.get('max_tokens'),
                        thinking_budget_tokens=data.get('thinking_budget_tokens'),
                        chat_template_kwargs=data.get('chat_template_kwargs'))
            return dict(object='chat.completion', model=self.args.model,
                        choices=[dict(index=0, finish_reason='stop', message=dict(role='assistant', content=json.dumps(seen)))],
                        usage=dict(prompt_tokens=1, completion_tokens=1, total_tokens=2))
        raise Unsupported(f'Stub does not implement {path}')


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--model', required=True)
    p.add_argument('--repo', default='stub')
    p.add_argument('--revision', required=True)
    p.add_argument('--family', choices=['embedding', 'timeseries', 'language'], default='embedding')
    p.add_argument('--host', default='127.0.0.1', choices=['127.0.0.1'])
    p.add_argument('--port', type=int, required=True)
    p.add_argument('--delay-ms', type=int, default=0)
    args = p.parse_args()
    serve(Stub(args), args)


if __name__ == '__main__':
    main()
