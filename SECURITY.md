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

- No TLS: keep traffic on loopback and SSH. Do not bind an agent to a LAN address without a TLS terminator in front of it.
- Spawned workers inherit the agent's environment, including keys.
- The rustup bootstrap used when a node has no Rust toolchain fetches `sh.rustup.rs` over HTTPS without a pinned digest; prebuilt release binaries will replace it.
