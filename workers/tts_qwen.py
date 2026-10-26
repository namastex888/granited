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

--runtime mlx (Apple silicon, --device mps) uses mlx-audio 0.6.1 (MIT) with the mlx-community 8-bit
weights. The reference is decoded, padded and resampled here and handed over as an array. mlx-audio
caches each reference's codec codes by its text and a checksum of its samples; that cache is
cleared after every request. MLX has no static cache: a reply stops at --max-frames, and one that
generated that many frames without the end-of-speech token is reported as truncated.
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
    p.add_argument('--runtime', default='reference', choices=['reference', 'faster', 'mlx'])
    p.add_argument('--chunk-frames', type=int, default=4, help='faster and mlx runtimes: frames per streamed chunk')
    # Talker positions one request may use: the reference's codec frames + 9, then one per generated
    # frame (12.5 per spoken second). 1152 = (the longest mood reference, 136 positions, plus a reply
    # at --max-input-chars 1000, 778 frames) x 1.25, rounded up to 64 (team-lead, 2026-10-26). The
    # library stops generating at this length without an error, so the input cap must stay in step.
    p.add_argument('--max-seq-len', type=int, default=1152, help='faster runtime: static cache length')
    # Frames one mlx reply may generate: the 778 a 1000-character reply measured, x 1.25, rounded up
    # to 1024 (82 s of speech). A request's max_new_tokens is clamped to it.
    p.add_argument('--max-frames', type=int, default=1024, help='mlx runtime: frames per reply')
    p.add_argument('--fault-after-chunks', type=int, default=0, help=argparse.SUPPRESS)  # tests: fail mid-stream
    p.add_argument('--fake-engine', action='store_true', help=argparse.SUPPRESS)  # tests: mlx wire format, no Metal
    args = p.parse_args()
    if args.runtime == 'faster' and args.device != 'cuda':
        p.error('--runtime faster captures CUDA graphs; it needs --device cuda')
    if args.runtime == 'mlx' and args.device != 'mps':
        p.error('--runtime mlx runs on Apple silicon; it needs --device mps')
    if args.fake_engine and args.runtime != 'mlx':
        p.error('--fake-engine stands in for the mlx runtime only')
    return args


def load(args):
    if args.runtime == 'mlx':
        if args.fake_engine:
            return FakeMlx()
        from mlx_audio.tts.utils import load_model

        return load_model(args.weights)
    import torch
    from qwen_tts import Qwen3TTSModel

    dtype = {'cpu': torch.float32, 'mps': torch.float16, 'cuda': torch.bfloat16}[args.device]
    if args.runtime == 'faster':
        from faster_qwen3_tts import FasterQwen3TTS

        engine = FasterQwen3TTS.from_pretrained(args.weights, device=args.device, dtype=dtype, max_seq_len=args.max_seq_len)
        graph, prefill_kv = engine.talker_graph, engine.talker_graph.prefill_kv
        engine.last_prefill = None

        def record_prefill(past_key_values):
            engine.last_prefill = prefill_kv(past_key_values)
            return engine.last_prefill
        graph.prefill_kv = record_prefill
        return engine
    return Qwen3TTSModel.from_pretrained(args.weights, device_map=args.device, dtype=dtype)


class FakeMlx:
    """Tests only: mlx-audio's generate() contract with a 220 Hz tone instead of speech, 0.78 frames
    per input character (the measured rate), so the wire format is checked without Metal."""
    sample_rate = 24000

    def __init__(self):
        self._icl_cache = {}

    def generate(self, text, ref_audio, ref_text, stream, streaming_interval, max_tokens, **_):
        from types import SimpleNamespace

        self._icl_cache[ref_text] = len(ref_audio)
        total = min(-(-len(text) * 78 // 100), max_tokens)
        step = max(1, int(streaming_interval * 12.5)) if stream else total
        for start in range(0, total, step):
            n = min(step, total - start)
            t = np.arange(start * SAMPLES_PER_FRAME, (start + n) * SAMPLES_PER_FRAME) / self.sample_rate
            yield SimpleNamespace(audio=(0.2 * np.sin(2 * np.pi * 220 * t)).astype(np.float32), token_count=n)


def mlx_generate(model, args, text, ref_audio, ref_text, options, emit=None):
    """One reply through mlx-audio: streamed to `emit` when given, else returned whole. The reference
    gets the same 0.5 s of trailing silence as the faster runtime's prompt."""
    audio, rate = ref_audio
    audio = np.concatenate([audio, np.zeros(int(0.5 * rate), dtype=np.float32)])
    if not args.fake_engine:
        import mlx.core as mx
        from mlx_audio.utils import resample_audio

        if rate != SAMPLE_RATE:
            audio = resample_audio(audio, rate, SAMPLE_RATE)
        audio = mx.array(audio)
    start, first, frames, parts = time.perf_counter(), None, 0, []
    try:
        for result in model.generate(text=text, ref_audio=audio, ref_text=ref_text, stream=emit is not None,
                                     streaming_interval=args.chunk_frames / 12.5, verbose=False, **options):
            pcm = np.asarray(result.audio, dtype=np.float32).reshape(-1)
            frames += int(result.token_count)
            if emit is None:
                parts.append(pcm)
                continue
            emit((np.clip(pcm, -1, 1) * 32767).astype('<i2').tobytes())
            first = first or time.perf_counter() - start
    finally:
        model._icl_cache.clear()
    timing = dict(synthesis_ms=round((time.perf_counter() - start) * 1000, 1), frames=frames,
                  truncated=frames >= options['max_tokens'])
    if emit is not None:
        timing['first_audio_ms'] = round((first or 0) * 1000, 1)
    return timing, (np.concatenate(parts) if parts else np.zeros(0, dtype=np.float32))


def truncated(engine, args, samples):
    """The library stops at max_seq_len without an error: prefill + one position per generated frame
    (12.5 per second, 1920 samples at 24 kHz) reaching max_seq_len - 1 means the reply was cut."""
    prefill = getattr(engine, 'last_prefill', None)
    return prefill is not None and prefill + samples // SAMPLES_PER_FRAME >= args.max_seq_len - 1


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
    text = body.get('input', '')
    if not text.strip() or len(text) > args.max_input_chars:
        raise ValueError(f'input must be 1 to {args.max_input_chars} characters')
    refs = body.get('references') or []
    if not 1 <= len(refs) <= 4:
        raise ValueError('Qwen3-TTS Base has no default voice: send 1 to 4 references (audio and transcript)')
    audio, rate = reference(refs[0])
    language = body.get('language') or 'auto'
    if not isinstance(language, str) or len(language) > 32:
        raise ValueError('language is a language name, such as portuguese, or auto')
    language = language.lower()
    if args.runtime == 'mlx':
        if body.get('seed') is not None and not args.fake_engine:
            import mlx.core as mx

            mx.random.seed(int(body['seed']))
        asked = body.get('max_new_tokens')
        options = dict(max_tokens=args.max_frames if asked is None else max(1, min(int(asked), args.max_frames)))
        options.update({k: float(body[k]) for k in ('temperature', 'top_p') if body.get(k) is not None})
        options['lang_code'] = language
        body['_frame_budget'] = options['max_tokens']
        return text, (audio, rate), refs[0]['text'], options
    import torch

    if body.get('seed') is not None:
        torch.manual_seed(int(body['seed']))
    options = {k: body[k] for k in ('temperature', 'top_p', 'max_new_tokens') if body.get(k) is not None}
    options['language'] = language
    return text, (audio, rate), refs[0]['text'], options


FIRST_FRAMES, CHUNK_FRAMES, LEFT_CONTEXT = 4, 12, 25


def stream(model, args, body, emit):
    """Generate on a thread; decode codes as they arrive and pass PCM bytes to `emit`."""
    text, ref_audio, ref_text, options = prepare(model, args, body)
    if args.runtime == 'mlx':
        return mlx_generate(model, args, text, ref_audio, ref_text, options, emit)[0]
    if args.runtime == 'faster':
        start, first, frames, samples = time.perf_counter(), None, 0, 0
        prompt = faster_prompt(model, ref_audio, ref_text)
        for chunk, _rate, _timing in model.generate_voice_clone_streaming(
                text=text, voice_clone_prompt=prompt, ref_text=ref_text,
                chunk_size=args.chunk_frames, **options):
            if first is None and model.last_prefill is not None:
                # Prefill is done before the first chunk: what the static cache leaves for this reply.
                body['_frame_budget'] = max(0, args.max_seq_len - 1 - model.last_prefill)
            pcm = np.asarray(chunk, dtype=np.float32).reshape(-1)
            samples += pcm.size
            emit((np.clip(pcm, -1, 1) * 32767).astype('<i2').tobytes())
            first = first or time.perf_counter() - start
            frames += 1
        return dict(first_audio_ms=round((first or 0) * 1000, 1),
                    synthesis_ms=round((time.perf_counter() - start) * 1000, 1), frames=frames,
                    truncated=truncated(model, args, samples))
    import torch

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
    if args.runtime == 'mlx':
        timing, audio = mlx_generate(model, args, text, ref_audio, ref_text, options)
        data, seconds = wav_bytes(SAMPLE_RATE, audio)
        elapsed = timing['synthesis_ms'] / 1000
        return data, dict(synthesis_ms=timing['synthesis_ms'], audio_ms=round(seconds * 1000, 1),
                          rtf=round(elapsed / seconds, 3) if seconds else None, truncated=timing['truncated'])
    start = time.perf_counter()
    if args.runtime == 'faster':
        wavs, out_rate = model.generate_voice_clone(
            text=text, voice_clone_prompt=faster_prompt(model, ref_audio, ref_text),
            ref_text=ref_text, **options)
    else:
        wavs, out_rate = model.generate_voice_clone(text=text, ref_audio=ref_audio, ref_text=ref_text, **options)
    elapsed = time.perf_counter() - start
    data, seconds = wav_bytes(out_rate, wavs[0])
    cut = args.runtime == 'faster' and truncated(model, args, len(wavs[0]))
    return data, dict(synthesis_ms=round(elapsed * 1000, 1), audio_ms=round(seconds * 1000, 1),
                      rtf=round(elapsed / seconds, 3) if seconds else None, truncated=cut)


def serve(args, model):
    lock = threading.Lock()
    backend = {'faster': 'faster-qwen3-tts-0.5.4', 'mlx': 'mlx-audio-0.6.1'}.get(args.runtime, 'qwen-tts-0.1.1')
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
            if not isinstance(body, dict):
                return self.error(400, 'invalid_schema', 'body is a JSON object')
            body.pop('_frame_budget', None)  # set below by the engine, never by a client
            fmt = body.get('stream_format')
            if fmt not in (None, 'wav', 'events') or (fmt is not None and not body.get('stream')):
                return self.error(400, 'invalid_schema', "stream_format is 'wav' or 'events', with stream: true")
            if not lock.acquire(blocking=False):
                return self.error(429, 'admission_busy', 'one synthesis at a time')
            if body.get('stream') and fmt == 'events':
                return self.evented(body, lock)
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
            headers = [('Server-Timing', f'synthesis;dur={timing["synthesis_ms"]}, audio;dur={timing["audio_ms"]}, '
                                         f'rtf;desc="{timing["rtf"]}"')]
            if timing['truncated']:
                headers.append(('X-Granite-Truncated', 'true'))
            if body.get('_frame_budget') is not None:
                headers.append(('X-Granite-Frame-Budget', str(body['_frame_budget'])))
            self.reply(200, data, 'audio/wav', headers)

        def streamed(self, body, lock):
            """Chunked transfer: the header now, PCM as it is decoded. An error after the header can
            only end the stream early; it is logged by type, never with the request's content."""
            started, cut = False, False

            def emit(pcm):
                nonlocal started
                if not started:
                    self.send_response(200)
                    self.send_header('Content-Type', 'audio/wav')
                    self.send_header('Transfer-Encoding', 'chunked')
                    self.send_header('Trailer', 'X-Granite-Truncated')
                    if body.get('_frame_budget') is not None:
                        self.send_header('X-Granite-Frame-Budget', str(body['_frame_budget']))
                    self.end_headers()
                    self.chunk(wav_header(SAMPLE_RATE))
                    started = True
                self.chunk(pcm)

            try:
                timing = stream(model, args, body, emit)
                cut = bool(timing.get('truncated'))
                print(f'streamed: first audio {timing["first_audio_ms"]} ms, total {timing["synthesis_ms"]} ms, '
                      f'{timing["frames"]} frames, truncated {cut}', file=sys.stderr, flush=True)
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
                # The headers left before generation ended, so a cut reply is reported in the trailer.
                self.wfile.write(b'0\r\nX-Granite-Truncated: true\r\n\r\n' if cut else b'0\r\n\r\n')
                self.wfile.flush()

        def evented(self, body, lock):
            """stream_format "events": server-sent events, one `audio` event per decoded chunk (base64
            16-bit PCM), then one `done` event, always last, also after a failure mid-stream: whether
            the engine's length limit cut the reply, the error if any, and the frames and seconds sent."""
            started, samples, chunks, failure, timing = False, 0, 0, None, {}

            def event(kind, data):
                nonlocal started
                if not started:
                    self.send_response(200)
                    self.send_header('Content-Type', 'text/event-stream')
                    self.send_header('Cache-Control', 'no-cache')
                    self.send_header('Transfer-Encoding', 'chunked')
                    if body.get('_frame_budget') is not None:
                        self.send_header('X-Granite-Frame-Budget', str(body['_frame_budget']))
                    self.end_headers()
                    started = True
                self.chunk(f'event: {kind}\ndata: {json.dumps(data)}\n\n'.encode())

            def emit(pcm):
                nonlocal samples, chunks
                if args.fault_after_chunks and chunks >= args.fault_after_chunks:
                    raise RuntimeError('fault injected for a test')
                event('audio', dict(pcm_base64=base64.b64encode(pcm).decode(), sample_rate=SAMPLE_RATE, format='s16le'))
                samples, chunks = samples + len(pcm) // 2, chunks + 1

            try:
                timing = stream(model, args, body, emit)
            except ValueError as e:
                if not started:
                    return self.error(400, 'invalid_schema', str(e))
                failure = dict(code='invalid_schema', message=str(e))
            except Exception as e:
                print(f'stream failed: {type(e).__name__}', file=sys.stderr, flush=True)
                if not started:
                    return self.error(500, 'inference_failed', type(e).__name__)
                failure = dict(code='inference_failed', message=type(e).__name__)
            finally:
                lock.release()
            event('done', dict(truncated=failure is None and bool(timing.get('truncated')), error=failure,
                               frames=samples // SAMPLES_PER_FRAME, audio_seconds=round(samples / SAMPLE_RATE, 3)))
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
SAMPLES_PER_FRAME = 1920  # the 12.5 Hz codec at 24 kHz


def main():
    args = parse()
    model = load(args)
    print(f'qwen3-tts worker ready on 127.0.0.1:{args.port} ({args.model}, {args.device})', file=sys.stderr, flush=True)
    serve(args, model)


if __name__ == '__main__':
    main()
