# Changelog

## Unreleased

### Breaking

- **Chat content parts are allowlisted** on every `/v1/chat/completions` request, including requests that name a model. The accepted types are `text`, `image_url`, `input_audio` and `refusal`. Any other type, or a part without a type, gets 400 `invalid_schema`, with the message `content part type ... is not accepted`.
- **Message `content` must be a string, an array of parts, or null** on every chat request, including requests that name a model. Anything else gets 400 `invalid_schema` with the message `message content must be a string or an array of parts`.
- **`image_url` must be a `data:` URI:** png, jpeg (or jpg) or webp, in standard base64 with no line breaks, at most 8 MiB decoded.
  - A remote `http(s)` URL gets 400 `invalid_schema` with the message `image_url must be a data: URI (data:image/png;base64,...); remote URLs are not fetched`.
  - To fix a request, fetch the image yourself and send it inline.

### Fixed

- The first requests after a manager restart no longer get `capacity_unavailable` for models on an operator-owned router. The manager now reads those routers once before it listens; before, it listened while that first read was still pending.
- A manager killed outright (crash, OOM, SIGKILL) no longer leaves its node unable to serve: its orphaned workers held their ports, and the restarted manager refused them with `worker_port_in_use`. It now reaps exactly the workers its own ledger names. `scripts/smoke_mesh.py` covers a 3-node mesh in which one node dies and rejoins.
- A transcription that one node forwards to a peer is served. The peer's `/v1/audio/transcriptions` now also takes the JSON form `{model, audio_base64}`; before, it took only multipart and refused the forwarded request with 400 `Invalid boundary`.

### Added

- Client counts (admin-only): an optional `X-LocalFleet-Client` header, 1–32 characters from `a-z 0-9 . _ -` (anything else counts as unidentified). `GET /internal/metrics` gains `clients`: `window_seconds` (300), requests in flight per label (`active`), `unidentified_active`, and `last_seen_unix_ms` per label, for at most 32 labels (the least recently seen idle label is evicted first). Only the node a request enters counts it, and the header is never forwarded. `GET /internal/hosts` gives each host its `clients` block, or `null` with `clients_error`. The label is self-declared and never used for routing, admission or auth. Known limit: requests through LiteLLM (`:8300`) count as unidentified, because LiteLLM forwards `x-` headers only with `forward_client_headers_to_llm_api`, which would also forward a spoofable `x-granite-hop`; that stays a separate decision.
- Usage counters (admin-only). `GET /internal/metrics` gains `usage`: per model, `requests_completed`, `requests_without_usage`, `prompt_tokens`, `cached_prompt_tokens` and `output_tokens` since the engine started (`since_unix_ms`), and `usage_reported: false` once a completed request came back without usage. Only the node that ran the backend counts a request, so a forwarded request is counted once, by the peer. Tokens come from the backend's own `usage`, never from counting chunks. Streamed chat asks the engine for `stream_options.include_usage`; the final usage-only chunk reaches the client only if the client asked for it. An event over 64 KiB passes through as it arrives, unread, and reading resumes after it. A JSON reply over 8 MiB, or one whose top level is not an object, counts as a request without usage. Audio replies count as completed and leave `usage_reported` alone. A stream cut short by the client is not counted. `GET /internal/hosts` gives each host its own `usage` block (a peer's from its `/internal/metrics`), or `usage: null` with `usage_error`.
- Power per accelerator in `GET /internal/hardware`: `power` with `watts`, `limit_watts`, `scope`, `source` and `reason`. NVIDIA from `nvidia-smi power.draw`/`power.limit`, AMD from amdgpu hwmon `power1_average`; an unreadable sensor is `null` with a reason, never 0, and macOS reports `null` because `powermetrics` needs root. A drm card that `nvidia-smi` already lists is no longer reported a second time.
- `model: "auto"` on `/v1/audio/speech` behind `auto.speech` (off by default): the best `prefer.speech_synthesis` tier that is warm or fits without evicting, on this node only.
- `GET /v1/devices`: each host's models per capability, in placement's words, with a `recommended` model per capability (the best registry `tier` that fits, warm first).
- Qwen3-TTS 1.7B and 0.6B Base (Apache-2.0) in the registry as the `good_gpu` and `gpu_poor` voice tiers, with a worker for `/v1/audio/speech` (in-context voice clone; there is no default voice).
- `/v1/models` reports `context_tokens` and `context_source` (`engine` once the engine reported it at load, else `config`) per model, and replies carry `x-granite-context-tokens`: the context one request gets on that node.
- `model: "auto"` can take image input (`auto.images`) and audio input (`auto.audio`). Both are off by default and enabled on no profile until a measured pass on the real models.
