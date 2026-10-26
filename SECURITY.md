# Security policy

## Reporting

Report vulnerabilities privately through GitHub's "Report a vulnerability" on this repository.
Please do not open public issues for security problems. Expect an acknowledgement within 5 working days.

## Model

- Every agent binds `127.0.0.1`. Nodes reach each other through SSH tunnels that `localfleet node add` supervises; no node port is exposed on the network by default.
- Two bearer keys per node (`GRANITE_API_KEY` for inference, `GRANITE_ADMIN_KEY` for lifecycle and telemetry) are generated into `state/secrets/keys.json` with mode 0600 and never written to logs, argv or Git.
- A coordinator holds each node's admin key to place work on it. That key grants full control of that node; treat the coordinator as a trusted machine. A scoped peer role is planned.
- Downloads are pinned: llama.cpp archives and GGUF files are checked against SHA-256 values in `locks/` and `reports/gguf-*-files.json`; Python environments install with `--require-hashes`; model weights use immutable Hugging Face revisions; container images use digests.
- `localfleet` reports engines already running on a machine (Ollama, Lemonade, oMLX, vLLM, llama.cpp) and never adopts or stops them.

## Known gaps

- `model: "auto"` never evicts a loaded model on the node that resolves it, and it asks a peer to serve only a model that is already loaded (`x-granite-load: warm-only`). Nodes on builds without the `x-granite-load` feature ignore that header: in the 2 s between peer snapshots, such a node may load, or evict to load, for an `auto` request. Any API-key holder may send `x-granite-load: warm-only` on an explicit request; it only forbids loading.
- Conversation ids (`x-granite-conversation`) share one namespace per node, because a fleet has one API key. Any key holder who guesses an id can reuse it, learn which model served it, and steer that conversation's next pick within the preferred list. The store is bounded (10,000 ids, 30 minutes idle), and a sticky pick can never cause a load that evicts.
- Separate thinking budgets need the last hop to run a build with `thinking_budget_tokens`. An older last hop passes the field to llama.cpp but does not add it to `max_tokens`, so on that hop thinking can still use part of the reply budget.
- Message `content` must be a string, an array of parts or null, and chat content parts are allowlisted by type on every chat request: `text`, `image_url`, `input_audio` and `refusal`. Any other type, such as `video_url`, `audio_url`, `input_image` or `file`, and any part without a type, gets 400 `invalid_schema`. `image_url` must be an inline `data:` URI (png, jpeg/jpg or webp, standard base64, 8 MiB decoded), so LocalFleet never forwards a URL from a content part to an engine. This holds only on nodes running this build: an older node in a mixed fleet still forwards whatever it receives. Under `auto`, `input_audio` (wav, 16 MiB) only ever reaches speech to text on the entry node, never a voice-reference slot, and its transcript is not logged.
- Orphan reaping: each worker the manager spawns is recorded in `state_dir/owned/<model>.pid` (PID and start time). A manager killed outright leaves its workers running, so at startup, holding that state directory's exclusive lock, the next manager stops exactly the process groups whose PID and start time still match. An entry whose start time differs (a reused PID) is deleted without a signal. Processes the manager did not spawn, such as containers started by hand, external engines, or another manager's workers, never appear in the ledger and are never signalled. A foreign process on a worker port is still refused (`worker_port_in_use`).
- `GET /v1/devices` is a read surface for API-key holders. For each host in the fleet it lists the deployed models' states (`warm`, `cold_fits`, `no_room`, `held`), each model's memory budget and last load time, free memory, and the accelerator's vendor and name. It never lists hostnames or addresses. The admin view `/internal/hosts` already shows the same facts to the owner.
- `model: "auto"` on `/v1/audio/speech` (profile flag `auto.speech`, off by default) runs only on the node that receives the request. Voice references are personal audio, so they are never forwarded to a peer, even a warm one; the profile refuses to start if a `prefer.speech_synthesis` model is not deployed locally.
- The Qwen3-TTS worker (`workers/tts_qwen.py`) decodes reference audio itself and passes the model a waveform, never a string: `qwen-tts` reads a string as a file path or a URL. References stay in memory for one request; they are never written, cached or logged.
- The `x-granite-route` header tells API-key holders which model served and, when it differs, the preferred one. So it shows whether the preferred model was serving at that moment. It never lists other candidates or their state.

- No TLS: keep traffic on loopback and SSH. Do not bind an agent to a LAN address without a TLS terminator in front of it.
- Spawned workers inherit the agent's environment, including keys.
- The rustup bootstrap used when a node has no Rust toolchain fetches `sh.rustup.rs` over HTTPS without a pinned digest; prebuilt release binaries will replace it.
