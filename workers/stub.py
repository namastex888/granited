#!/usr/bin/env python3
"""Deterministic stand-in worker for tests and development. It loads no model and serves no real inference."""
import argparse
import base64
import hashlib
import json
import math
import time
from http.server import ThreadingHTTPServer

import native
from native import Unsupported, serve


class Events(list):
    """Server-sent events the stub returns in place of one JSON body."""


def streaming(handler):
    """native.serve's handler, able to send a streamed chat reply as server-sent events."""
    class Handler(handler):
        def send(self, status, data):
            if not isinstance(data, Events):
                return super().send(status, data)
            payload = b''.join(b'data: ' + (e if isinstance(e, str) else json.dumps(e)).encode() + b'\n\n' for e in data)
            self.send_response(status)
            self.send_header('Content-Type', 'text/event-stream')
            self.send_header('Content-Length', str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
    return Handler


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
            parts = [p for m in data.get('messages', []) if isinstance(m.get('content'), list) for p in m['content']]
            seen = dict(model=data['model'], temperature=data.get('temperature'), max_tokens=data.get('max_tokens'),
                        thinking_budget_tokens=data.get('thinking_budget_tokens'),
                        chat_template_kwargs=data.get('chat_template_kwargs'), reasoning_effort=data.get('reasoning_effort'),
                        parts=[p.get('type') for p in parts], texts=[p['text'] for p in parts if p.get('type') == 'text'])
            if 'ARRAY' in text:
                # A 2xx whose top level is not an object: the router passes it and reads no usage.
                return [dict(prompt_tokens=9)]
            if data.get('stream'):
                # Like llama.cpp and vLLM: usage arrives in a final chunk with no choices, only when asked.
                chunk = lambda **delta: dict(object='chat.completion.chunk', model=self.args.model, choices=[
                    dict(index=0, delta=delta, finish_reason=None if delta else 'stop')])
                # HUGE: one event far past the router's 64 KiB event bound.
                content = 'x' * (256 << 10) if 'HUGE' in text else json.dumps(seen)
                events = Events([chunk(role='assistant'), chunk(content=content), chunk()])
                if (data.get('stream_options') or {}).get('include_usage'):
                    events.append(dict(object='chat.completion.chunk', model=self.args.model, choices=[],
                                       usage=dict(prompt_tokens=3, completion_tokens=2, total_tokens=5,
                                                  prompt_tokens_details=dict(cached_tokens=1))))
                return Events(events + ['[DONE]'])
            return dict(object='chat.completion', model=self.args.model,
                        choices=[dict(index=0, finish_reason='stop', message=dict(role='assistant', content=json.dumps(seen)))],
                        usage=dict(prompt_tokens=1, completion_tokens=1, total_tokens=2))
        if path == '/v1/audio/transcriptions' and self.args.family == 'speech':
            audio = base64.b64decode(data['audio_base64'], validate=True)
            return dict(model=self.args.model, text=f'stub heard {len(audio)} bytes')
        if path == '/v1/audio/speech' and self.args.family == 'speech_synthesis':
            return dict(model=self.args.model, stub_spoke=len(data['input']), references=len(data.get('references') or []))
        raise Unsupported(f'Stub does not implement {path}')


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--model', required=True)
    p.add_argument('--repo', default='stub')
    p.add_argument('--revision', required=True)
    p.add_argument('--family', choices=['embedding', 'timeseries', 'language', 'speech', 'speech_synthesis'], default='embedding')
    p.add_argument('--host', default='127.0.0.1', choices=['127.0.0.1'])
    p.add_argument('--port', type=int, required=True)
    p.add_argument('--delay-ms', type=int, default=0)
    args = p.parse_args()
    native.ThreadingHTTPServer = lambda address, handler: ThreadingHTTPServer(address, streaming(handler))
    serve(Stub(args), args)


if __name__ == '__main__':
    main()
