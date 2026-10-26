# Changelog

## Unreleased

### Breaking

- **Chat content parts are allowlisted** on every `/v1/chat/completions` request, including requests that name a model. The accepted types are `text`, `image_url`, `input_audio` and `refusal`. Any other type, or a part without a type, gets 400 `invalid_schema`, with the message `content part type ... is not accepted`.
- **Message `content` must be a string, an array of parts, or null** on every chat request, including requests that name a model. Anything else gets 400 `invalid_schema` with the message `message content must be a string or an array of parts`.
- **`image_url` must be a `data:` URI:** png, jpeg (or jpg) or webp, in standard base64 with no line breaks, at most 8 MiB decoded.
  - A remote `http(s)` URL gets 400 `invalid_schema` with the message `image_url must be a data: URI (data:image/png;base64,...); remote URLs are not fetched`.
  - To fix a request, fetch the image yourself and send it inline.

### Fixed

- A transcription that one node forwards to a peer is served. The peer's `/v1/audio/transcriptions` now also takes the JSON form `{model, audio_base64}`; before, it took only multipart and refused the forwarded request with 400 `Invalid boundary`.

### Added

- `model: "auto"` on `/v1/audio/speech` behind `auto.speech` (off by default): the best `prefer.speech_synthesis` tier that is warm or fits without evicting, on this node only.
- `GET /v1/devices`: each host's models per capability, in placement's words, with a `recommended` model per capability (the best registry `tier` that fits, warm first).
- Qwen3-TTS 1.7B and 0.6B Base (Apache-2.0) in the registry as the `good_gpu` and `gpu_poor` voice tiers, with a worker for `/v1/audio/speech` (in-context voice clone; there is no default voice).
- `/v1/models` reports `context_tokens` and `context_source` (`engine` once the engine reported it at load, else `config`) per model, and replies carry `x-granite-context-tokens`: the context one request gets on that node.
- `model: "auto"` can take image input (`auto.images`) and audio input (`auto.audio`). Both are off by default and enabled on no profile until a measured pass on the real models.
