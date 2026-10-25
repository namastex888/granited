#!/usr/bin/env python3
"""Select one short, real Portuguese test file for repeatable speech functionality checks."""
import hashlib
import io
import json
from pathlib import Path

from huggingface_hub import hf_hub_download
import pyarrow.parquet as pq
import soundfile as sf

ROOT = Path(__file__).resolve().parents[1]
REVISION = '70bb2e84b976b7e960aa89f1c648e09c59f894dd'


def main():
    path = Path(hf_hub_download('google/fleurs', 'parquet-data/pt_br/test-00000-of-00001.parquet',
                              repo_type='dataset', revision=REVISION))
    for row in pq.read_table(path).to_pylist():
        samples, rate = sf.read(io.BytesIO(row['audio']['bytes']), dtype='float32')
        if rate != 16000 or samples.ndim != 1 or not 3 <= len(samples) / rate <= 12:
            continue
        out = ROOT / 'benchmarks/fixtures'
        out.mkdir(parents=True, exist_ok=True)
        sf.write(out / 'fleurs-ptbr.wav', samples, rate, subtype='PCM_16')
        record = dict(id='fleurs-ptbr-'+str(row['id']), family='speech', path='v1/speech/transcribe', synthetic=False,
                      request=dict(model='granite-speech-5.0-470m-turboctc', samples=samples.tolist(), sample_rate=rate, language='pt'),
                      expected=dict(transcript=row['transcription'], language='pt'))
        (out / 'speech-fleurs.jsonl').write_text(json.dumps(record, ensure_ascii=False)+'\n')
        manifest = dict(repo='google/fleurs', revision=REVISION, split='pt_br/test', sample_id=row['id'],
                        reference=row['transcription'], seconds=len(samples)/rate, sample_rate=rate, license='cc-by-4.0',
                        parquet_sha256=hashlib.file_digest(open(path, 'rb'), 'sha256').hexdigest(),
                        preprocessing_sha256=hashlib.file_digest(open(__file__, 'rb'), 'sha256').hexdigest(),
                        scope='one real file, repeated for latency; no aggregate quality claim')
        (ROOT / 'reports/fleurs-sample.json').write_text(json.dumps(manifest, indent=2, ensure_ascii=False)+'\n')
        print(json.dumps(manifest, ensure_ascii=False))
        return
    raise SystemExit('No matching mono 16 kHz file found')


if __name__ == '__main__':
    main()
