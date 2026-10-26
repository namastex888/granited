#!/usr/bin/env python3
"""End-to-end check of `model: "auto"` for chat, with two stub chat models and no real weights.

Each scenario starts the router on a fresh profile: the stubs stand in for Granite 4.2 8B and 3B,
sharing one exclusive device (a GPU-like rule: one resident model) or the CPU (no such rule).
The checks cover the auto v0 spec: warm before cold, no load that evicts a ready model, visible
fallbacks, the temperature clamp, sticky conversations, the closed aggregate refusal, and effort.
"""
import base64
import concurrent.futures
import http.client
import json
import os
import signal
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
API, ADMIN = 'smoke-api-key-0123456789', 'smoke-admin-key-0123456789'
REGISTRY = {m['id']: m for m in json.loads((ROOT / 'configs/registry.json').read_text())['models']}
B8, B3, VISION = 'granite-4.2-8b', 'granite-4.2-3b', 'granite-vision-4.1-4b'
SPEECH = 'granite-speech-5.0-470m-turboctc'
SWITCH, EMBED, TTM = 'granite-switch-4.1-8b', 'granite-embedding-97m-multilingual-r2', 'granite-timeseries-ttm-r3'
STUB_FAMILY = {'speech': 'speech', 'speech_synthesis': 'speech_synthesis', 'embedding': 'embedding', 'time_series': 'timeseries'}
PNG = 'data:image/png;base64,' + base64.b64encode(b'\x89PNG\r\n\x1a\n' + bytes(64)).decode()
def wav(seconds=0.5, rate=16000):
    import io, wave
    buffer = io.BytesIO()
    with wave.open(buffer, 'wb') as w:
        w.setnchannels(1); w.setsampwidth(2); w.setframerate(rate); w.writeframes(bytes(2 * int(seconds * rate)))
    return buffer.getvalue()


WAV_BYTES = wav()
WAV = base64.b64encode(WAV_BYTES).decode()


def image(url=PNG):
    return {'type': 'image_url', 'image_url': {'url': url}}


def audio(fmt='wav'):
    return {'type': 'input_audio', 'input_audio': {'data': WAV, 'format': fmt}}


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


class Router:
    def __init__(self, device, hold=False, extra=(), models=None, peers=(), host='auto-smoke', auto=None, backend=None):
        self.state = tempfile.TemporaryDirectory()
        self.port = free_port()
        deployments = []
        for model in (models or (B8, B3)) + tuple(extra):
            port = free_port()
            family = STUB_FAMILY.get(REGISTRY[model]['family'], 'language')
            deployments.append(dict(
                model=model, backend=backend or family + '_native', base_url=f'http://127.0.0.1:{port}',
                revision=REGISTRY[model]['hf_revision'], artifact_repo='stub',
                command=['python3', 'workers/stub.py', '--family', family, '--model', model,
                         '--revision', REGISTRY[model]['hf_revision'], '--port', str(port)] + (['--backend', backend] if backend else []),
                memory_bytes=64 << 20, context_tokens=4096, device=device, health_path='/healthz', identity_path='/internal/metadata',
                runtime_manifest='workers/stub.py', backend_build='vllm-vstub' if backend == 'vllm' else 'stub', max_concurrency=1, load_timeout_seconds=60,
                **({'hold_reason': 'held for the test'} if hold else {})))
        config = dict(listen=f'127.0.0.1:{self.port}', host_id=host, registry='configs/registry.json',
                      state_dir=self.state.name, api_key_env='GRANITE_API_KEY', admin_key_env='GRANITE_ADMIN_KEY',
                      ram_reserve_bytes=0, accelerator_reserve_bytes=0, deployments=deployments, peers=list(peers),
                      auto=auto or dict(enabled=True, prefer=dict(chat=[B8, B3])))
        path = Path(self.state.name) / 'config.json'
        path.write_text(json.dumps(config))
        env = dict(os.environ, GRANITE_API_KEY=API, GRANITE_ADMIN_KEY=ADMIN, PEER_TOKEN=ADMIN)
        self.process = subprocess.Popen([str(ROOT / 'target/debug/granited'), 'serve', str(path)], cwd=ROOT, env=env,
                                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(80):
            try:
                urllib.request.urlopen(f'http://127.0.0.1:{self.port}/healthz', timeout=1).close()
                return
            except OSError:
                time.sleep(0.25)
        raise SystemExit('router did not start')

    def chat(self, model='auto', text='hi', headers=None, content=None, **body):
        data = json.dumps(dict(model=model, messages=[dict(role='user', content=content or text)], **body)).encode()
        req = urllib.request.Request(f'http://127.0.0.1:{self.port}/v1/chat/completions', data,
                                     {'Authorization': 'Bearer ' + API, 'Content-Type': 'application/json', **(headers or {})})
        try:
            with urllib.request.urlopen(req, timeout=180) as r:
                status, head, raw = r.status, {k.lower(): v for k, v in r.headers.items()}, r.read()
        except urllib.error.HTTPError as e:
            status, head, raw = e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()
        body = json.loads(raw)
        route = json.loads(head['x-granite-route']) if 'x-granite-route' in head else {}
        seen = json.loads(body['choices'][0]['message']['content']) if status == 200 else {}
        return status, head, body, route, seen

    def transcribe(self, model):
        boundary = 'smokeboundary'
        body = (f'--{boundary}\r\nContent-Disposition: form-data; name="model"\r\n\r\n{model}\r\n'
                f'--{boundary}\r\nContent-Disposition: form-data; name="file"; filename="a.wav"\r\n'
                'Content-Type: audio/wav\r\n\r\n').encode() + WAV_BYTES + f'\r\n--{boundary}--\r\n'.encode()
        req = urllib.request.Request(f'http://127.0.0.1:{self.port}/v1/audio/transcriptions', body,
                                     {'Authorization': 'Bearer ' + API, 'Content-Type': f'multipart/form-data; boundary={boundary}'})
        try:
            with urllib.request.urlopen(req, timeout=180) as r:
                return r.status, {k.lower(): v for k, v in r.headers.items()}
        except urllib.error.HTTPError as e:
            return e.code, {k.lower(): v for k, v in e.headers.items()}

    def models(self):
        req = urllib.request.Request(f'http://127.0.0.1:{self.port}/v1/models', headers={'Authorization': 'Bearer ' + API})
        return json.loads(urllib.request.urlopen(req, timeout=60).read())['data']

    def admin(self, path):
        req = urllib.request.Request(f'http://127.0.0.1:{self.port}{path}', b'{}',
                                     {'Authorization': 'Bearer ' + ADMIN, 'Content-Type': 'application/json'})
        return json.loads(urllib.request.urlopen(req, timeout=60).read())

    def states(self):
        req = urllib.request.Request(f'http://127.0.0.1:{self.port}/internal/hosts', headers={'Authorization': 'Bearer ' + ADMIN})
        hosts = json.loads(urllib.request.urlopen(req, timeout=60).read())['hosts']
        return {w['model']: w['state'] for w in hosts[0]['hardware']['managed_workers']}

    def close(self):
        self.process.terminate()
        self.process.wait(timeout=20)
        self.state.cleanup()


def main():
    checks = []
    check = lambda name, ok: checks.append((name, bool(ok)))

    # One exclusive device: only one of the two can be resident.
    r = Router('unified')
    try:
        status, head, _, route, seen = r.chat()
        check('both cold: 8B loads and serves as the first preference',
              status == 200 and head.get('x-granite-model-alias') == B8 and route.get('source') == 'rule'
              and route.get('why') == 'cold_free' and seen.get('chat_template_kwargs', {}).get('enable_thinking') is False)
        r.chat(model=B3, reasoning_effort='off', temperature=0)  # an explicit 3B evicts 8B, as today
        status, head, _, route, seen = r.chat()
        check('3B warm, 8B cold on the same device: 3B serves and 8B is not loaded',
              status == 200 and head.get('x-granite-model-alias') == B3 and r.states().get(B8) != 'ready')
        check('the fallback is visible with the preferred model',
              route.get('source') == 'fallback' and route.get('preferred') == B8
              and route.get('fallback_reason') == 'preferred_unavailable' and route.get('why') == 'warm')
        check('3B with effort off and no temperature is clamped to 0',
              seen.get('temperature') == 0 and route.get('clamps') == ['temperature_zero'])
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            slow = pool.submit(r.chat, model=B3, text='SLOW', reasoning_effort='off', temperature=0)
            time.sleep(0.5)
            status, head, body, _, _ = r.chat()
            slow.result()
        check('3B full and 8B would evict it: 429 auto_unavailable/busy, Retry-After 2, nothing loaded',
              status == 429 and body['error'].get('code') == 'auto_unavailable' and body['error'].get('reason') == 'busy'
              and head.get('retry-after') == '2' and r.states().get(B8) != 'ready')
        status, head, _, route, _ = r.chat(headers={'x-granite-conversation': 'c1'})
        r.chat(model=B8, reasoning_effort='off')  # 8B evicts 3B
        status, head, _, route, _ = r.chat(headers={'x-granite-conversation': 'c1'})
        check('a sticky 3B conversation moves to warm 8B when 3B would need a load over it',
              status == 200 and head.get('x-granite-model-alias') == B8 and route.get('why') == 'sticky_moved'
              and r.states().get(B3) != 'ready')
        status, _, body, _, _ = r.chat(headers={'x-granite-conversation': 'not valid!'})
        check('a malformed conversation id is a 400', status == 400 and body['error']['code'] == 'invalid_schema')
        status, _, body, _, _ = r.chat(model=B3, reasoning_effort='off')
        check('an explicit 3B with effort off and no temperature is still a 400, never clamped',
              status == 400 and body['error']['code'] == 'invalid_schema')
        status, _, body, _, _ = r.chat(headers={'x-granite-hop': '1'})
        check('a peer hop never resolves auto', status == 400 and body['error']['code'] == 'auto_not_forwardable')
        status, _, body, _, _ = r.chat(content=[{'type': 'text', 'text': 'what is this'}, image()])
        check('image input under auto is refused while the profile leaves it off',
              status == 400 and body['error']['code'] == 'unsupported_auto')
    finally:
        r.close()

    # CPU: both can be resident.
    r = Router('cpu')
    try:
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            slow = pool.submit(r.chat, model=B8, text='SLOW', reasoning_effort='off')
            time.sleep(0.8)
            status, head, _, route, _ = r.chat(headers={'x-granite-conversation': 'c2'})
            slow.result()
        check('8B full: 3B loads beside it (no eviction on CPU) and serves',
              status == 200 and head.get('x-granite-model-alias') == B3 and route.get('why') == 'cold_free')
        status, head, _, route, _ = r.chat(headers={'x-granite-conversation': 'c2'})
        check('a conversation stays on 3B while 8B is free: sticky, no fallback_reason',
              status == 200 and head.get('x-granite-model-alias') == B3 and route.get('source') == 'sticky'
              and route.get('preferred') == B8 and 'fallback_reason' not in route)
        status, head, _, route, seen = r.chat(thinking_budget_tokens=300, max_tokens=100)
        check('a thinking budget alone means full effort with that budget',
              status == 200 and route.get('effort') == 'full' and seen.get('thinking_budget_tokens') == 300
              and seen.get('max_tokens') == 400)
        status, _, _, route, seen = r.chat(reasoning_effort='low')
        check('an explicit effort wins', status == 200 and route.get('effort') == 'low'
              and seen.get('chat_template_kwargs', {}).get('low_effort') is True)
    finally:
        r.close()

    # A third model, not in auto's list, sits idle on the same device: auto must not evict it.
    r = Router('unified', extra=(VISION,))
    try:
        r.chat(model=VISION)
        status, head, body, _, _ = r.chat()
        check('an idle model outside the list is never evicted: 503 auto_unavailable/no_room, Retry-After 30',
              status == 503 and body['error'].get('reason') == 'no_room' and head.get('retry-after') == '30'
              and r.states().get(VISION) == 'ready' and r.states().get(B8) != 'ready')
        status, head, _, _, _ = r.chat(model=B8, reasoning_effort='off')
        check('an explicit model id still loads by evicting, as before', status == 200 and r.states().get(VISION) != 'ready')
    finally:
        r.close()

    # Peers: B serves 8B, A has 3B locally. A peer counts only while warm, and is told so.
    b = Router('cpu', models=(B8,), host='peer-b')
    a = Router('cpu', models=(B3,), host='node-a',
               peers=[dict(id='peer-b', url=f'http://127.0.0.1:{b.port}', token_env='PEER_TOKEN', models=[B8])])
    try:
        status, head, _, _, _ = b.chat(headers={'x-granite-load': 'warm-only'}, model=B8, reasoning_effort='off')
        check('a peer asked for warm-only never loads a cold model',
              status == 503 and head.get('x-granite-error-code') == 'model_not_warm' and b.states().get(B8) != 'ready')
        status, head, _, route, _ = a.chat()
        check('a cold peer is not tried: the local 3B loads and serves',
              status == 200 and head.get('x-granite-model-alias') == B3 and route.get('source') == 'fallback'
              and b.states().get(B8) != 'ready')
        b.chat(model=B8, reasoning_effort='off')  # warm 8B on B
        time.sleep(2.2)  # let A's snapshot of B expire
        status, head, _, route, _ = a.chat()
        check('a warm peer 8B serves as the first preference',
              status == 200 and head.get('x-granite-model-alias') == B8 and head.get('x-granite-host') == 'peer-b'
              and route.get('source') == 'rule')
        b.admin(f'/admin/models/{B8}/unload')
        for _ in range(40):
            if b.states().get(B8) != 'ready':
                break
            time.sleep(0.1)
        status, head, _, route, _ = a.chat()  # A's snapshot still says warm for up to 2 s
        # Within A's 2 s snapshot of B, B refuses model_not_warm (refused_next); a slow machine may
        # take longer, and then A already sees B cold (cold_free). Either way B must not load.
        check('a peer that went cold is never loaded; the next candidate serves',
              status == 200 and head.get('x-granite-model-alias') == B3
              and route.get('why') in ('refused_next', 'cold_free') and b.states().get(B8) != 'ready')
    finally:
        a.close()
        b.close()

    # Image and audio input: a vision list and a speech stage, on the CPU (no eviction rule).
    omni = dict(enabled=True, images=True, audio=True,
                prefer=dict(chat=[B8, B3], vision=[VISION], transcription=[SPEECH]))
    r = Router('cpu', models=(B8, B3, VISION, SPEECH), auto=omni)
    try:
        auto_model = next(m for m in r.models() if m['id'] == 'auto')
        check('/v1/models says auto takes text, image and audio', auto_model.get('input_modalities') == ['text', 'image', 'audio'])
        b8 = next(m for m in r.models() if m['id'] == B8)
        status, head, _, _, _ = r.chat(model=B8, reasoning_effort='off')
        check('per-request context: the configured value before an engine reports one, in /v1/models and on the reply',
              b8.get('context_tokens') == 4096 and b8.get('context_source') == 'config'
              and status == 200 and head.get('x-granite-context-tokens') == '4096')
        status, head, _, route, seen = r.chat(content=[{'type': 'text', 'text': 'what is this'}, image()],
                                              reasoning_effort='full', thinking_budget_tokens=100)
        check('an image goes to the vision model with its parts intact and no effort fields',
              status == 200 and head.get('x-granite-model-alias') == VISION and route.get('modality') == 'vision'
              and 'effort' not in route and seen.get('parts') == ['text', 'image_url']
              and seen.get('reasoning_effort') is None and seen.get('thinking_budget_tokens') is None)
        status, _, body, _, _ = r.chat(content=[image('https://example.com/cat.png')])
        status2, _, body2, _, _ = r.chat(model=VISION, content=[image('https://example.com/cat.png')])
        check('a remote image URL is refused, under auto and for an explicit model',
              status == 400 and status2 == 400 and body['error']['code'] == body2['error']['code'] == 'invalid_schema')
        remote = 'http://169.254.169.254/latest/meta-data'
        for shape in ({'type': 'video_url', 'video_url': {'url': remote}}, {'type': 'audio_url', 'audio_url': {'url': remote}},
                      {'type': 'input_image', 'image_url': remote}, {'type': 'file', 'file': {'url': remote}},
                      {'image_url': {'url': remote}}):
            codes = {r.chat(model=m, content=[shape])[0] for m in ('auto', VISION)}
            check(f'a {shape.get("type", "untyped")} part naming a URL is refused, under auto and explicit', codes == {400})
        jpg = 'data:image/jpg;base64,' + base64.b64encode(bytes(16)).decode()
        params = 'data:image/png;name=a.png;base64,' + base64.b64encode(bytes(16)).decode()
        check('image/jpg and MIME parameters are accepted',
              r.chat(content=[image(jpg)])[0] == 200 and r.chat(content=[image(params)])[0] == 200)
        status, _, _, _, _ = r.chat(content=[image('data:image/gif;base64,R0lGOD')])
        big = 'data:image/png;base64,' + base64.b64encode(bytes(9 << 20)).decode()
        status2, _, _, _, _ = r.chat(content=[image(big)])
        check('a gif or an image over 8 MiB is refused', status == 400 and status2 == 400)
        status, head, _, route, seen = r.chat(content=[audio()])
        check('audio is transcribed first, and the text goes to the chat list',
              status == 200 and head.get('x-granite-model-alias') == B8
              and route.get('stages') == [{'modality': 'transcription', 'model': SPEECH}]
              and seen.get('parts') == ['text'] and seen.get('texts') == [f'stub heard {len(WAV_BYTES)} bytes'])
        status, head, _, route, seen = r.chat(content=[audio(), image()])
        check('audio and an image: transcribed, then the vision model',
              status == 200 and head.get('x-granite-model-alias') == VISION and route.get('stages')
              and seen.get('parts') == ['text', 'image_url'])
        fake = {'type': 'input_audio', 'input_audio': {'data': base64.b64encode(bytes(400)).decode(), 'format': 'wav'}}
        status, _, body, _, _ = r.chat(content=[fake])
        check('bytes that are not a wav file are refused before the speech worker', status == 400 and 'not a wav' in body['error']['message'])
        status, _, body, _, _ = r.chat(content=[audio()] * 5)
        check('more than 4 audio parts in one request are refused', status == 400 and 'at most 4' in body['error']['message'])
        status, _, body, _, _ = r.chat(content=[audio('mp3')])
        check('audio other than wav is refused', status == 400 and body['error']['code'] == 'invalid_schema')
    finally:
        r.close()

    # Speech to text stays on the entry node, even when a peer has the speech model warm.
    b = Router('cpu', models=(SPEECH,), host='peer-b')
    a = Router('cpu', models=(B8, SPEECH), host='node-a', auto=dict(
        enabled=True, audio=True, prefer=dict(chat=[B8], transcription=[SPEECH])),
        peers=[dict(id='peer-b', url=f'http://127.0.0.1:{b.port}', token_env='PEER_TOKEN', models=[SPEECH])])
    try:
        b.admin(f'/admin/models/{SPEECH}/load')
        for _ in range(100):
            if b.states().get(SPEECH) == 'ready':
                break
            time.sleep(0.1)
        status, head, _, route, _ = a.chat(content=[audio()])
        check('audio is transcribed on the entry node, never by a warm peer',
              b.states().get(SPEECH) == 'ready' and status == 200
              and route.get('stages') == [{'modality': 'transcription', 'model': SPEECH}]
              and a.states().get(SPEECH) == 'ready')
        a.admin(f'/admin/models/{SPEECH}/unload')
        for _ in range(100):
            if a.states().get(SPEECH) != 'ready':
                break
            time.sleep(0.1)
        status, head = a.transcribe(SPEECH)
        check('an explicit transcription forwarded to the warm peer is served (the peer takes the JSON form)',
              status == 200 and head.get('x-granite-host') == 'peer-b')
        bad = {'type': 'image_url', 'image_url': {'url': 'http://169.254.169.254/'}}
        codes = {a.chat(model=m, content=bad)[0] for m in ('auto', B8)}
        check('object-valued message content is refused, under auto and explicit', codes == {400})
    finally:
        a.close()
        b.close()

    # The vision list never evicts either: 8B is resident on the one exclusive device.
    r = Router('unified', models=(B8, B3, VISION),
               auto=dict(enabled=True, images=True, prefer=dict(chat=[B8, B3], vision=[VISION])))
    try:
        r.chat()
        status, head, body, _, _ = r.chat(content=[image()])
        check('an image never evicts the resident chat model: 503 no_room for the vision list',
              status == 503 and body['error'].get('reason') == 'no_room' and 'vision' in body['error']['message']
              and r.states().get(B8) == 'ready' and r.states().get(VISION) != 'ready')
    finally:
        r.close()

    # A peer that dies is skipped: the request falls over to the next host at once.
    b = Router('cpu', models=(B3,), host='peer-b')
    a = Router('cpu', models=(B3,), host='node-a',
               peers=[dict(id='peer-b', url=f'http://127.0.0.1:{b.port}', token_env='PEER_TOKEN', models=[B3])])
    try:
        b.chat(model=B3, reasoning_effort='off', temperature=0)
        status, head, _, _, _ = a.chat(model=B3, reasoning_effort='off', temperature=0)
        served_by_peer = status == 200 and head.get('x-granite-host') == 'peer-b'
        b.process.kill()
        b.process.wait()
        status, head, _, _, _ = a.chat(model=B3, reasoning_effort='off', temperature=0)
        check('a dead peer is skipped: the next host serves instead of a 502',
              served_by_peer and status == 200 and head.get('x-granite-host') == 'node-a')
        # A killed manager leaves its worker running; its ledger names exactly that process.
        ledger = list((Path(b.state.name) / 'owned').glob('*.pid'))
        check('a killed manager leaves a ledger entry naming its orphaned worker', len(ledger) == 1)
    finally:
        a.close()
        for entry in (Path(b.state.name) / 'owned').glob('*.pid'):
            try:
                os.killpg(int(entry.read_text().split('\t')[0]), signal.SIGTERM)
            except (ProcessLookupError, ValueError):
                pass
        b.state.cleanup()

    # /v1/devices: per-device model states in placement's words and the best tier that fits.
    T17, T06 = 'qwen3-tts-1.7b-base', 'qwen3-tts-0.6b-base'
    r = Router('unified', models=(T17, T06), peers=[dict(id='gone', url='http://127.0.0.1:9', token_env='PEER_TOKEN', models=[T17])])
    try:
        def devices(key=API):
            req = urllib.request.Request(f'http://127.0.0.1:{r.port}/v1/devices', headers={'Authorization': 'Bearer ' + key})
            try:
                with urllib.request.urlopen(req, timeout=60) as resp:
                    return resp.status, resp.read().decode()
            except urllib.error.HTTPError as e:
                return e.code, ''
        status, raw = devices()
        local, gone = json.loads(raw)['devices']
        tts = {e['model']: e['state'] for e in local['capabilities']['speech_synthesis']}
        check('/v1/devices: both voice tiers fit while nothing is loaded, and the better tier is recommended',
              status == 200 and tts == {T17: 'cold_fits', T06: 'cold_fits'}
              and local['recommended']['speech_synthesis']['model'] == T17)
        check('/v1/devices: an unreachable peer is listed as unreachable, and no address or hostname leaks',
              gone == {'device': 'gone', 'local': False, 'reachable': False} and '127.0.0.1' not in raw and 'hostname' not in raw)
        r.admin(f'/admin/models/{T06}/load')
        deadline = time.time() + 180
        while r.states().get(T06) != 'ready' and time.time() < deadline:
            time.sleep(0.2)
        local = json.loads(devices()[1])['devices'][0]
        tts = {e['model']: e['state'] for e in local['capabilities']['speech_synthesis']}
        print('   devices with 0.6B loaded:', tts, local['recommended'].get('speech_synthesis'))
        check('/v1/devices: with 0.6B resident on the one device, 1.7B is no_room and 0.6B is recommended',
              tts == {T17: 'no_room', T06: 'warm'} and local['recommended']['speech_synthesis']['model'] == T06)
        check('/v1/devices needs the API key', devices('wrong-key')[0] == 401)
    finally:
        r.close()

    # auto on /v1/audio/speech: best voice tier first, never evicting, this node only.
    def speak(r, headers=None):
        data = json.dumps(dict(model='auto', input='hello there')).encode()
        req = urllib.request.Request(f'http://127.0.0.1:{r.port}/v1/audio/speech', data,
                                     {'Authorization': 'Bearer ' + API, 'Content-Type': 'application/json', **(headers or {})})
        try:
            with urllib.request.urlopen(req, timeout=180) as resp:
                status, head, raw = resp.status, {k.lower(): v for k, v in resp.headers.items()}, resp.read()
        except urllib.error.HTTPError as e:
            status, head, raw = e.code, {k.lower(): v for k, v in e.headers.items()}, e.read()
        return status, head, json.loads(raw), json.loads(head.get('x-granite-route', '{}'))
    voice = dict(enabled=True, speech=True, prefer=dict(chat=[B3], speech_synthesis=[T17, T06]))
    r = Router('unified', models=(B3, T17, T06), auto=voice)
    try:
        status, head, body, route = speak(r)
        check('auto speech: with nothing loaded, the best tier (1.7B) loads and serves',
              status == 200 and body.get('model') == T17 and route.get('modality') == 'speech_synthesis' and route.get('model') == T17)
        status, head, body, route = speak(r, {'x-granite-hop': '1'})
        check('auto speech is resolved by the first node only', status == 400 and head.get('x-granite-error-code') == 'auto_not_forwardable')
        def cut(text):
            data = json.dumps(dict(model=T17, input=text, response_format='wav')).encode()
            req = urllib.request.Request(f'http://127.0.0.1:{r.port}/v1/audio/speech', data,
                                         {'Authorization': 'Bearer ' + API, 'Content-Type': 'application/json'})
            with urllib.request.urlopen(req, timeout=180) as resp:
                return resp.status, resp.headers.get('x-granite-truncated')
        check('a reply the speech worker cut at its cache length reaches the client marked x-granite-truncated',
              cut('TRUNCATE this') == (200, 'true') and cut('hello') == (200, None))
    finally:
        r.close()
    r = Router('unified', models=(B3, T17, T06), auto=voice)
    try:
        r.admin(f'/admin/models/{T06}/load')
        deadline = time.time() + 180
        while r.states().get(T06) != 'ready' and time.time() < deadline:
            time.sleep(0.2)
        status, head, body, route = speak(r)
        check('auto speech: with 0.6B resident, 0.6B serves warm and 1.7B is never loaded over it',
              status == 200 and body.get('model') == T06 and route.get('why') == 'warm' and r.states().get(T17) != 'ready')
    finally:
        r.close()
    b = Router('unified', models=(T17,), host='voice-peer')
    try:
        b.admin(f'/admin/models/{T17}/load')
        deadline = time.time() + 180
        while b.states().get(T17) != 'ready' and time.time() < deadline:
            time.sleep(0.2)
        a = Router('unified', hold=True, models=(B3, T17, T06), auto=voice,
                   peers=[dict(id='voice-peer', url=f'http://127.0.0.1:{b.port}', token_env='PEER_TOKEN', models=[T17])])
        try:
            status, head, body, route = speak(a)
            check('auto speech never sends voice references to a peer, even a warm one, when local tiers are held',
                  status == 503 and body['error'].get('reason') == 'held' and b.states().get(T17) == 'ready'
                  and head.get('x-granite-host') != 'voice-peer')
        finally:
            a.close()
    finally:
        b.close()
    r = Router('unified', models=(B3, T17, T06), auto=dict(enabled=True, prefer=dict(chat=[B3], speech_synthesis=[T17, T06])))
    try:
        status, head, body, route = speak(r)
        check('auto speech stays off unless the profile sets auto.speech', status == 400 and head.get('x-granite-error-code') == 'unsupported_auto')
    finally:
        r.close()

    # Usage counters: counted only by the node that ran the backend, from the backend's own usage.
    def get(r, path, key=ADMIN):
        req = urllib.request.Request(f'http://127.0.0.1:{r.port}{path}', headers={'Authorization': 'Bearer ' + key})
        try:
            with urllib.request.urlopen(req, timeout=60) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as e:
            return e.code, None

    def post_stream(url, model, text='hi', key=API, **body):
        data = json.dumps(dict(model=model, messages=[dict(role='user', content=text)], stream=True,
                               reasoning_effort='off', temperature=0, **body)).encode()
        req = urllib.request.Request(f'{url}/v1/chat/completions', data,
                                     {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=180) as response:
            return {k.lower(): v for k, v in response.headers.items()}, response.read()

    def stream(r, model, **body):
        raw = post_stream(f'http://127.0.0.1:{r.port}', model, **body)[1]
        events = [e[len('data: '):] for e in raw.decode().split('\n\n') if e.startswith('data: ')]
        return [e if e == '[DONE]' else json.loads(e) for e in events]

    usage_only = lambda events: [e for e in events if isinstance(e, dict) and e.get('usage') and not e.get('choices')]
    b = Router('cpu', models=(B8,), host='peer-b')
    a = Router('cpu', models=(B3, SPEECH), host='node-a',
               peers=[dict(id='peer-b', url=f'http://127.0.0.1:{b.port}', token_env='PEER_TOKEN', models=[B8])])
    try:
        head, raw = post_stream(f'http://127.0.0.1:{a.port}', B3)
        check('a stripped stream carries no upstream Content-Length', 'content-length' not in head
              and head.get('transfer-encoding') == 'chunked' and b'"usage"' not in raw)
        events = stream(a, B3)
        check('a stream whose client did not ask for usage gets no usage chunk, and still ends',
              not usage_only(events) and events[-1] == '[DONE]' and len(events) == 4)
        events = stream(a, B3, stream_options=dict(include_usage=True))
        check('a client that asks for usage still gets the usage chunk', len(usage_only(events)) == 1
              and usage_only(events)[0]['usage']['completion_tokens'] == 2)
        a.chat(model=B3, reasoning_effort='off', temperature=0)
        _, metrics = get(a, '/internal/metrics')
        usage = metrics['usage']
        check('tokens come from the backend usage: three streams and one buffered reply',
              usage['models'].get(B3) == dict(requests_completed=4, requests_without_usage=0, prompt_tokens=10,
                                             cached_prompt_tokens=3, output_tokens=7)
              and usage['usage_reported'] is True and isinstance(usage['since_unix_ms'], int))
        events = stream(a, B8, stream_options=dict(include_usage=True))
        _, metrics = get(a, '/internal/metrics')
        _, peer = get(b, '/internal/metrics')
        status, hosts = get(a, '/internal/hosts')
        total = sum(m.get(B8, {}).get('requests_completed', 0) for h in hosts['hosts'] for m in [h['usage']['models']])
        check('a request forwarded to a peer is counted once, by the peer, never by the coordinator',
              len(usage_only(events)) == 1 and B8 not in metrics['usage']['models']
              and peer['usage']['models'][B8]['requests_completed'] == 1 and total == 1
              and hosts['hosts'][1]['usage'] == peer['usage'])
        _, raw = post_stream(f'http://127.0.0.1:{a.port}', B3, text='HUGE')
        engine = json.loads((Path(a.state.name) / 'config.json').read_text())['deployments'][0]['base_url']
        _, direct = post_stream(engine, B3, text='HUGE', stream_options=dict(include_usage=True))
        events = direct.split(b'\n\n')
        unasked = b'\n\n'.join(e for e in events if not (b'"choices": []' in e and b'"usage"' in e))
        _, metrics = get(a, '/internal/metrics')
        check('an event past the 64 KiB bound passes through byte for byte; the usage chunk after it is still read and dropped',
              len(raw) > 256 << 10 and raw == unasked and raw != direct
              and metrics['usage']['models'][B3]['requests_completed'] == 5
              and metrics['usage']['models'][B3]['requests_without_usage'] == 0)
        array = json.dumps(dict(model=B3, messages=[dict(role='user', content='ARRAY')], reasoning_effort='off', temperature=0)).encode()
        replies = []
        for url, key in ((f'http://127.0.0.1:{a.port}', API), (engine, API)):
            req = urllib.request.Request(f'{url}/v1/chat/completions', array, {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
            with urllib.request.urlopen(req, timeout=60) as response:
                replies.append((response.status, response.read()))
        _, metrics = get(a, '/internal/metrics')
        check('a 200 whose top level is an array passes intact and counts as a request without usage',
              replies[0] == replies[1] and replies[0][0] == 200 and json.loads(replies[0][1]) == [dict(prompt_tokens=9)]
              and metrics['usage']['models'][B3]['requests_without_usage'] == 1 and metrics['usage']['usage_reported'] is False)
        status, _ = a.transcribe(SPEECH)
        _, metrics = get(a, '/internal/metrics')
        usage = metrics['usage']
        check('a reply without usage counts the request and no tokens, and the block says so',
              status == 200 and usage['models'][SPEECH] == dict(requests_completed=1, requests_without_usage=1,
                                                               prompt_tokens=0, cached_prompt_tokens=0, output_tokens=0)
              and usage['usage_reported'] is False)
        check('usage stays admin-only', get(a, '/internal/metrics', key=API)[0] == 401
              and get(a, '/internal/hosts', key=API)[0] == 401)
    finally:
        a.close()
    try:
        _, hosts = get(b, '/internal/hosts')
        gone = Router('cpu', models=(B3,), host='node-c',
                      peers=[dict(id='gone', url='http://127.0.0.1:9', token_env='PEER_TOKEN', models=[B8])])
        try:
            _, missing = get(gone, '/internal/hosts')
        finally:
            gone.close()
        check('an unreachable peer has usage null and a usage_error',
              missing['hosts'][1]['usage'] is None and missing['hosts'][1].get('usage_error')
              and hosts['hosts'][0]['usage']['models'][B8]['requests_completed'] == 1)
    finally:
        b.close()

    # Activity: one job per execution on the node that ran it, with the units its reply carried.
    def post(r, path, body, key=API, raw_reply=False):
        req = urllib.request.Request(f'http://127.0.0.1:{r.port}{path}', json.dumps(body).encode(),
                                     {'Authorization': 'Bearer ' + key, 'Content-Type': 'application/json'})
        try:
            with urllib.request.urlopen(req, timeout=180) as response:
                return response.status, response.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()
        except http.client.IncompleteRead:
            return -1, b''

    def activity(r, query='', key=ADMIN):
        req = urllib.request.Request(f'http://127.0.0.1:{r.port}/internal/activity{query}', headers={'Authorization': 'Bearer ' + key})
        try:
            with urllib.request.urlopen(req, timeout=60) as response:
                return response.status, json.loads(response.read())
        except urllib.error.HTTPError as e:
            return e.code, None

    def jobs(r):
        # Summed over buckets, so a run that crosses a 15-minute boundary still adds up.
        total = {}
        for bucket in activity(r)[1]['hosts'][0]['buckets']:
            t = total.setdefault((bucket['model'], bucket['capability']), dict(succeeded=0, failed=0, cancelled=0, units={}, units_unavailable=0))
            for k in ('succeeded', 'failed', 'cancelled', 'units_unavailable'):
                t[k] += bucket[k]
            for k, v in bucket['units'].items():
                t['units'][k] = t['units'].get(k, 0) + v
        return total

    say = lambda text, **body: dict(model=B3, messages=[dict(role='user', content=text)], reasoning_effort='off', temperature=0, **body)
    r = Router('cpu', models=(B3, SWITCH, SPEECH, 'qwen3-tts-0.6b-base', EMBED, TTM),
               auto=dict(enabled=True, audio=True, prefer=dict(chat=[B3], transcription=[SPEECH])))
    try:
        ok = [post(r, '/v1/chat/completions', say('hi'))[0],
              post(r, '/v1/systemone', dict(model=SWITCH, state={}, questions={
                  'tone': dict(instructions='pick', type='choice', criteria=dict(a='first', b='second')),
                  'risk': dict(instructions='rate', type='score', criteria=['low', 'high'])}))[0],
              r.transcribe(SPEECH)[0],
              post(r, '/v1/audio/speech', dict(model='qwen3-tts-0.6b-base', input='hello', response_format='wav'))[0],
              post(r, '/v1/embeddings', dict(model=EMBED, input=['a', 'b']))[0],
              post(r, '/v1/retrieval/rank', dict(model=EMBED, query='q', documents=[dict(id=i, text=i) for i in 'xyz']))[0],
              post(r, '/v1/forecast', dict(model=TTM, history=[1.0, 2.0, 3.0, 4.0], horizon=4))[0],
              r.chat(content=[dict(type='text', text='say'), audio()])[0]]
        bad = [post(r, '/v1/chat/completions', say('FAIL'))[0],
               post(r, '/v1/chat/completions', say('BREAK', stream=True))[0],
               post(r, '/v1/chat/completions', say('BIGJSON'))[0]]
        stream_error = post(r, '/v1/chat/completions', say('SSEERROR', stream=True))[0]
        # A client that leaves after the first event: the engine is still pausing, so the job is cancelled.
        conn = http.client.HTTPConnection('127.0.0.1', r.port, timeout=30)
        conn.request('POST', '/v1/chat/completions', json.dumps(say('DRIP', stream=True)),
                     {'Authorization': 'Bearer ' + API, 'Content-Type': 'application/json'})
        conn.getresponse().read1(64)
        conn.close()
        deadline = time.time() + 15
        while time.time() < deadline and jobs(r).get((B3, 'chat'), {}).get('cancelled', 0) < 1:
            time.sleep(0.25)
        # Disconnect while the worker is still computing and has not sent headers.
        conn = http.client.HTTPConnection('127.0.0.1', r.port, timeout=30)
        conn.request('POST', '/v1/chat/completions', json.dumps(say('SLOW')),
                     {'Authorization': 'Bearer ' + API, 'Content-Type': 'application/json'})
        time.sleep(0.5)
        conn.close()
        deadline = time.time() + 15
        while time.time() < deadline and jobs(r).get((B3, 'chat'), {}).get('cancelled', 0) < 2:
            time.sleep(0.25)
        j = jobs(r)
        check('every capability that ran is a succeeded job with its native units',
              ok == [200] * 8
              and j[(SWITCH, 'adapters')] == dict(succeeded=1, failed=0, cancelled=0, units_unavailable=0,
                                                 units=dict(questions_answered=2, input_tokens=20))
              and j[(SPEECH, 'transcription')]['units'] == dict(audio_seconds=1.0) and j[(SPEECH, 'transcription')]['succeeded'] == 2
              and j[('qwen3-tts-0.6b-base', 'speech_synthesis')]['units'] == dict(audio_seconds=0.75)
              and j[(EMBED, 'embeddings')]['units'] == dict(vectors=2, prompt_tokens=2)
              and j[(EMBED, 'retrieval')]['units'] == dict(documents_ranked=3)
              and j[(TTM, 'forecast')]['units'] == dict(forecast_steps=4))
        check('auto with audio counts one transcription job and one chat job on the node that ran them',
              j[(B3, 'chat')]['succeeded'] == 3 and j[(B3, 'chat')]['units'] == dict(prompt_tokens=2, output_tokens=2))
        check('a backend error and a stream broken midway are failed; a client that left is cancelled',
              bad[0] == 500 and bad[1] in (200, -1) and stream_error == 200 and j[(B3, 'chat')]['failed'] == 3 and j[(B3, 'chat')]['cancelled'] == 2)
        check('a reply past the read cap is a succeeded job with its units unavailable',
              bad[2] == 200 and j[(B3, 'chat')]['units_unavailable'] == 1)
        status, report = activity(r)
        host = report['hosts'][0] if status == 200 else {}
        check('/internal/activity names its bucket size, retention and this process\'s coverage',
              report['bucket_seconds'] == 900 and report['retention_days'] == 35 and host.get('host') == 'auto-smoke'
              and len(host['coverage']) == 1 and host['truncated'] is False
              and all(b['start_unix_ms'] % 900000 == 0 for b in host['buckets']))
        now = int(time.time() * 1000)
        check('/internal/activity is admin-only and bounds its window to 35 days',
              activity(r, key=API)[0] == 401
              and activity(r, f'?since_unix_ms={now - 36 * 86400000}&until_unix_ms={now}')[0] == 400
              and activity(r, f'?since_unix_ms={now}&until_unix_ms={now - 1}')[0] == 400
              and activity(r, '?since_unix_ms=yesterday')[0] == 400)
        _, metrics = get(r, '/internal/metrics')
        check('systemone input_tokens count as prompt tokens, and the request is not one without usage',
              metrics['usage']['models'][SWITCH] == dict(requests_completed=1, requests_without_usage=0, prompt_tokens=20,
                                                       cached_prompt_tokens=0, output_tokens=0))
        # A graceful stop drains requests in flight: this one finishes, counts, and is written before exit.
        with concurrent.futures.ThreadPoolExecutor(1) as pool:
            draining = pool.submit(post, r, '/v1/chat/completions', say('DRIP', stream=True))
            time.sleep(0.5)
            r.process.terminate()
            status, _ = draining.result(timeout=30)
        r.process.wait(timeout=30)
        stored = json.loads((Path(r.state.name) / 'activity.json').read_text())
        chats = [b for b in stored['buckets'] if b['model'] == B3 and b['capability'] == 'chat']
        check('a request in flight at a graceful stop finishes and its outcome is written before exit',
              status == 200 and sum(b['succeeded'] for b in chats) == 4 and sum(b['cancelled'] for b in chats) == 2)
    finally:
        r.close()

    # Vision normalization buffers the backend JSON before sending client headers.
    r = Router('cpu', models=(VISION,), backend='vllm')
    try:
        body = dict(model=VISION, prompt='BUFFER', mime_type='image/png', image_base64=PNG.split(',')[1])
        warm = post(r, '/v1/vision/extract', dict(body, prompt='hi'))[0]
        conn = http.client.HTTPConnection('127.0.0.1', r.port, timeout=30)
        conn.request('POST', '/v1/vision/extract', json.dumps(body),
                     {'Authorization': 'Bearer ' + API, 'Content-Type': 'application/json'})
        time.sleep(1)
        conn.close()
        deadline = time.time() + 10
        while time.time() < deadline and jobs(r).get((VISION, 'vision'), {}).get('cancelled', 0) < 1:
            time.sleep(0.25)
        j = jobs(r).get((VISION, 'vision'), {})
        check('disconnect while a vision response body is buffered records cancellation once',
              warm == 200 and j.get('cancelled') == 1 and j.get('succeeded') == 1 and j.get('failed') == 0)
    finally:
        r.close()

    # Both held: nothing may load.
    r = Router('unified', hold=True)
    try:
        status, head, body, _, _ = r.chat()
        check('both held: 503 auto_unavailable/held with no Retry-After',
              status == 503 and body['error'].get('reason') == 'held' and 'retry-after' not in head
              and 'name a model' not in body['error']['message'])
    finally:
        r.close()

    for name, ok in checks:
        print(('PASS ' if ok else 'FAIL ') + name)
    sys.exit(0 if all(ok for _, ok in checks) else 1)


if __name__ == '__main__':
    main()
