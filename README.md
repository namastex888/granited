# granited

One endpoint for every IBM Granite model family, served from the machines you already own.

granited runs a small Rust agent on each machine. The agent measures the hardware, starts the right native runtime for each Granite model (oMLX on Apple silicon; llama.cpp on Metal, Vulkan, CUDA or CPU; PyTorch workers for embeddings, speech and time series; vLLM for vision), and keeps each model inside the memory it can actually afford. Machines join each other over SSH, and any OpenAI client can talk to the one endpoint they share.

```
   any OpenAI client, curl, the macOS menu-bar app
                              │
                     granited (coordinator)          one URL, one key
            ┌─────────────────┼──────────────────┐
     this machine        node over SSH        node over SSH
  oMLX · llama.cpp Metal    llama.cpp · Vulkan    llama.cpp · CUDA
  embeddings · CPU     speech · TTM · CPU    Switch · vision
```

## Quick start

```sh
git clone <this repository> granited && cd granited
python3 scripts/granitectl.py scan        # what this machine can run; changes nothing
python3 scripts/granitectl.py install     # shows the plan, asks, then sets everything up
```

`install` fetches pinned runtimes and model files (each checked against a recorded SHA-256), builds the agent, writes a login service (launchd on macOS, a systemd user unit on Linux), and prints the dashboard URL and where your admin key is. On an M5 Pro with warm caches it took 30 s; the first request to a cold model also loads it (about a minute for the embedding worker).

Add another machine you can reach with `ssh`:

```sh
python3 scripts/granitectl.py node add user@gpu-box.local
```

That scans the machine over SSH, copies this checkout there, installs it with its own plan, opens a supervised SSH link, and enrolls it. Nothing on the node listens on the network. A loopback test of the whole sequence (warm caches) took 35 s, after which the coordinator routed requests to the node.

On macOS, `apps/macos/build-app.sh` builds GraniteBar: a menu-bar icon with the engine's live state and a System Settings-style Preferences window (models, devices, nodes, connection details, start at login). Headless Linux nodes are managed from it through the coordinator.

## What runs where

`granitectl scan` decides from measured hardware; it never guesses from a processor name.

| Device | Runtime | Granite models |
| --- | --- | --- |
| Apple silicon | oMLX 0.7.2 with the official ibm-granite MLX builds when oMLX is installed, else llama.cpp Metal (b11669) | Granite 4.2 3B / 8B |
| NVIDIA GPU (Linux) | llama.cpp CUDA (b11669), vLLM for vision | Granite 4.2 3B / 8B, Switch 4.1 8B, Vision 4.1 4B |
| AMD or Intel iGPU (Linux, Vulkan driver) | llama.cpp Vulkan (b11669) over shared RAM | Granite 4.2 3B / 8B |
| Any CPU (macOS arm64, Linux x86_64) | PyTorch native workers | Embedding 97M multilingual, TTM R3 forecasting, Speech 5.0 TurboCTC |
| AMD XDNA NPU | detected and reported | none yet: no Granite build exists for FastFlowLM/Lemonade |

Measured so far: Granite 4.2 on M5 Pro Metal and on the Ryzen AI 9 HX 470's Radeon 890M (Vulkan); CPU workers on both; on an RTX 4060 Ti, Vision through vLLM and Switch through an operator-owned llama.cpp b11870. The installer's CUDA path for Granite 4.2 uses the same llama.cpp release but has not been measured yet. Each measurement keeps its raw per-request data, p50/p95/p99 and its limits in the reference deployment's evidence set.

## Why granited and not …

| If you want | Use | What granited adds |
| --- | --- | --- |
| Chat with one model on one Mac | oMLX (granited can drive it as its Mac engine), LM Studio, Ollama | Non-chat Granite families with typed APIs; several machines as one endpoint |
| Maximum throughput on a GPU server | vLLM | Mixed fleets (Mac + iGPU box + GPU box) with memory admission across them |
| A single llama.cpp server | llama.cpp | Lifecycle, idle eviction, provenance headers, families llama.cpp does not serve |

granited does not pool memory across machines into one big GPU. Each model runs whole on one device; different devices serve different models at the same time.

## API

| Endpoint | Purpose |
| --- | --- |
| `POST /v1/chat/completions` | OpenAI chat, JSON or SSE; `reasoning_effort` off / low / full maps to Granite 4.2 template controls |
| `POST /v1/embeddings` | OpenAI float embeddings |
| `POST /v1/audio/transcriptions` | Multipart audio, mono, up to 30 s |
| `POST /v1/forecast` | Typed univariate history → horizon, with native quantiles |
| `POST /v1/retrieval/rank` | Rank documents with real embedding vectors |
| `POST /v1/vision/extract` | Image + prompt → text |
| `POST /v1/systemone` | Granite Switch typed decisions |
| `GET /v1/models` | Aliases, immutable revisions, capabilities |
| `GET /admin/models`, `POST /admin/models/{id}/load\|unload\|prefetch` | Lifecycle (admin key) |
| `GET /internal/hosts`, `/internal/metrics`, `/internal/hardware` | Cluster and hardware state (admin key) |

Every response carries a request ID, the model revision, the serving host and backend build, and `server-timing` for admission and upstream time; GGUF responses add the file's SHA-256. Errors are structured (`unknown_model`, `admission_busy`, `insufficient_memory`, `memory_pressure`, …) and busy or unavailable responses include `Retry-After`.

## How memory stays safe

- Each deployment declares a memory budget. A cold load starts only if available RAM (or free VRAM) covers the budget plus an OS reserve (8 GiB, or a quarter of RAM on small machines).
- Cold loads pause while the host is stalling on memory (Linux PSI or macOS memory pressure above 5%); warm models keep serving.
- Idle models are evicted after their timeout; models with requests in flight are never evicted.
- Background requests can never take the last slot of a worker.

## Security

Agents bind loopback only; nodes are reached through SSH tunnels. Keys live in `state/secrets/keys.json` (0600). Read [SECURITY.md](SECURITY.md) for the model and its known gaps (no TLS; the coordinator holds node admin keys).

## Development

```sh
cargo test --locked && python3 scripts/smoke_stub.py
```

`workers/stub.py` stands in for a real worker, so control-plane work needs no models. See [CONTRIBUTING.md](CONTRIBUTING.md).

## Status

P0/P1 (the families above) are implemented and measured. Guardian, Docling, reranking, geospatial and other TSFM architectures are declared but return structured `unsupported` errors until each has a validated adapter. No accuracy scores are claimed; quality suites and saturation studies are open work. Model cards for every pinned revision are under `reports/model-cards/`.

## License

Apache-2.0. Granite models are released by IBM under their own licenses on Hugging Face.
