#!/usr/bin/env python3
"""Qwen3-TTS Base (Apache-2.0) behind granited's /v1/audio/speech: in-context voice clone, WAV out.

  tts_qwen.py --weights DIR --model qwen3-tts-0.6b-base --revision SHA --port N [--device cpu|mps|cuda]

There is no default voice: every request carries 1 to 4 references (audio and its transcript), and
the first is the voice. Reference audio is decoded here from base64 and handed to the model as a
waveform. It is never passed as a string, because qwen-tts reads a string as a file path or a URL.
It stays in memory for that one request and is never written, cached or logged.

With "stream": true the reply is a WAV header with an unknown length, then 16-bit PCM as it is
generated: each step's codes are caught by a hook on the talker and decoded in small chunks, each
with the preceding frames as left context (the codec decoder is causal).

--runtime faster uses faster-qwen3-tts 0.5.4 (MIT), which captures the talker and code-predictor
loops in CUDA graphs. The voice prompt is still built here from the in-memory waveform and passed
as `voice_clone_prompt`, so the library's path-keyed prompt cache is never used.
It needs transformers 5.15.1: 5.20.2 breaks qwen-tts-hf's rope compatibility shim
(`MimiConfig` has no `rope_theta`), and 4.x lacks `standardize_rope_params`.
"""
import argparse
import base64
import queue
import struct
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
    p.add_argument('--runtime', default='reference', choices=['reference', 'faster'])
    p.add_argument('--chunk-frames', type=int, default=4, help='faster runtime: frames per streamed chunk')
    args = p.parse_args()
    if args.runtime == 'faster' and args.device != 'cuda':
        p.error('--runtime faster captures CUDA graphs; it needs --device cuda')
    return args


def load(args):
    import torch
    from qwen_tts import Qwen3TTSModel

    dtype = {'cpu': torch.float32, 'mps': torch.float16, 'cuda': torch.bfloat16}[args.device]
    if args.runtime == 'faster':
        from faster_qwen3_tts import FasterQwen3TTS

        return FasterQwen3TTS.from_pretrained(args.weights, device=args.device, dtype=dtype)
    return Qwen3TTSModel.from_pretrained(args.weights, device_map=args.device, dtype=dtype)


def faster_prompt(engine, ref_audio, ref_text):
    """The library's ICL prompt, built from memory: 0.5 s of trailing silence keeps the reference's
    last phoneme out of the start of the speech, as the library does for a file."""
    audio, rate = ref_audio
    audio = np.concatenate([audio, np.zeros(int(0.5 * rate), dtype=np.float32)])
    items = engine.model.create_voice_clone_prompt(ref_audio=(audio, rate), ref_text=ref_text)
    return engine.model._prompt_items_to_voice_clone_prompt(items)


def faster_options(body):
    options = {k: body[k] for k in ('temperature', 'top_p', 'max_new_tokens') if body.get(k) is not None}
    return options


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


def prepare(model, args, body):
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
    return text, (audio, rate), refs[0]['text'], options


FIRST_FRAMES, CHUNK_FRAMES, LEFT_CONTEXT = 4, 12, 25


def stream(model, args, body, emit):
    """Generate on a thread; decode codes as they arrive and pass PCM bytes to `emit`."""
    import torch

    text, ref_audio, ref_text, options = prepare(model, args, body)
    if args.runtime == 'faster':
        start, first, frames = time.perf_counter(), None, 0
        prompt = faster_prompt(model, ref_audio, ref_text)
        for chunk, _rate, _timing in model.generate_voice_clone_streaming(
                text=text, language='Auto', voice_clone_prompt=prompt, ref_text=ref_text,
                chunk_size=args.chunk_frames, **options):
            emit((np.clip(np.asarray(chunk, dtype=np.float32).reshape(-1), -1, 1) * 32767).astype('<i2').tobytes())
            first = first or time.perf_counter() - start
            frames += 1
        return dict(first_audio_ms=round((first or 0) * 1000, 1),
                    synthesis_ms=round((time.perf_counter() - start) * 1000, 1), frames=frames)
    prompt = model.create_voice_clone_prompt(ref_audio=ref_audio, ref_text=ref_text)
    ref_code = prompt[0].ref_code
    eos = model.model.config.talker_config.codec_eos_token_id
    codes, done = queue.Queue(), object()

    def hook(_module, _inputs, output):
        step = output.hidden_states[-1] if output.hidden_states else None
        if step is not None:
            codes.put(step[0].detach())

    handle = model.model.talker.register_forward_hook(hook)
    failure = []

    def run():
        try:
            model.generate_voice_clone(text=text, voice_clone_prompt=prompt, **options)
        except Exception as e:  # reported after the stream ends
            failure.append(e)
        finally:
            codes.put(done)

    worker = threading.Thread(target=run, daemon=True)
    start = time.perf_counter()
    worker.start()
    history = [] if ref_code is None else list(ref_code.to('cpu'))
    base, sent, first, finished = len(history), 0, None, False
    try:
        while not finished:
            item = codes.get()
            if item is done:
                finished = True
            elif int(item[0]) == eos:
                finished = True
            else:
                history.append(item.to('cpu'))
            ready = len(history) - base - sent
            if ready < (FIRST_FRAMES if sent == 0 else CHUNK_FRAMES) and not (finished and ready > 0):
                continue
            begin = base + sent
            context = min(LEFT_CONTEXT, begin)
            window = torch.stack(history[begin - context:])
            wav, rate = model.model.speech_tokenizer.decode([{'audio_codes': window}])
            wav = np.asarray(wav[0], dtype=np.float32).reshape(-1)
            per_frame = len(wav) // len(window)
            emit((np.clip(wav[context * per_frame:], -1, 1) * 32767).astype('<i2').tobytes())
            if first is None:
                first = time.perf_counter() - start
            sent = len(history) - base
    finally:
        handle.remove()
    worker.join()
    if failure:
        raise failure[0]
    return dict(first_audio_ms=round((first or 0) * 1000, 1), synthesis_ms=round((time.perf_counter() - start) * 1000, 1),
                frames=sent)


def wav_header(rate):
    """A mono 16-bit WAV header whose sizes say 'unknown', for a stream of PCM."""
    return (b'RIFF' + struct.pack('<I', 0xFFFFFFFF) + b'WAVEfmt ' + struct.pack('<IHHIIHH', 16, 1, 1, rate, rate * 2, 2, 16)
            + b'data' + struct.pack('<I', 0xFFFFFFFF))


def synthesize(model, args, body):
    text, ref_audio, ref_text, options = prepare(model, args, body)
    start = time.perf_counter()
    if args.runtime == 'faster':
        wavs, out_rate = model.generate_voice_clone(
            text=text, language='Auto', voice_clone_prompt=faster_prompt(model, ref_audio, ref_text),
            ref_text=ref_text, **options)
    else:
        wavs, out_rate = model.generate_voice_clone(text=text, ref_audio=ref_audio, ref_text=ref_text, **options)
    elapsed = time.perf_counter() - start
    data, seconds = wav_bytes(out_rate, wavs[0])
    return data, dict(synthesis_ms=round(elapsed * 1000, 1), audio_ms=round(seconds * 1000, 1),
                      rtf=round(elapsed / seconds, 3) if seconds else None)


def serve(args, model):
    lock = threading.Lock()
    backend = 'faster-qwen3-tts-0.5.4' if args.runtime == 'faster' else 'qwen-tts-0.1.1'
    identity = json.dumps({'model': args.model, 'revision': args.revision, 'backend': backend,
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
            if body.get('stream'):
                return self.streamed(body, lock)
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

        def streamed(self, body, lock):
            """Chunked transfer: the header now, PCM as it is decoded. An error after the header can
            only end the stream early; it is logged by type, never with the request's content."""
            started = False

            def emit(pcm):
                nonlocal started
                if not started:
                    self.send_response(200)
                    self.send_header('Content-Type', 'audio/wav')
                    self.send_header('Transfer-Encoding', 'chunked')
                    self.end_headers()
                    self.chunk(wav_header(SAMPLE_RATE))
                    started = True
                self.chunk(pcm)

            try:
                timing = stream(model, args, body, emit)
                print(f'streamed: first audio {timing["first_audio_ms"]} ms, total {timing["synthesis_ms"]} ms, '
                      f'{timing["frames"]} frames', file=sys.stderr, flush=True)
            except ValueError as e:
                if not started:
                    return self.error(400, 'invalid_schema', str(e))
            except Exception as e:
                print(f'stream failed: {type(e).__name__}', file=sys.stderr, flush=True)
                if not started:
                    return self.error(500, 'inference_failed', type(e).__name__)
            finally:
                lock.release()
            if started:
                self.wfile.write(b'0\r\n\r\n')
                self.wfile.flush()

        def chunk(self, data):
            self.wfile.write(f'{len(data):x}\r\n'.encode() + data + b'\r\n')
            self.wfile.flush()

        def log_message(self, fmt, *a):
            # Request lines only: no body, so no reference audio or text reaches the log.
            print(fmt % a, file=sys.stderr, flush=True)

    ThreadingHTTPServer(('127.0.0.1', args.port), Handler).serve_forever()


SAMPLE_RATE = 24000


def main():
    args = parse()
    model = load(args)
    print(f'qwen3-tts worker ready on 127.0.0.1:{args.port} ({args.model}, {args.device})', file=sys.stderr, flush=True)
    serve(args, model)


if __name__ == '__main__':
    main()
