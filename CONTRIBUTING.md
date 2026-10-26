# Contributing

Thanks for helping. The bar for a change is that it is measured or tested, and that it never claims more than its evidence shows.

## Set up

```sh
cargo build --locked
python3 scripts/localfleet.py scan          # what this machine can run, changes nothing
```

For control-plane work you do not need models: `workers/stub.py` is a deterministic stand-in worker.

```sh
GRANITE_API_KEY=dev-api GRANITE_ADMIN_KEY=dev-admin ./target/debug/granited serve configs/examples/stub.json
curl -H 'Authorization: Bearer dev-admin' http://127.0.0.1:8410/internal/hosts
```

## Checks (CI runs the same)

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
PYTHONPATH=benchmarks python3 -m unittest discover -s benchmarks/tests
python3 scripts/smoke_stub.py               # router + stub worker end to end
```

## Rules of the road

- Pin what you add: crate versions in `Cargo.toml`, hashed Python locks in `locks/`, immutable model revisions, image digests.
- A new model family ships with a native worker, a typed contract in `src/contracts.rs`, a fixture under `benchmarks/fixtures/`, and recorded measurements. Unsupported paths return structured errors; there is no silent fallback to another model.
- Performance claims cite the command and raw output that produced them.
- Commits are signed off (`git commit -s`), certifying the [DCO](https://developercertificate.org/).
