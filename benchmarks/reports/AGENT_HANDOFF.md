# Engineering handoff — IBM Granite inference system (Rust, behind LiteLLM)

**Date:** 2026-10-25. **Priority:** design for a three-host lab (Apple M5 Pro 48 GB unified; NVIDIA RTX 4060, VRAM capacity must be detected; Ryzen AI HX 470 96 GB RAM). This document is a proposed engineering specification, **not evidence of completed inference integrations**.

## Decision

Implement the **Rust control plane** as the durable system and use native accelerator-specific inference workers. Do not attempt to reimplement all Granite architectures in Rust or to make LiteLLM handle arbitrary tensor input. The system sits **behind LiteLLM**: LiteLLM provides client authentication / OpenAI routing; Rust handles model registry, admission control, lifecycle, placement, health, and forwarding to backends. For non-chat endpoints, either route through LiteLLM's documented passthrough feature (test version/route support) or publish direct versioned endpoints on the Rust service.

Architecture:

```
Client -> LiteLLM -> Granite Rust Router/Registry -> CUDA vLLM (Linux + RTX 4060)
                                    |            -> macOS native llama.cpp/MLX workers (M5 Pro)
                                    |            -> Ryzen PyTorch/ONNX CPU workers (TSFM, retrieval, geospatial)
                                    |            -> Docling/ASR specialist workers
                                    +--> metadata store / HF cache / metrics / benchmark runner
```

### Rust crates (choose and lock tested versions before shipping)
- HTTP: axum + tokio; HTTP client reqwest with SSE streaming support.
- serde + schemars for API contracts; tracing + OpenTelemetry for request IDs and stage telemetry.
- sysinfo for generic capacity info, plus NVML/Metal/ROCm-specific probes where available.
- sqlx/sqlite or embedded metadata store for registry and run artifacts.
- Use cargo lock **committed** to repo and CI across macOS/Linux. No crate versions are claimed to be verified as of this report; resolve and freeze at implementation time.

### Service contracts
- `GET /healthz`, `GET /readyz`; `GET /v1/models` and `GET /admin/models` including revision, capabilities, formats, license, backend compatibility, memory estimates, loading state, host.
- `POST /admin/models/{id}/load`, `/unload`, `/prefetch` should be authenticated and asynchronous operation-ID resources; callers poll status. Cancellation, deduplication, timeouts, failed-load cooldown, and idle TTL required.
- Forward OpenAI chat/embeddings/audio where backend supports the API. Preserve streaming, `usage` and raw reasoning boundaries, provide stable `model` alias. Be explicit about thinking, low-effort, thinking-disabled chat templates.
- Define nonstandard typed APIs: `POST /v1/forecast`, `/v1/anomalies`, `/v1/rerank`, `/v1/retrieval/rank` (evaluation only), `/v1/documents/parse`, `/v1/geospatial/predict`, `/v1/guardian/evaluate`. Document units, shape, dtype, revision and input size limits; separate large files into object storage references.
- Every response includes `x-granite-request-id`, `x-granite-model-revision`, `x-granite-backend`, and `Server-Timing` stage timings; return structured JSON errors for OOM, unsupported capability, cold-start, invalid schema.
- Admission control by available VRAM/RAM; never auto-load 30B on small GPU. Reserve runtime overhead, vision memory, KV cache and OS headroom. Per-host GPU exclusivity or configurable fractions; cross-host routing; no assumptions that AMD NPU works without benchmark and runtime support.

## Official checkpoint candidates and caveats

| Family | Official model / card | Backend experiment |
|---|---|---|
| Language | https://huggingface.co/ibm-granite/granite-4.2-3b and `granite-4.2-8b` (confirm exact revisions) | Metal llama.cpp/MLX; CUDA vLLM if compatible |
| Switch | https://huggingface.co/ibm-granite/granite-switch-4.1-8b | transformers >=5.16 <=5.17; torch >=2.11; vLLM >=0.26,<0.28 per card; adapter selection via template; test separately |
| Vision | https://huggingface.co/ibm-granite/granite-vision-4.1-4b | vLLM >=0.21 per official card, commands `<chart2csv>`, `<tables_json>` etc. |
| Speech | https://huggingface.co/ibm-granite/granite-speech-5.0-470m-turboctc | transformers >=5.16; Oct 21 update adds en/fr/de/es/pt/it/ja; MLX-Audio needs separate reproducibility lock |
| Speech 2B | https://huggingface.co/ibm-granite/granite-speech-5.0-2b | test official vLLM implementation |
| Time series | https://huggingface.co/collections/ibm-granite/granite-time-series | TTM R3, PatchTST FM R2, FlowState `r1.1` revision; CPU-first PyTorch TSFM. TSPulse is anomaly/classification/imputation, **not** forecast baseline. Ensemble needs independent component weights |
| Embedding | https://huggingface.co/ibm-granite/granite-embedding-311m-multilingual-r2 | SentenceTransformers torch/onnx, vLLM embed, ONNX on Ryzen; 97M size variant |
| Rerank | https://huggingface.co/ibm-granite/granite-embedding-reranker-english-r2 | cross-encoder worker; measure distinct rerank quality/latency |
| Documents | https://huggingface.co/ibm-granite/granite-docling-258M | use Docling pipeline, not generic causal serving |
| Guardian | https://huggingface.co/ibm-granite/granite-guardian-4.1-8b | **prescribed Guardian scoring**; output `<score>`; BYOC + think/non-think variants; English validated |
| Geospatial | https://huggingface.co/ibm-granite | official org contains task-specific checkpoints; **no single generic geospatial model ID is validated in this handoff**. Agent must inventory task-specific geospatial cards, licenses, input bands, spectral normalization and output heads before integration. |

**Dependency-pin rule:** every deployment MUST specify immutable HF `revision=<40 hex SHA>` from the Hub, immutable OCI image digest, compiler/toolchain version, and lockfile. Version ranges cited above are upstream compatibility **constraints**, not frozen deployments. Do not manufacture SHAs, Rust crate pins, or a cross-platform `torch` hash. Generate `locks/macos-arm64`, `locks/linux-x86_64-cuda`, `locks/linux-x86_64-cpu` after actually resolving each environment. For models requiring untrusted remote code, disable by default and review source and revision before enabling.

## Host allocation

1. **M5 Pro** — daily quantized Granite 4.2 3B/8B and small MLX speech on demand. macOS Metal, no CUDA Docker stack. Keep a warm text model and one cold model slot. Set observed memory high-water limits.
2. **RTX 4060** — CUDA-specific vLLM experiments, one model at a time unless measured fit. Detect VRAM, compute capability, driver and CUDA library before choosing image. Vision 4B may require quantization/offload or may not fit; show `unavailable` if infeasible, not silent swap to another model.
3. **Ryzen HX 470 96 GB** — CPU TSFM, embedding/rerank via ONNX, Docling workers and raster processing. NPU/ROCm/Vulkan acceleration is optional and must be measured against CPU.

Use per-host content-addressed HF storage; shared NFS only if throughput/locking tests show benefit. Do not put HF cache under `~` in container mounts, rely on absolute paths.

## Benchmark acceptance contract

Repository `granite_bench` is an **initial executable HTTP protocol/scoring harness** (stdlib-only), not a completed end-to-end family benchmark. Smoke fixtures are synthetic. Extend it to run real public datasets with fixed immutable dataset revisions and exact preprocessing. Every experiment must run **direct worker** and **through LiteLLM** and **through Rust router** where feasible to quantify overhead. Compare cold-start and prewarmed, batch size 1 and throughput saturation, response sizes, streaming and concurrency 1/2/4/8, at least 30 repeats (more when unstable), confidence intervals and p50/p95/p99 latencies. Export raw per-request JSONL/Parquet, machine info, power and memory telemetry, aggregate summaries, plots, exact reproducibility manifest.

| Family | Required pass/fail measurements |
|---|---|
| Language | TTFT from request dispatch, inter-token latency, decode tokens/s, exact/task accuracy, actual reasoning tokens, unwanted reasoning in `off`, malformed `<think>` rate; use fixed tasks and seeds |
| Vision | table structure F1 (cell coordinates, spans, content), chart value/series extraction F1; per-image latency, peak allocated/resident memory, input resolution |
| Speech | WER/CER by language and audio condition, RTF=`processing_time/audio_duration`, quality vs chunk size and real-time streaming |
| Time series | rolling origin MASE, WQL, quantile empirical coverage and calibration error, model warm/cold latency; no leakage; horizon/frequency/context stratification |
| Embedding/rerank | nDCG@10, recall@10, MRR, QPS at given concurrency, long-input truncation; multilingual and code search, with fixed corpora |
| Documents | table TEDS or structural matching, reading order edit distance, page/sec, end-to-end PDF processing cost; distinguish PDF pipeline from model runtime |
| Guardian | precision, recall, FPR, FNR, F1, threshold curves, calibration, BYOC/hallucination/tool-call subcategories, think/nonthink profiles |
| Geospatial | benchmark each task separately: IoU/mIoU for semantic segmentation, RMSE/MAE for biomass/height/regression; raster megapixels/s, VRAM, georeferencing consistency |

### Important metric caveats in initial client
- Initial implementation computes only **token-level F1** for vision/documents, not structural table matching.
- Its SSE TTFT is measured from request dispatch to the first content-bearing SSE delta; validate networking overhead and streamed reasoning/content semantics before publication.
- Serial QPS shown is not saturation QPS. Current output tokens/s uses total latency rather than decode interval.
- No external data downloads, GPU profiling, production datasets, runtime backend integration, or real machine validation have been performed.
- Report null/unsupported metrics explicitly; never fill fabricated benchmark values.

## Agent implementation order / acceptance milestones

**P0 (protocol and observability):** Rust router skeleton, registry + typed capabilities, health checks, OpenAI-compatible chat streaming, stable error envelope, stage telemetry, 3 host agents, revisioned model IDs, memory-based admission. Run smoke fixture under direct/through-Rust/through-LiteLLM paths. All fixtures must return shaped responses; no claims of model quality.

**P1 (local priority paths):** M5 Pro Granite 4.2 3B/8B llama.cpp/MLX, GPU Vision vLLM, multilingual TurboCTC with Portuguese, Ryzen TTM and embedding 97M; inspect actual hardware capacity and build immutable locks. Produce first measured baselines.

**P2 (family breadth):** Guardian prescribed scoring, Docling structured output, reranker, FlowState/PatchTST, TSPulse, additional vision chart/table prompts, Switch 4.1 adapters, 2B speech. Geospatial only once task-specific cards and preprocessing have been verified.

**P3 (benchmark science):** real datasets and scored quality for all eight groups, proper streaming metrics, cold/warm, concurrent saturation, energy/memory and reproducible per-host results; compare backend decisions and implement eviction heuristics based on evidence.

**Definition of done:** (a) pinned manifests for all active workers; (b) accurately advertised model capabilities and failures; (c) working cross-host dispatch; (d) no silently altered model identity; (e) reproducible real-dataset results for each claimed family; (f) explicit throughput/latency/quality tradeoffs. Avoid claiming all checkpoints have been run when only protocol mocks pass.
