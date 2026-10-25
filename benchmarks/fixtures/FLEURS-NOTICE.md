# FLEURS sample attribution

`fleurs-ptbr.wav` and `speech-fleurs.jsonl` contain test sample 1853 from Google FLEURS, Portuguese (Brazil), revision `70bb2e84b976b7e960aa89f1c648e09c59f894dd`, `pt_br/test`.

Source: [google/fleurs](https://huggingface.co/datasets/google/fleurs/tree/70bb2e84b976b7e960aa89f1c648e09c59f894dd). License: [Creative Commons Attribution 4.0](https://creativecommons.org/licenses/by/4.0/). Attribution: Google FLEURS contributors.

The preprocessing selects the first 3–12 second mono 16 kHz file from the pinned test parquet, decodes it to float PCM and writes a PCM16 WAV and typed HTTP fixture. No resampling, transcription alteration or model-generated audio is performed. The original dataset is not bundled. `reports/fleurs-sample.json` records the exact parquet and preprocessing hashes. Repeated inference on this one file measures latency, not aggregate Portuguese WER.
