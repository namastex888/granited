#!/usr/bin/env python3
"""One native architecture per owned process. Rust owns placement and lifecycle."""
import argparse
import base64
import importlib.metadata
import io
import json
import math
import platform
import resource
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Unsupported(ValueError):
    pass


class Native:
    def __init__(self, args):
        self.args = args
        self.started = time.perf_counter()
        import torch
        self.torch = torch
        torch.set_num_threads(args.threads)
        if len(args.revision) != 40 or any(c not in '0123456789abcdef' for c in args.revision):
            raise ValueError('An immutable Hub revision is required')
        if args.device == 'cuda' and not torch.cuda.is_available():
            raise Unsupported('CUDA is not available')
        if args.device == 'mps' and not torch.backends.mps.is_available():
            raise Unsupported('Metal/MPS is not available')
        kwargs = dict(revision=args.revision, trust_remote_code=False)
        if args.family == 'embedding':
            from sentence_transformers import SentenceTransformer
            if args.multimodal:
                # Pinned weights on disk; float32 on CPU, bfloat16 on GPUs. float16 overflows this family.
                dtype = torch.float32 if args.device == 'cpu' else torch.bfloat16
                self.model = SentenceTransformer(args.weights, device=args.device, trust_remote_code=False,
                                                 local_files_only=True, model_kwargs=dict(dtype=dtype))
            else:
                self.model = SentenceTransformer(args.repo, device=args.device, **kwargs)
            self.model.eval()
        elif args.family == 'speech':
            from transformers import AutoModelForCTC, AutoProcessor
            self.processor = AutoProcessor.from_pretrained(args.repo, **kwargs)
            self.model = AutoModelForCTC.from_pretrained(args.repo, **kwargs).to(args.device).eval()
        elif args.family == 'timeseries':
            from huggingface_hub import hf_hub_download
            from tsfm_public.models.tinytimemixer.configuration_tinytimemixer import TinyTimeMixerConfig
            from tsfm_public.models.tinytimemixer.modeling_tinytimemixer import TinyTimeMixerForDecomposedPrediction, TinyTimeMixerForPrediction
            cfg_path = hf_hub_download(args.repo, 'config.json', revision=args.revision)
            cfg = json.load(open(cfg_path))
            cls = TinyTimeMixerForDecomposedPrediction if cfg.get('decompose') else TinyTimeMixerForPrediction
            self.model = cls.from_pretrained(args.repo, revision=args.revision).to(args.device).eval()
            self.config = self.model.config
        else:
            raise Unsupported(f'No validated specialist implementation for {args.family}')
        self.lock = threading.Lock()
        self.load_seconds = time.perf_counter() - self.started
        packages = ('torch', 'transformers', 'sentence-transformers', 'granite-tsfm', 'huggingface-hub', 'numpy', 'av', 'soxr', 'pillow')
        self.versions = {}
        for name in packages:
            try:
                self.versions[name] = importlib.metadata.version(name)
            except importlib.metadata.PackageNotFoundError:
                pass

    def metadata(self):
        rss = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
        capabilities = {}
        if self.args.family == 'embedding':
            capabilities = dict(dimensions=self.model.get_sentence_embedding_dimension(), model_max_tokens=self.model.max_seq_length,
                                deployment_max_tokens=self.args.max_input_tokens, batch_size=self.args.batch_size, normalization='l2')
            if self.args.multimodal:
                capabilities.update(modalities=['text', 'image', 'audio', 'video'], output_dimensions=self.args.dimensions,
                                    tasks=sorted(self.model.prompts), media_token_cost=MEDIA_COST,
                                    media=dict(image=sorted(MEDIA['image']), audio=sorted(MEDIA['audio']), video=sorted(MEDIA['video'])))
        elif self.args.family == 'timeseries':
            capabilities = dict(context_length=self.config.context_length, max_horizon=self.config.prediction_length,
                                quantiles=list(self.config.quantile_levels), channels=self.config.num_input_channels)
        elif self.args.family == 'speech':
            capabilities = dict(sample_rate=self.processor.feature_extractor.sampling_rate, streaming=False, max_audio_seconds=self.args.max_audio_seconds)
        return dict(model=self.args.model, repo=self.args.repo, revision=self.args.revision,
                    family=self.args.family, backend=self.args.family + '_native', device=self.args.device,
                    versions=self.versions, load_seconds=self.load_seconds,
                    peak_rss_bytes=rss if platform.system() == 'Darwin' else rss * 1024,
                    python=platform.python_version(), remote_code=False, capabilities=capabilities,
                    processor_revision=self.args.revision)

    def encode_multimodal(self, data):
        """Text, image, audio and video items, alone or interleaved, into one vector space. Media are data: URIs,
        decoded here; nothing is fetched or read from disk. A task applies its prompt to plain-text items only."""
        items = data.get('input')
        items = [items] if isinstance(items, (str, dict)) else items
        if not isinstance(items, list) or not 0 < len(items) <= 64:
            raise ValueError('input requires 1..64 items: text, or objects with text, image, audio and video')
        task = data.get('task')
        if task is not None and task not in self.model.prompts:
            raise ValueError(f'task must be one of {sorted(self.model.prompts)}')
        dims = data.get('dimensions')
        if dims is not None and dims not in self.args.dimensions:
            raise ValueError(f'dimensions must be one of {self.args.dimensions}')
        cap = min(self.model.max_seq_length, self.args.max_input_tokens)
        prepared, total = [], 0
        for i, item in enumerate(items):
            value, cost = self.media_item(item, task)
            if cost > cap:
                raise ValueError(f'input[{i}] needs {cost} tokens; this deployment takes {cap} and never truncates')
            prepared.append(value)
            total += cost
        vectors = [self.model.encode([v], normalize_embeddings=True, convert_to_numpy=True, truncate_dim=dims)[0] for v in prepared]
        return vectors, total

    def media_item(self, item, task):
        """One input item as sentence-transformers takes it, and the tokens it costs in the shared context."""
        if isinstance(item, str):
            text = self.model.prompts[task] + item if task else item
            return text, len(self.model.tokenizer(text)['input_ids'])
        if not isinstance(item, dict) or not item or set(item) - {'text', 'image', 'audio', 'video'}:
            raise ValueError('an input object takes text, image, audio and video')
        value, cost = {}, 0
        for kind in ('image', 'audio', 'video'):
            uris = item.get(kind, [])
            uris = [uris] if isinstance(uris, str) else uris
            if not isinstance(uris, list):
                raise ValueError(f'{kind} must be a data: URI or a list of them')
            decoded = [decode_media(kind, uri) for uri in uris]
            for media, units in decoded:
                cost += MEDIA_COST[kind] * units
            if decoded:
                value[kind] = [media for media, _ in decoded]
        text = item.get('text')
        if text is None:
            if sum(len(v) for v in value.values()) != 1:
                raise ValueError('an object without text holds exactly one image, audio or video')
            kind, media = next((k, v[0]) for k, v in value.items())
            # A bare frame list would read as a batch of images; a video goes in its own key.
            return ({'video': media} if kind == 'video' else media), cost
        if not isinstance(text, str):
            raise ValueError('text must be a string')
        for kind in ('image', 'audio', 'video'):
            if text.count(f'<|{kind}|>') != len(value.get(kind, [])):
                raise ValueError(f'text needs one <|{kind}|> per {kind}, in order')
        value = {k: v if len(v) > 1 else v[0] for k, v in value.items()}
        value['text'] = text
        return value, cost + len(self.model.tokenizer(text)['input_ids'])

    def encode(self, texts):
        if isinstance(texts, str):
            texts = [texts]
        if not isinstance(texts, list) or not texts or len(texts) > 1024 or any(not isinstance(t, str) for t in texts):
            raise ValueError('input requires 1..1024 texts')
        lengths = [len(ids) for ids in self.model.tokenizer(texts, truncation=False)['input_ids']]
        cap = min(self.model.max_seq_length, self.args.max_input_tokens)
        if max(lengths) > cap:
            raise ValueError(f'Input exceeds this memory profile cap of {cap} tokens; model context is {self.model.max_seq_length}; truncation is never implicit')
        vectors = self.model.encode(texts, normalize_embeddings=True, convert_to_numpy=True, batch_size=self.args.batch_size)
        return vectors, sum(lengths)

    def infer(self, path, data):
        if data.get('model') != self.args.model:
            raise ValueError('Model alias differs from loaded artifact')
        with self.lock, self.torch.inference_mode():
            started = time.perf_counter()
            if self.args.family == 'embedding' and path == '/v1/embeddings':
                if data.get('encoding_format', 'float') != 'float':
                    raise Unsupported('Only float embeddings are supported')
                if self.args.multimodal:
                    vectors, tokens = self.encode_multimodal(data)
                elif data.get('dimensions') is not None:
                    raise Unsupported('Dimension reduction has not been validated for this worker')
                else:
                    vectors, tokens = self.encode(data.get('input'))
                result = dict(object='list', model=self.args.model,
                              data=[dict(object='embedding', index=i, embedding=v.tolist()) for i, v in enumerate(vectors)],
                              usage=dict(prompt_tokens=tokens, total_tokens=tokens))
            elif self.args.family == 'embedding' and path == '/v1/retrieval/rank':
                docs = data['documents']
                if not docs or len(docs) > 1024 or len({d['id'] for d in docs}) != len(docs):
                    raise ValueError('Expected 1..1024 documents with unique IDs')
                query, _ = self.encode(data['query'])
                vectors, _ = self.encode([d['text'] for d in docs])
                scores = (vectors @ query[0]).tolist()
                order = sorted(range(len(docs)), key=lambda i: scores[i], reverse=True)
                result = dict(ranked_ids=[docs[i]['id'] for i in order], scores=[scores[i] for i in order])
            elif self.args.family == 'timeseries' and path == '/v1/forecast':
                history, horizon = data['history'], data['horizon']
                context = self.config.context_length
                if not isinstance(horizon, int) or horizon < 1 or horizon > self.config.prediction_length:
                    raise Unsupported(f'horizon must be in 1..{self.config.prediction_length}')
                if len(history) != context or any(not isinstance(x, (float, int)) or not math.isfinite(x) for x in history):
                    raise ValueError(f'This immutable TTM profile requires exactly {context} finite, equally spaced observations')
                if data.get('frequency') is not None:
                    raise Unsupported('Frequency conditioning not validated for this checkpoint')
                requested = data.get('quantiles', [])
                levels = list(getattr(self.config, 'quantile_levels', []) or [])
                if any(q not in levels for q in requested):
                    raise Unsupported(f'Supported quantiles: {levels}')
                tensor = self.torch.tensor(history, dtype=self.torch.float32, device=self.args.device).reshape(1, context, 1)
                output = self.model(past_values=tensor)
                result = dict(forecast=output.prediction_outputs[0, :horizon, 0].float().cpu().tolist(),
                              context_length=context, horizon=horizon, units='input_units')
                quantiles = getattr(output, 'quantile_outputs', None)
                if requested:
                    if quantiles is None:
                        raise Unsupported('This checkpoint did not produce quantiles')
                    # TSFM 0.3.11 returns [batch, quantiles, prediction_length, channels].
                    if tuple(quantiles.shape) != (1, len(levels), self.config.prediction_length, 1):
                        raise Unsupported(f'Unvalidated native quantile shape {tuple(quantiles.shape)}')
                    result['quantiles'] = {str(q): quantiles[0, levels.index(q), :horizon, 0].float().cpu().tolist() for q in requested}
            elif self.args.family == 'speech' and path in ('/v1/audio/transcriptions', '/v1/speech/transcribe'):
                import numpy as np
                if 'audio_base64' in data:
                    import soundfile as sf
                    raw = base64.b64decode(data['audio_base64'], validate=True)
                    if len(raw) > 25 * 1024 * 1024:
                        raise ValueError('Audio file exceeds 25 MiB')
                    try:
                        samples, rate = sf.read(io.BytesIO(raw), dtype='float32')
                    except sf.LibsndfileError:
                        raise ValueError('Audio is not a readable wav, flac or ogg file') from None
                    if samples.ndim != 1:
                        raise ValueError('Expected mono audio; downmix explicitly')
                else:
                    samples, rate = np.asarray(data['samples'], dtype='float32'), data['sample_rate']
                expected = self.processor.feature_extractor.sampling_rate
                if rate != expected:
                    raise ValueError(f'Expected {expected} Hz; resample explicitly')
                if samples.ndim != 1 or not 0 < samples.size <= expected * self.args.max_audio_seconds or not np.isfinite(samples).all() or abs(samples).max() > 1:
                    raise ValueError(f'Expected up to {self.args.max_audio_seconds} seconds mono PCM in [-1,1]')
                language = data.get('language')
                if language is not None and language not in ('en', 'fr', 'de', 'es', 'pt', 'it', 'ja'):
                    raise Unsupported('Language hint is outside the pinned multilingual card')
                inputs = self.processor([samples], sampling_rate=rate, device=self.args.device)
                inputs = inputs.to(self.args.device, dtype=self.model.dtype)
                outputs = self.model.generate(**inputs)
                text = self.processor.batch_decode(outputs, skip_special_tokens=True)[0]
                result = dict(text=text, language=None, requested_language=language,
                              audio_duration_seconds=samples.size / rate,
                              note='Language hint is not model-detected language')
            else:
                raise Unsupported(f'{self.args.family} does not implement {path}')
            result['execution'] = dict(inference_seconds=time.perf_counter() - started, **self.metadata())
            return result


# What EmbeddingGemma 2 charges each media unit in its shared context: an image, a video frame (sampled at
# 1 fps), a second of audio (mono, 16 kHz).
MEDIA_COST = dict(image=280, video=140, audio=25)
MEDIA = dict(image={'image/png': 8, 'image/jpeg': 8, 'image/webp': 8},
             audio={'audio/wav': 25, 'audio/x-wav': 25, 'audio/flac': 25, 'audio/ogg': 25},
             video={'video/mp4': 24, 'video/webm': 24, 'video/quicktime': 24})  # MiB decoded, at most


def decode_media(kind, uri):
    """A data: URI into what the model takes, and its units: (PIL image, 1), (audio dict, seconds) or
    (frames at 1 fps, frame count). Audio is downmixed to mono and resampled to 16 kHz."""
    if not isinstance(uri, str) or not uri.startswith('data:') or ';base64,' not in uri:
        raise ValueError(f'{kind} must be a data: URI ({kind}/...;base64,...); nothing is fetched')
    mime, payload = uri[5:].split(';base64,', 1)
    if mime == 'image/jpg':
        mime = 'image/jpeg'
    if mime not in MEDIA[kind]:
        raise ValueError(f'{kind} must be one of {sorted(MEDIA[kind])}')
    try:
        raw = base64.b64decode(payload, validate=True)
    except ValueError:
        raise ValueError(f'{kind} is not valid base64') from None
    if not raw or len(raw) > MEDIA[kind][mime] * 1024 * 1024:
        raise ValueError(f'{kind} must be 1 byte to {MEDIA[kind][mime]} MiB decoded')
    if kind == 'image':
        from PIL import Image
        Image.MAX_IMAGE_PIXELS = 40_000_000
        try:
            image = Image.open(io.BytesIO(raw))
            image.load()
        except Exception:
            raise ValueError('image is not a readable png, jpeg or webp') from None
        return image.convert('RGB'), 1
    if kind == 'audio':
        import numpy as np
        import soundfile as sf
        import soxr
        try:
            samples, rate = sf.read(io.BytesIO(raw), dtype='float32', always_2d=True)
        except sf.LibsndfileError:
            raise ValueError('audio is not a readable wav, flac or ogg file') from None
        mono = samples.mean(axis=1)
        if rate != 16000:
            mono = soxr.resample(mono, rate, 16000)
        if not mono.size or not np.isfinite(mono).all():
            raise ValueError('audio is empty or not finite')
        return dict(array=np.ascontiguousarray(mono, dtype=np.float32), sampling_rate=16000), math.ceil(mono.size / 16000)
    import av
    frames = []
    try:
        with av.open(io.BytesIO(raw)) as container:
            stream = container.streams.video[0]
            next_at = 0.0
            for frame in container.decode(stream):
                at = float(frame.pts * stream.time_base) if frame.pts is not None else next_at
                if at + 1e-6 >= next_at:
                    frames.append(frame.to_image())
                    next_at += 1.0
                    if len(frames) > 120:
                        raise ValueError('video is longer than 120 s at 1 frame per second')
    except ValueError:
        raise
    except Exception:
        raise ValueError('video is not a readable mp4, webm or mov file') from None
    if not frames:
        raise ValueError('video has no frames')
    return frames, len(frames)


def serve(native, args):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = 'HTTP/1.1'
        # Keep-alive clients otherwise wait ~40 ms on Linux delayed ACK for a second small write.
        disable_nagle_algorithm = True

        def send(self, status, data):
            payload = json.dumps(data, allow_nan=False).encode()
            self.send_response(status)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(payload)))
            self.send_header('x-granite-model-revision', args.revision)
            self.send_header('x-granite-backend', args.family + '_native')
            self._headers_buffer.append(b'\r\n' + payload)
            self.flush_headers()

        def do_GET(self):
            if self.path in ('/healthz', '/internal/metadata'):
                self.send(200, native.metadata())
            elif self.path == '/v1/models':
                self.send(200, dict(object='list', data=[dict(id=args.model, object='model', revision=args.revision)]))
            else:
                self.send(404, dict(error=dict(code='route_not_found', message=self.path)))

        def do_POST(self):
            try:
                length = int(self.headers.get('Content-Length', '0'))
                if not 0 < length <= 32 * 1024 * 1024:
                    raise ValueError('Expected a JSON body up to 32 MiB')
                data = json.loads(self.rfile.read(length))
                self.send(200, native.infer(self.path, data))
            except Unsupported as e:
                self.send(501, dict(error=dict(code='unsupported_capability', message=str(e))))
            except (ValueError, KeyError, TypeError) as e:
                self.send(400, dict(error=dict(code='invalid_schema', message=str(e))))
            except Exception as e:
                oom = 'out of memory' in str(e).lower()
                self.send(503 if oom else 500, dict(error=dict(code='out_of_memory' if oom else 'inference_failed', message=f'{type(e).__name__}: {e}')))

    print(json.dumps(native.metadata()), flush=True)
    ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--model', required=True)
    parser.add_argument('--repo', required=True)
    parser.add_argument('--revision', required=True)
    parser.add_argument('--family', choices=['embedding', 'timeseries', 'speech'], required=True)
    parser.add_argument('--device', choices=['cpu', 'mps', 'cuda'], default='cpu')
    parser.add_argument('--host', default='127.0.0.1', choices=['127.0.0.1'])
    parser.add_argument('--port', type=int, required=True)
    parser.add_argument('--threads', type=int, default=4)
    parser.add_argument('--max-input-tokens', type=int, default=512)
    parser.add_argument('--batch-size', type=int, default=1)
    parser.add_argument('--max-audio-seconds', type=int, default=30)
    parser.add_argument('--multimodal', action='store_true', help='embedding: text, image, audio and video items from --weights')
    parser.add_argument('--weights', help='local directory of the pinned --revision; required with --multimodal')
    parser.add_argument('--dimensions', type=lambda v: [int(x) for x in v.split(',')], default=[],
                        help='output dimensions a request may ask for (Matryoshka), e.g. 768,512,256,128')
    args = parser.parse_args()
    if args.multimodal and (args.family != 'embedding' or not args.weights):
        parser.error('--multimodal is an embedding option and needs --weights')
    serve(Native(args), args)


if __name__ == '__main__':
    main()
