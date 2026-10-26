"""Checks that the Qwen3-TTS worker reports a reply cut short by its static cache (CUDA host only).

The faster runtime stops generating at --max-seq-len without an error. This starts the worker with a
tiny cache, so a long line must come back marked truncated (header when buffered, trailer when
streamed), and a short line must not.

    python3 scripts/check_tts_truncation.py --python venv-faster/bin/python --weights qwen3-tts-0.6b-base \\
        --ref ref/en.wav --ref-text ref/en.txt
"""
import argparse
import base64
import http.client
import json
import os
import signal
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LONG = ("Good morning! Here is a summary of your calendar and your messages. You have a product meeting at "
        "half past nine, then a short call with finance. The afternoon is quieter: one reply is still owed to "
        "the packaging supplier, who asked for the order to be confirmed by Thursday. ") * 3
SHORT = 'Hi! How are you doing?'


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def buffered(port, body):
    c = http.client.HTTPConnection('127.0.0.1', port, timeout=600)
    c.request('POST', '/v1/audio/speech', json.dumps(body), {'Content-Type': 'application/json'})
    r = c.getresponse()
    r.read()
    return r.status, r.getheader('x-granite-truncated')


def streamed(port, body):
    """Reads the chunked reply by hand: http.client drops trailers."""
    data = json.dumps(dict(body, stream=True)).encode()
    with socket.create_connection(('127.0.0.1', port), timeout=600) as s:
        s.sendall(b'POST /v1/audio/speech HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n'
                  + f'Content-Length: {len(data)}\r\nConnection: close\r\n\r\n'.encode() + data)
        raw = b''
        while chunk := s.recv(65536):
            raw += chunk
    head, _, rest = raw.partition(b'\r\n\r\n')
    status = int(head.split(b' ', 2)[1])
    while True:
        size_line, _, rest = rest.partition(b'\r\n')
        size = int(size_line.split(b';')[0], 16)
        if size == 0:
            break
        rest = rest[size + 2:]
    trailers = dict(line.split(b': ', 1) for line in rest.split(b'\r\n') if b': ' in line)
    value = trailers.get(b'X-Granite-Truncated') or trailers.get(b'x-granite-truncated')
    return status, value.decode() if value else None


def main():
    p = argparse.ArgumentParser()
    p.add_argument('--python', required=True)
    p.add_argument('--weights', required=True)
    p.add_argument('--ref', required=True)
    p.add_argument('--ref-text', required=True)
    p.add_argument('--max-seq-len', type=int, default=256)
    args = p.parse_args()
    port = free_port()
    worker = subprocess.Popen([args.python, '-I', str(ROOT / 'workers/tts_qwen.py'), '--weights', args.weights,
                               '--model', 'qwen3-tts-check', '--revision', 'check', '--port', str(port),
                               '--device', 'cuda', '--runtime', 'faster', '--max-seq-len', str(args.max_seq_len)],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    checks = []
    try:
        for _ in range(600):
            if worker.poll() is not None:
                raise SystemExit(f'worker exited {worker.returncode}')
            try:
                socket.create_connection(('127.0.0.1', port), 0.5).close()
                break
            except OSError:
                time.sleep(0.5)
        ref = dict(audio_base64=base64.b64encode(Path(args.ref).read_bytes()).decode(),
                   text=Path(args.ref_text).read_text().strip())
        body = lambda text: dict(model='qwen3-tts-check', input=text, references=[ref], seed=7)
        status, flag = buffered(port, body(LONG))
        checks.append((f'a long line past max_seq_len {args.max_seq_len} is marked truncated (buffered)',
                       status == 200 and flag == 'true'))
        status, flag = streamed(port, body(LONG))
        checks.append(('the same line is marked truncated in the trailer (streamed)', status == 200 and flag == 'true'))
        status, flag = buffered(port, body(SHORT))
        checks.append(('a short line that ends on its own is not marked', status == 200 and flag is None))
    finally:
        os.killpg(worker.pid, signal.SIGTERM)
        worker.wait(timeout=30)
    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
