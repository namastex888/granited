# Granite HTTP benchmark harness

The original engineering handoff and smoke fixtures are preserved under `reports/AGENT_HANDOFF.md` and `fixtures/smoke.jsonl`. The runner now supports direct native workers, the Rust router, and LiteLLM; raw requests are saved beside each summary as `*.requests.jsonl`.

Run from the repository root:

```sh
PYTHONPATH=benchmarks python3 -m unittest discover -s benchmarks/tests -v
PYTHONPATH=benchmarks python3 -m granite_bench.runner \
  --base-url http://127.0.0.1:8310 --fixture benchmarks/fixtures/ttm-functional.jsonl \
  --stage rust --kind smoke --repeat 30 --warmup 1 --output reports/ttm-rust.json
```

`BENCH_API_KEY` supplies the bearer token. `--stage` is `direct`, `rust`, or `litellm`. For typed LiteLLM endpoints add `--typed-prefix /granite`; chat and embeddings retain their OpenAI paths. `--concurrency` defaults to 1. Reported throughput is achieved throughput at that concurrency, not saturation throughput. SSE first-token latency includes reasoning; first-answer latency is separate. Raw SSE objects preserve reasoning boundaries, native timings and token usage. No token count is estimated from characters.

Smoke fixtures, including real inference on synthetic inputs, produce no accuracy scores. `--kind dataset` requires `--dataset-manifest` containing `repo`, a resolved 40-character `revision`, `split`, and `preprocessing_sha256`. Metrics remain partial: vision/documents leave structural F1 explicitly null until a structural evaluator exists. The task-specific dataset and preprocessing must justify every published quality claim.

The original smoke fixture contains a five-point forecast example that the pinned TTM R3 default checkpoint does not support, plus Guardian without a P1 worker. Those return structured failures. Use `ttm-functional.jsonl` for the real checkpoint's 512-point input contract. Unknown/unsupported metrics remain absent or null.
