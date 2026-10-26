#!/usr/bin/env python3
"""Qwen3-TTS Base (Apache-2.0) behind granited's /v1/audio/speech: in-context voice clone, WAV out.

  tts_qwen.py --weights DIR --model qwen3-tts-0.6b-base --revision SHA --port N [--device cpu|mps|cuda]

There is no default voice: every request carries 1 to 4 references (audio and its transcript), and
the first is the voice. Reference audio is decoded here from base64 and handed to the model as a
waveform. It is never passed as a string, because qwen-tts reads a string as a file path or a URL.
It stays in memory for that one request and is never written, cached or logged.
"""
import argparse
import base64
import io
import json
import sys
import threading
import time
import wave
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import numpy as np
import soundfile as sf


def parse():
    p = argparse.ArgumentParser()
    p.add_argument('--weights', required=True, help='local directory with the pinned revision')
    p.add_argument('--model', required=True)
    p.add_argument('--revision', required=True)
    p.add_argument('--port', type=int, required=True)
    p.add_argument('--device', default='cpu', choices=['cpu', 'mps', 'cuda'])
    p.add_argument('--max-input-chars', type=int, default=1000)
    return p.parse_args()


def load(args):
    import torch
    from qwen_tts import Qwen3TTSModel

    dtype = {'cpu': torch.float32, 'mps': torch.float16, 'cuda': torch.bfloat16}[args.device]
    return Qwen3TTSModel.from_pretrained(args.weights, device_map=args.device, dtype=dtype)


def reference(clip):
    try:
        audio, rate = sf.read(io.BytesIO(base64.b64decode(clip['audio_base64'], validate=True)), dtype='float32')
    except (ValueError, sf.LibsndfileError):
        raise ValueError('reference audio is not a readable wav, flac or ogg file') from None
    if audio.ndim == 2:
        audio = audio.mean(axis=1)
    if not 1.0 <= len(audio) / rate <= 30.0:
        raise ValueError('a reference clip must be 1 to 30 seconds long')
    return audio, rate


def wav_bytes(rate, audio):
    pcm = (np.clip(np.asarray(audio, dtype=np.float32).reshape(-1), -1, 1) * 32767).astype('<i2')
    buf = io.BytesIO()
    with wave.open(buf, 'wb') as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(rate)
        w.writeframes(pcm.tobytes())
    return buf.getvalue(), len(pcm) / rate


def synthesize(model, args, body):
    import torch

    text = body.get('input', '')
    if not text.strip() or len(text) > args.max_input_chars:
        raise ValueError(f'input must be 1 to {args.max_input_chars} characters')
    refs = body.get('references') or []
    if not 1 <= len(refs) <= 4:
        raise ValueError('Qwen3-TTS Base has no default voice: send 1 to 4 references (audio and transcript)')
    audio, rate = reference(refs[0])
    if body.get('seed') is not None:
        torch.manual_seed(int(body['seed']))
    options = {k: body[k] for k in ('temperature', 'top_p', 'max_new_tokens') if body.get(k) is not None}
    start = time.perf_counter()
    wavs, out_rate = model.generate_voice_clone(text=text, ref_audio=(audio, rate), ref_text=refs[0]['text'], **options)
    elapsed = time.perf_counter() - start
    data, seconds = wav_bytes(out_rate, wavs[0])
    return data, dict(synthesis_ms=round(elapsed * 1000, 1), audio_ms=round(seconds * 1000, 1),
                      rtf=round(elapsed / seconds, 3) if seconds else None)


def serve(args, model):
    lock = threading.Lock()
    identity = json.dumps({'model': args.model, 'revision': args.revision, 'backend': 'qwen-tts-0.1.1',
                           'device': args.device}).encode()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'
        disable_nagle_algorithm = True

        def reply(self, status, payload, kind='application/json', extra=()):
            self.send_response(status)
            self.send_header('Content-Type', kind)
            self.send_header('Content-Length', str(len(payload)))
            for name, value in extra:
                self.send_header(name, value)
            self._headers_buffer.append(b'\r\n' + payload)
            self.flush_headers()

        def error(self, status, code, message):
            self.reply(status, json.dumps({'error': {'type': 'granite_error', 'code': code, 'message': message}}).encode())

        def do_GET(self):
            if self.path == '/health':
                self.reply(200, b'{"status":"ok"}')
            elif self.path == '/identity':
                self.reply(200, identity)
            else:
                self.error(404, 'route_not_found', 'unknown route')

        def do_POST(self):
            if self.path != '/v1/audio/speech':
                return self.error(404, 'route_not_found', 'unknown route')
            try:
                body = json.loads(self.rfile.read(int(self.headers.get('Content-Length', 0))) or b'{}')
            except ValueError:
                return self.error(400, 'invalid_schema', 'body is not JSON')
            if not lock.acquire(blocking=False):
                return self.error(429, 'admission_busy', 'one synthesis at a time')
            try:
                data, timing = synthesize(model, args, body)
            except ValueError as e:
                return self.error(400, 'invalid_schema', str(e))
            except Exception as e:
                return self.error(500, 'inference_failed', type(e).__name__)
            finally:
                lock.release()
            self.reply(200, data, 'audio/wav', [(
                'Server-Timing', f'synthesis;dur={timing["synthesis_ms"]}, audio;dur={timing["audio_ms"]}, rtf;desc="{timing["rtf"]}"')])

        def log_message(self, fmt, *a):
            # Request lines only: no body, so no reference audio or text reaches the log.
            print(fmt % a, file=sys.stderr, flush=True)

    ThreadingHTTPServer(('127.0.0.1', args.port), Handler).serve_forever()


def main():
    args = parse()
    model = load(args)
    print(f'qwen3-tts worker ready on 127.0.0.1:{args.port} ({args.model}, {args.device})', file=sys.stderr, flush=True)
    serve(args, model)


if __name__ == '__main__':
    main()
