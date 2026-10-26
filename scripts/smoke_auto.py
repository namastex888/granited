#!/usr/bin/env python3
"""End-to-end check of `model: "auto"` for chat, with two stub chat models and no real weights.

Each scenario starts the router on a fresh profile: the stubs stand in for Granite 4.2 8B and 3B,
sharing one exclusive device (a GPU-like rule: one resident model) or the CPU (no such rule).
The checks cover the auto v0 spec: warm before cold, no load that evicts a ready model, visible
fallbacks, the temperature clamp, sticky conversations, the closed aggregate refusal, and effort.
"""
import base64
import concurrent.futures
import json
import os
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
    def __init__(self, device, hold=False, extra=(), models=None, peers=(), host='auto-smoke', auto=None):
        self.state = tempfile.TemporaryDirectory()
        self.port = free_port()
        deployments = []
        for model in (models or (B8, B3)) + tuple(extra):
            port = free_port()
            family = 'speech' if model == SPEECH else 'speech_synthesis' if model.startswith('qwen3-tts') else 'language'
            deployments.append(dict(
                model=model, backend=family + '_native', base_url=f'http://127.0.0.1:{port}',
                revision=REGISTRY[model]['hf_revision'], artifact_repo='stub',
                command=['python3', 'workers/stub.py', '--family', family, '--model', model,
                         '--revision', REGISTRY[model]['hf_revision'], '--port', str(port)],
                memory_bytes=64 << 20, context_tokens=4096, device=device, health_path='/healthz', identity_path='/internal/metadata',
                runtime_manifest='workers/stub.py', backend_build='stub', max_concurrency=1, load_timeout_seconds=60,
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
    finally:
        a.close()
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
