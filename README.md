<p align="center"><img src="docs/how-it-works.svg" alt="Apps send requests to one LocalFleet endpoint, which picks the device and the model, and answers from a Mac, a gaming PC or a mini PC" width="100%"></p>

# LocalFleet

**Private AI that chats, sees, listens and forecasts, on the computers you already own.**

LocalFleet turns your Mac, gaming PC and mini PC into one AI server. Point any app that speaks the OpenAI API at it, name a model, and LocalFleet runs it on a device with room for it. Nothing leaves your network.

Turn on `auto` in your profile ([example](configs/examples/auto.json)) and you can ask for `"model": "auto"`. LocalFleet then answers with a model that is already loaded, preferring the larger one, or loads one only where that evicts nothing. Effort is off unless you ask for it. Send `x-granite-conversation` to keep a conversation on one model while it stays available. Older nodes in a mixed fleet may still load on their own (see [SECURITY.md](SECURITY.md)).

- **One URL for every kind of model:** chat and reasoning, vision, speech-to-text, embeddings, forecasting, with text-to-speech in preview.
- **Every chip you own:** Apple GPUs, NVIDIA, AMD and Intel integrated GPUs, and plain CPUs.
- **Faster than a proxy:** LocalFleet adds at most 6 ms. LiteLLM adds up to 16 ms more on the same requests ([benchmarks](benchmarks/RESULTS.md)).
- **Safe by default:** a model loads only if it fits, and no port is opened to your network.
- **Open source:** Apache-2.0, built around IBM Granite.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/namastex888/localfleet/dev/install.sh | sh
```

The installer looks at your hardware, shows what it will run, asks, then sets everything up as a login service. It needs only `git` and `curl`: Python comes from [uv](https://docs.astral.sh/uv/), and every download is checked against a pinned SHA-256. When it finishes, it prints your URL and where your key is.

Add another computer you can reach over SSH. LocalFleet copies itself there, installs, and joins it to the fleet:

```sh
localfleet node add you@gaming-pc.local
```

On a Mac, `apps/macos/build-app.sh` builds the menu-bar app: live status, models, devices and nodes in a System Settings-style window.

## Use

```sh
export LOCALFLEET_KEY=$(localfleet key)
curl http://127.0.0.1:8310/v1/chat/completions \
  -H "Authorization: Bearer $LOCALFLEET_KEY" -H "Content-Type: application/json" \
  -d '{"model": "granite-4.2-3b", "reasoning_effort": "off", "temperature": 0, "messages": [{"role": "user", "content": "Summarize this contract in 3 bullets: ..."}]}'
```

Any OpenAI SDK works: set `base_url` to `http://127.0.0.1:8310/v1`. `auto` works through this URL; a LiteLLM proxy in front knows only its own aliases. Each answer says where it ran, in headers like `x-granite-host`, `x-granite-model-alias` and `server-timing`.

Send `X-LocalFleet-Client: <name>` (1–32 of `a-z 0-9 . _ -`) to have the admin view (`/internal/metrics`, `/internal/hosts`) count your app's requests by name. The name is self-declared, counted only on the node the request enters, and never used for routing, admission or auth. Requests through a LiteLLM proxy (`:8300`) count as unidentified: LiteLLM passes `x-` headers on only with `forward_client_headers_to_llm_api`, which would also pass on a spoofable `x-granite-hop`.

The admin view `GET /internal/activity?since_unix_ms=&until_unix_ms=` lists the jobs each node ran, per 15-minute UTC bucket, model and capability: `succeeded`, `failed` (the server's fault: a 5xx, a timeout, a broken stream), `rejected` (the backend refused the input with a 4xx, so the caller must fix the request), `cancelled` and the work units each reply reported (tokens for chat, vectors for embeddings, audio seconds for speech, and so on), plus each node's uptime as `coverage`. A request forwarded to a peer is counted by the peer only, so the fleet total counts every job once. One request spans at most 35 days. For chat, `output_tokens` is everything generated, thinking included; `reasoning_tokens`, when present, is a share of it, never extra. A graceful stop lets requests in flight finish and count; a request in flight when the process is killed records no outcome, and a crash loses at most the last 30 s of counts.

| Endpoint | What it does |
| --- | --- |
| `/v1/chat/completions` | Chat and reasoning. `model: "auto"` or a model name; `reasoning_effort` off, low or full; `max_tokens` is the reply, `thinking_budget_tokens` caps thinking |
| `/v1/embeddings` | Vectors for search and memory. `model: "auto"` uses one pinned model |
| `/v1/audio/transcriptions` | Speech to text |
| `/v1/audio/speech` | Text to speech (preview) |
| `/v1/vision/extract` | Read images and documents |
| `/v1/forecast` | Time-series forecasting, with quantiles. `model: "auto"` uses one pinned model |
| `/v1/models` | Every model on every device, and what it can do |

## Speed

| On a MacBook Pro M5 Pro | Result |
| --- | --- |
| Overhead of LocalFleet vs. calling the engine directly | **at most 6 ms** |
| Extra cost of LiteLLM 1.106 on the same requests | 1–16 ms |
| Granite 4.2 3B, 4 conversations at once | **281 tokens/s** total |
| First token, short prompt | 23 ms |

All numbers, methods and raw data: [benchmarks/RESULTS.md](benchmarks/RESULTS.md).

## What runs where

`localfleet scan` decides from measured hardware, never from a chip's name.

| Device | Engine | Models |
| --- | --- | --- |
| Apple silicon | oMLX or llama.cpp on Metal | Granite 4.2 3B and 8B |
| NVIDIA GPU | llama.cpp CUDA, vLLM | Granite 4.2, Switch 4.1 8B, Vision 4.1 4B; Fish S2 Pro voices (preview) |
| AMD or Intel integrated GPU | llama.cpp Vulkan | Granite 4.2 3B and 8B |
| Any CPU | PyTorch workers | Embedding 97M, TTM R3 forecasting, Speech 5.0 |
| AMD NPU | detected | none yet: no Granite build for it exists |

## How it works

- Each computer runs **granited**, a small Rust agent. It measures the hardware and starts the right engine for each model.
- Requests go to the best device: one with the model already loaded and a free slot first, then one with enough free memory. A device that just filled up hands the request to the next one.
- Memory budgets come from measured peaks. Idle models unload, and busy ones are never interrupted.
- Computers talk over SSH tunnels with keys. Nothing listens on your network. See [SECURITY.md](SECURITY.md).

## Develop

```sh
cargo test --locked && python3 scripts/smoke_stub.py
```

`workers/stub.py` stands in for a model, so you can work on the router without downloading anything. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Names

LocalFleet was called granited until 2026-10-26. The engine keeps that name: the `granited` binary, `ai.granited.*` services, `x-granite-*` headers and `GRANITE_*` keys. `scripts/granitectl.py` still works as an alias of `localfleet`.

## License

Apache-2.0. Granite models are released by IBM under their own licenses on Hugging Face.
