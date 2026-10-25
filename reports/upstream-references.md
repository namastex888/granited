# Upstream sources consulted

Earlier revisions of this repository carried verbatim copies of upstream files for review. They are
removed because redistributing them needs their own license notices; the pinned upstream locations
below are the reference instead. Git history still holds the copies for audit.

| Project | License | Pinned location | Used for |
| --- | --- | --- | --- |
| oMLX | see upstream | [jundot/omlx@78fa08b](https://github.com/jundot/omlx/tree/78fa08bc1aef7290c93c83bb70811547763ebf33) (`omlx/engine_pool.py`, `omlx/server.py`, `omlx/admin/routes.py`) | [oMLX integration decision](omlx-integration.md) |
| llama.cpp server | MIT | [ggml-org/llama.cpp@74bca3950 (b11870)](https://github.com/ggml-org/llama.cpp/tree/74bca3950/tools/server) (`server-context.cpp`, `server-decision.cpp`, `server-task.cpp`, `server-task.h`) | Switch decision and `cache_prompt` behaviour |
| granite-tsfm | Apache-2.0 | [ibm-granite/granite-tsfm v0.3.11](https://github.com/ibm-granite/granite-tsfm/tree/v0.3.11) (`tinytimemixer/modeling_tinytimemixer.py`, `toolkit/get_model.py`) | TTM R3 decomposed architecture and quantiles |
