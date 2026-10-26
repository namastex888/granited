# LocalFleet benchmarks

Measured on real hardware on 2026-10-25. Every number links to its data in [`results/`](results/), and `python -m granite_bench.runner` reproduces it (see [README](README.md)). These are speed and memory measurements; model accuracy is IBM's to state, and LocalFleet does not change it.

Machines: MacBook Pro M5 Pro (48 GB), Ryzen AI 9 HX 470 mini PC (96 GB, Radeon 890M), Ryzen 9 5950X with an RTX 4060 Ti 16 GB.

## LocalFleet adds almost nothing; LiteLLM adds more

Median (p50) latency of the same request three ways on the Mac. In the LiteLLM row, LiteLLM 1.106.4 (Python proxy) forwards to LocalFleet, so the difference between those two columns is LiteLLM's own cost. 30–90 requests each, all successful.

| Request | Engine directly | Through LocalFleet | Through LiteLLM | LiteLLM's extra cost |
| --- | ---: | ---: | ---: | ---: |
| Chat, Granite 4.2 3B | 121.0 ms | 118.2 ms | 127.0 ms | +8.8 ms |
| Chat, Granite 4.2 8B | 262.4 ms | 268.5 ms | 284.1 ms | +15.6 ms |
| Forecast, TTM R3 | 8.2 ms | 9.5 ms | 14.2 ms | +4.7 ms |
| Vision 4.1 4B (on the GPU PC) | 190.4 ms | 190.4 ms | 191.5 ms | +1.1 ms |

LocalFleet's overhead was at most 6 ms on these requests. That covers authentication, placement across devices, memory admission and provenance headers. Data: [`results/gateway-overhead/`](results/gateway-overhead/).

**Known gap:** on the HX mini PC, very small CPU requests took longer through LocalFleet than direct: embeddings 18 → 58 ms, TTM 8 → 49 ms, while chat matched direct (71 → 69 ms). The same TTM request costs 1.3 ms extra on the Mac. The HX runs a build from before the fix that removed request-path stalls in admission and in the Python workers (Nagle's algorithm left on, which costs about 40 ms per reply on Linux), so these rows will be re-measured when that node is updated. LiteLLM's HX rows also crossed the LAN, so they are not a fair comparison and are not shown above.

## Throughput grows with parallel requests

Granite 4.2 on the Mac's GPU. Total output tokens per second across all requests in flight (256-token answers, 16 per setting). Data: [`results/metal-concurrency/`](results/metal-concurrency/).

| Engine and model | 1 at a time | 2 | 4 | Time per answer at 4 |
| --- | ---: | ---: | ---: | ---: |
| oMLX, 3B | 97 tok/s | 175 | **281** | 3.6 s |
| llama.cpp, 3B | 78 tok/s | 141 | 238 | 4.3 s |
| llama.cpp, 8B | — | 71 | 118 | 8.6 s |

First token: 23 ms (llama.cpp) and 65 ms (oMLX) for a short prompt. A 4,446-token prompt starts answering in 1.6 s on oMLX and 2.5 s on llama.cpp ([oMLX report](../reports/omlx-integration.md)).

LocalFleet runs 4 chat slots per model on Macs with 32 GB or more, and 2 elsewhere until each host is measured.

## Memory never overcommits

Every model has a memory budget taken from its measured peak plus 15%. A model loads only when the device has room for that budget plus a reserve for the operating system. Measured peaks (`phys_footprint`, which counts GPU memory) are in [`results/metal-concurrency/footprint.txt`](results/metal-concurrency/footprint.txt):

| Model | Peak at its slot count | Budget |
| --- | ---: | ---: |
| 3B, llama.cpp, 4 slots | 5.3 GB | 5.75 GiB |
| 8B, llama.cpp, 4 slots | 11.3 GB | 12 GiB |
| 3B, oMLX, 2 long prompts | 5.1 GB | 5.5 GiB |
| 8B, oMLX, 2 long prompts | 10.2 GB | 11 GiB |

## CPU models: one slot is right

`granite-embedding-97m` on the Mac CPU, 16 texts per request: about 4.5 requests per second at 1, 2, 4 or 8 parallel slots, so extra slots only add waiting. Data: [`results/cpu-concurrency/`](results/cpu-concurrency/).
