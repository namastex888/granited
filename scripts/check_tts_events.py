"""Checks the Qwen3-TTS worker's streamed formats, on CUDA (--runtime faster) or Apple silicon (--runtime mlx).

- The default stream (a WAV header, then raw PCM) stays byte-identical: --baseline FILE records its
  sha256 on the first run (run it against the previous worker) and compares on every later run.
- stream_format "events": `audio` events, then one `done` event, always last, whose frames and
  audio seconds match the audio sent, and which carries `truncated` and `error`.
- A reply cut at the engine's length limit (--max-seq-len 256 on CUDA, --max-frames 64 on MLX)
  reports done.truncated; a failure after the first chunks (--fault-after-chunks, a test-only flag)
  still ends with done, carrying the error.

    python3 scripts/check_tts_events.py --python venv-faster/bin/python --weights qwen3-tts-0.6b-base \\
        --ref ref/en.wav --ref-text ref/en.txt --baseline /tmp/tts-default.sha256
    python3 scripts/check_tts_events.py --runtime mlx --python .venv-tts-mlx/bin/python --weights DIR ...

--fake runs the mlx runtime's stand-in engine (a tone, no Metal, no weights): it checks the wire format only.
"""
import argparse
import base64
import hashlib
import io
import json
import math
import os
import signal
import socket
import subprocess
import struct
import sys
import time
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LINE = 'Hi! How are you doing? I will be your voice from now on. Does this sound right?'
LONG = ("Good morning! Here is a summary of your calendar and your messages. You have a product meeting at "
        "half past nine, then a short call with finance. ") * 3
WAV_HEADER = b'RIFF\xff\xff\xff\xffWAVEfmt '


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def start(args, *extra):
    port = free_port()
    device = 'mps' if args.runtime == 'mlx' else 'cuda'
    fake = ['--fake-engine'] if args.fake else []
    worker = subprocess.Popen([args.python, '-I', str(ROOT / 'workers/tts_qwen.py'), '--weights', args.weights,
                               '--model', 'qwen3-tts-check', '--revision', 'check', '--port', str(port),
                               '--device', device, '--runtime', args.runtime, *fake, *extra],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    if args.pids:
        # For an outside watchdog: each worker's PID as it spawns, so it can be stopped by PID alone.
        with open(args.pids, 'a') as f:
            f.write(f'{worker.pid}\n')
    for _ in range(600):
        if worker.poll() is not None:
            return worker, None
        try:
            socket.create_connection(('127.0.0.1', port), 0.5).close()
            return worker, port
        except OSError:
            time.sleep(0.5)
    return worker, None


def stop(worker):
    if worker.poll() is None:
        os.killpg(worker.pid, signal.SIGTERM)
        worker.wait(timeout=30)


def post(port, body):
    """The status, the headers and the de-chunked body, read by hand so nothing is reassembled."""
    data = json.dumps(body).encode()
    with socket.create_connection(('127.0.0.1', port), timeout=600) as s:
        s.sendall(b'POST /v1/audio/speech HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n'
                  + f'Content-Length: {len(data)}\r\nConnection: close\r\n\r\n'.encode() + data)
        raw = b''
        while chunk := s.recv(65536):
            raw += chunk
    head, _, rest = raw.partition(b'\r\n\r\n')
    lines = head.decode().split('\r\n')
    headers = {k.lower(): v for k, v in (line.split(': ', 1) for line in lines[1:])}
    if headers.get('transfer-encoding') != 'chunked':
        return int(lines[0].split()[1]), headers, rest
    body_bytes = b''
    while True:
        size_line, _, rest = rest.partition(b'\r\n')
        size = int(size_line.split(b';')[0], 16)
        if size == 0:
            break
        body_bytes += rest[:size]
        rest = rest[size + 2:]
    return int(lines[0].split()[1]), headers, body_bytes


def events(body_bytes):
    out = []
    try:
        text = body_bytes.decode()
    except UnicodeDecodeError:  # audio, not events
        return out
    for block in text.split('\n\n'):
        kind = next((l[6:].strip() for l in block.splitlines() if l.startswith('event:')), None)
        data = next((l[5:].strip() for l in block.splitlines() if l.startswith('data:')), None)
        if kind:
            out.append((kind, json.loads(data) if data else None))
    return out


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--python', required=True)
    p.add_argument('--runtime', default='faster', choices=['faster', 'mlx'])
    p.add_argument('--fake', action='store_true', help='mlx only: the stand-in engine, no weights or Metal')
    p.add_argument('--weights', default='-')
    p.add_argument('--ref')
    p.add_argument('--ref-text')
    p.add_argument('--baseline', required=True, help='file holding the default stream sha256')
    p.add_argument('--pids', help='append each spawned worker PID to this file')
    args = p.parse_args()
    if args.fake and args.runtime != 'mlx':
        p.error('--fake needs --runtime mlx')
    if not args.fake and not (args.ref and args.ref_text and args.weights != '-'):
        p.error('--weights, --ref and --ref-text are required unless --fake')
    if args.fake:
        tone = io.BytesIO()
        with wave.open(tone, 'wb') as w:
            w.setnchannels(1), w.setsampwidth(2), w.setframerate(24000)
            w.writeframes(b''.join(struct.pack('<h', int(8000 * math.sin(i / 20))) for i in range(48000)))
        ref = dict(audio_base64=base64.b64encode(tone.getvalue()).decode(), text='a two second tone')
    else:
        ref = dict(audio_base64=base64.b64encode(Path(args.ref).read_bytes()).decode(), text=Path(args.ref_text).read_text().strip())
    limit = ['--max-frames', '64'] if args.runtime == 'mlx' else ['--max-seq-len', '256']
    body = lambda text, **kw: dict(model='qwen3-tts-check', input=text, references=[ref], seed=7, stream=True, **kw)
    checks = []

    worker, port = start(args)
    try:
        status, headers, raw = post(port, body(LINE))
        digest = hashlib.sha256(raw).hexdigest()
        baseline = Path(args.baseline)
        if not baseline.exists():
            baseline.write_text(digest + '\n')
            print(f'   recorded the default stream baseline {digest[:16]} in {baseline}')
        checks.append(('the default stream is a WAV header then PCM, byte-identical to the baseline',
                       status == 200 and headers.get('content-type') == 'audio/wav' and raw.startswith(WAV_HEADER)
                       and b'event:' not in raw[:64] and baseline.read_text().strip() == digest))
        status, headers, raw = post(port, body(LINE, stream_format='events'))
        got = events(raw) if headers.get('content-type', '').startswith('text/event-stream') else []
        audio = [d for k, d in got if k == 'audio']
        samples = sum(len(base64.b64decode(d['pcm_base64'])) // 2 for d in audio)
        done = got[-1][1] if got and got[-1][0] == 'done' else {}
        print('   events:', len(audio), 'audio, last', got[-1][0] if got else None, {k: done.get(k) for k in ('frames', 'audio_seconds', 'truncated', 'error')})
        checks.append(('stream_format events: audio events, then one done event, last', status == 200 and audio
                       and [k for k, _ in got].count('done') == 1 and got[-1][0] == 'done'))
        # Barge-in: the client leaves after the first audio event; the worker must stop and free its slot.
        data = json.dumps(body(LONG, stream_format='events')).encode()
        with socket.create_connection(('127.0.0.1', port), timeout=600) as sock:
            sock.sendall(b'POST /v1/audio/speech HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n'
                         + f'Content-Length: {len(data)}\r\nConnection: close\r\n\r\n'.encode() + data)
            seen = b''
            while b'event: audio' not in seen and (chunk := sock.recv(65536)):
                seen += chunk
        left, served = time.perf_counter(), None
        while time.perf_counter() - left < 30:
            status = post(port, body(LINE))[0]
            if status != 429:
                served = (status, round(time.perf_counter() - left, 2))
                break
            time.sleep(0.2)
        print('   barge-in: next request', served)
        checks.append(('a client that leaves after the first audio event frees the worker: the next request is served',
                       b'event: audio' in seen and served is not None and served[0] == 200))
        checks.append(('done carries the frames and seconds sent, not truncated, no error',
                       done.get('frames') == samples // 1920 and abs(done.get('audio_seconds', -1) - samples / 24000) < 0.01
                       and done.get('truncated') is False and done.get('error') is None))
    finally:
        stop(worker)

    worker, port = start(args, *limit)
    try:
        got = events(post(port, body(LONG, stream_format='events'))[2]) if port else []
        checks.append((f'a reply cut at {" ".join(limit)} reports done.truncated', got and got[-1][0] == 'done' and got[-1][1].get('truncated') is True))
    finally:
        stop(worker)

    worker, port = start(args, '--fault-after-chunks', '2')
    try:
        got = events(post(port, body(LONG, stream_format='events'))[2]) if port else []
        done = got[-1][1] if got and got[-1][0] == 'done' else {}
        print('   fault: worker', 'up' if port else 'refused the flag', '| last event', got[-1][0] if got else None, done.get('error'))
        checks.append(('a failure mid-stream still ends with done, carrying the error and the audio sent',
                       bool(done) and done.get('error') is not None and done.get('audio_seconds', 0) > 0
                       and sum(1 for k, _ in got if k == 'audio') == 2))
    finally:
        stop(worker)

    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
