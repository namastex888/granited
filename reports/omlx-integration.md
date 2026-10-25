# oMLX integration decision

Use oMLX as a future Mac inference worker, retaining the Rust coordinator for cluster placement. No fork or kernel rewrite is justified by current evidence. oMLX has not been installed or performance-tested in this deployment; Granite 4.2 MLX conversion compatibility remains unvalidated.

Source inspected at immutable commit `78fa08bc1aef7290c93c83bb70811547763ebf33`:
- [Native engine pool](https://github.com/jundot/omlx/blob/78fa08bc1aef7290c93c83bb70811547763ebf33/omlx/engine_pool.py): loading, unloading, model pinning and memory management already belong to the native runtime.
- [Admin routes](https://github.com/jundot/omlx/blob/78fa08bc1aef7290c93c83bb70811547763ebf33/omlx/admin/routes.py): `/admin/api/models`, model `/load`, `/unload` and `/settings`. Loading accepts admin or bearer auth; unloading uses `require_admin`. An adapter must respect that authentication difference and the asynchronous unload response.
- [Server](https://github.com/jundot/omlx/blob/78fa08bc1aef7290c93c83bb70811547763ebf33/omlx/server.py): OpenAI-compatible inference and separate admin router.

The next bounded experiment is one pinned compatible Granite conversion, native oMLX versus the validated llama.cpp Metal baseline, checking template controls, reasoning deltas, schema constraints, cold load, resident memory and prompt-cache behavior. Preserve oMLX’s model-manager experience by delegating its native lifecycle. Keep cluster hardware, admission, aliases, provenance and remote workers in Rust. Expose one LiteLLM client endpoint for both engines.

A working distributed service already uses the same Rust binary on each node, separate native workers by architecture, private node links, explicit model allowlists, and live memory/slot admission. Devices serve different models concurrently; no claim is made that their memory forms one tensor-parallel GPU.

## Gate results, 2026-10-25

Static checks, no model loaded (the Mac Metal slot is held for another workload):

| Check | Result |
| --- | --- |
| Granite 4.2 MLX conversion | **Official** IBM conversions exist: [`ibm-granite/granite-4.2-8b-q4-mlx@9047f07`](https://huggingface.co/ibm-granite/granite-4.2-8b-q4-mlx/tree/9047f073e46c527f70dade07bb55b1700d728e36) and [`ibm-granite/granite-4.2-3b-q4-mlx@0c6f39b`](https://huggingface.co/ibm-granite/granite-4.2-3b-q4-mlx/tree/0c6f39b1827afd5eb2c1c3b13751929857434953), Apache-2.0; q8, bf16 and 30B variants also published |
| oMLX license and upkeep | Apache-2.0; pushed 2026-10-25, about 23k stars, not archived |
| Installed here | oMLX 0.7.2 at `~/.omlx/bin/omlx`, not running |
| Default bind and auth | `serve` binds 127.0.0.1:8000; `--api-key` is required for non-loopback binds. The key is an argv flag, so it is visible in `ps`; the adapter should prefer a loopback bind or an environment/config key |
| Clustering | oMLX has its own `cluster` mode for Macs (text generation only); granited keeps cross-device placement |

Still open, needs the Metal slot: chat template and `reasoning_effort` behaviour, JSON-schema constraints, cold load, resident memory and prompt cache, measured against the llama.cpp b11669 Metal baseline with the same prompts. The adapter (external worker with delegated lifecycle, reading oMLX `/admin/api/models` for state) is written only after that run passes; until then the Mac serves Granite through llama.cpp Metal and the menu-bar app plus `/ui` are the Mac UI.

## Runtime comparison, 2026-10-25 (owner-run, M5 Pro)

Granite 4.2 3B, same prompts and parameters (temperature 0), one engine at a time: llama.cpp b11669 Metal with `granite-4.2-3b-Q4_K_M.gguf` versus oMLX 0.7.2 with the official `ibm-granite/granite-4.2-3b-q4-mlx@0c6f39b`. Harness: `engine_bench.py`. Warm figures are 10 requests at concurrency 1 (about 40 output tokens each), so treat them as indicative.

| Check | llama.cpp Metal | oMLX | Note |
| --- | ---: | ---: | --- |
| `enable_thinking: false` reasoning chars | 0 | 0 | both honour the template control |
| `enable_thinking: true` reasoning chars | 394 | 459 | both stream reasoning in a separate field |
| `low_effort: true` reasoning chars | 30 | 40 | both shorten reasoning |
| strict JSON schema | valid | valid | identical answer |
| server start → first token | 1.7 s | 4.5 s | cold load |
| warm first token, short prompt (p50) | 23 ms | 65 ms | llama.cpp faster |
| warm decode (p50) | 47.9 tok/s | 97.4 tok/s | oMLX about 2× |
| 4,446-token prompt, first token | 2,470 ms | 1,619 ms | oMLX prefill faster |
| same prompt repeated | 30 ms | 497 ms | llama.cpp prompt cache much stronger |
| resident memory | 2,922 MiB | 2,388 MiB | oMLX smaller |

Decision: oMLX passes the correctness gate and is the better Mac engine for chat and long generations (decode and prefill dominate). llama.cpp stays the default for agent-style workloads that resend one long prefix. The adapter landed the same day: backend `omlx`, one oMLX process per alias on a private loopback port with its own `--base-path`, so it never shares state with an oMLX app the user runs. granited owns spawn, preload, idle unload and admission as for llama.cpp.
