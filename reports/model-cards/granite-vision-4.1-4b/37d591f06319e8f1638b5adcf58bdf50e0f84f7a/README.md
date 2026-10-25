---
language:
- en
license: apache-2.0
pipeline_tag: image-text-to-text
library_name: transformers
---

# Granite-Vision-4.1-4B

**Model Summary:**
Granite Vision 4.1 4B is a vision-language model (VLM) that delivers frontier-level
performance on structured document extraction tasks — chart extraction, table extraction,
and semantic key-value pair extraction — in a compact 4B parameter footprint, providing
a lightweight alternative to much larger frontier models for these tasks:

- **Chart extraction:** Converting charts into structured, machine-readable formats (Chart2CSV, Chart2Summary, and Chart2Code)
- **Table extraction:** Accurately extracting tables with complex layouts from document images to JSON, HTML, or OTSL
- **Semantic Key-Value Pair (KVP) extraction:** Extracting values based on key names and descriptions across diverse document layouts

The model is finetuned on top of [Granite-4.1-3B](https://huggingface.co/ibm-granite/granite-4.1-3b), with a 3.4B LLM and 0.6B Vision Encoder and Projectors. See
[Model Architecture](#model-architecture) for details.

The methodology and data (ChartNet) used for this model are described in the paper [ChartNet: A Million-Scale, High-Quality Multimodal Dataset for Robust Chart Understanding](https://huggingface.co/papers/2603.27064).

While our focus is on specialized document extraction tasks, the current model preserves and extends the 
capabilities of Granite Vision 4.0 3B, ensuring that existing users can adopt it seamlessly with no changes to their workflow. 
It continues to support vision‑language tasks such as producing detailed natural‑language descriptions from images (image‑to‑text).
The model can be used standalone and integrates seamlessly
with [Docling](https://github.com/docling-project/docling) to enhance document processing pipelines
with deep visual understanding capabilities.

- **Developer:** IBM Research
- **GitHub Repository:** https://github.com/ibm-granite
- **Release Date:** April 29th, 2026
- **License:** [Apache 2.0](https://www.apache.org/licenses/LICENSE-2.0)

## Supported Tasks

The model supports specialized extraction tasks, each activated by a simple task tag in
the user message. The chat template automatically expands tags into the full prompt — no
need to write verbose instructions.

| Tag | Task | Output |
|-----|------|--------|
| `<chart2csv>` | Chart to CSV | CSV table with headers and numeric values |
| `<chart2code>` | Chart to Python code | Python code that recreates the chart |
| `<chart2summary>` | Chart to summary | Natural-language description of the chart |
| `<tables_json>` | Table extraction (JSON) | Structured JSON with dimensions and cells |
| `<tables_html>` | Table extraction (HTML) | HTML `<table>` markup |
| `<tables_otsl>` | Table extraction (OTSL) | OTSL markup with cell/merge tags |
| KVP (see prompt instructions below) | Schema based Key-Value pairs extraction | JSON with nested dictionaries and arrays |


## Model Performance

### Benchmark Results

Granite Vision 4.1 4B provides a lightweight alternative to frontier models on structured document extraction
benchmarks, delivering comparable performance at a fraction of the parameter count.

<img src="bench_all_benchmarks.png" width="80%">

#### Chart Extraction

We evaluate chart extraction using the human-verified test-set from [ChartNet](https://huggingface.co/datasets/ibm-granite/ChartNet).
Models are scored by LLM-as-a-judge (GPT4o) comparing predictions against ground truth.
We report average scores (0–100) on Chart2CSV and Chart2Summary tasks.

<div style="display: flex; gap: 10px;">
  <img src="bench_c2c.png" width="49%">
  <img src="bench_c2s.png" width="49%">
</div>

#### Table Extraction

To benchmark table extraction, we construct a unified evaluation suite spanning multiple datasets and settings to assess end-to-end table extraction capabilities of vision-language models:

1. **[TableVQA-Extract](https://github.com/naver-ai/tablevqabench)** — Converts the original visual table QA benchmark into a cropped table extraction task.  
2. **[OmniDocBench-tables](https://github.com/opendatalab/OmniDocBench)** — A document parsing benchmark over diverse PDF types with detailed annotations for layout, text, formulas, and tables. We use the subset of pages that contain one or more tables to evaluate table extraction in full-page settings.  
3. **[PubTablesV2](https://huggingface.co/datasets/kensho/PubTables-v2)** — A large-scale table extraction benchmark evaluated in both cropped-table and full-page document settings.

To unify evaluation, we replace each dataset’s original annotations (e.g., Q&A pairs) with a single instruction: *extract the table(s) from the image in HTML format*, using the corresponding HTML as ground truth. For full-page inputs, only tabular elements are considered; when multiple tables appear, they are aggregated into a Python list.

We report results using **TEDS** ([Tree-Edit Distance-based Similarity](https://arxiv.org/abs/2208.00385)), which measures structural and content similarity between predicted and ground-truth HTML tables.

Results are presented separately for cropped-table and full-page settings to highlight performance across controlled and realistic document scenarios.

<div style="display: flex; gap: 10px;">
  <img src="bench_tvqa.png" width="49%">
  <img src="bench_omni.png" width="49%">
</div>
<div style="display: flex; gap: 10px;">
  <img src="bench_pubcrop.png" width="49%">
  <img src="bench_pubfull.png" width="49%">
</div>

#### Key-Value Pair (KVP) Extraction

We evaluate on [VAREX](https://udibarzi.github.io/varex-bench/), a benchmark for
multimodal structured extraction from documents. Granite Vision 4.1 4B achieves
94.2% exact-match accuracy (zero-shot), competitive with much larger frontier models
(view results [here](https://udibarzi.github.io/varex-bench/#leaderboard)).

<div style="display: flex; gap: 10px;">
  <img src="bench_varex.png" width="60%">
</div>

### Setup

Tested with python=3.11
```shell
pip install torch==2.10.0 --index-url https://download.pytorch.org/whl/cu128
pip install transformers>=5.8.0 peft>=0.19.1 tokenizers>=0.22.2 pillow>=12.2.0
```
Requires `transformers>=5.8.0` for native support. On older versions, pass `trust_remote_code=True` to both `from_pretrained` calls to use the bundled legacy modeling code.                                  

## Usage with Transformers


```python
import re
from io import StringIO

import pandas as pd
import torch
from transformers import AutoProcessor, AutoModelForImageTextToText
from PIL import Image
from huggingface_hub import hf_hub_download

model_id = "ibm-granite/granite-vision-4.1-4b"
device = "cuda" if torch.cuda.is_available() else "cpu"

processor = AutoProcessor.from_pretrained(model_id)
processor.tokenizer.padding_side = "left"  # required for correct batched generation
model = AutoModelForImageTextToText.from_pretrained(
    model_id,
    dtype=torch.bfloat16,
    device_map=device
).eval()


def run_inference(model, processor, images, prompts):
    """Run batched inference on image+prompt pairs (one image per prompt)."""
    conversations = [
        [{"role": "user", "content": [
            {"type": "image"},
            {"type": "text", "text": prompt},
        ]}]
        for prompt in prompts
    ]
    texts = [
        processor.apply_chat_template(conv, tokenize=False, add_generation_prompt=True)
        for conv in conversations
    ]
    inputs = processor(
        text=texts, images=images, return_tensors="pt", padding=True, do_pad=True
    ).to(model.device)
    outputs = model.generate(
        **inputs,
        max_new_tokens=4096, 
        use_cache=True
    )
    results = []
    for i in range(len(prompts)):
        gen = outputs[i, inputs["input_ids"].shape[1]:]
        results.append(processor.decode(gen, skip_special_tokens=True))
    return results


def display_table(text):
    """Pretty-print CSV (possibly wrapped in ```csv```) or HTML table content via pandas."""
    m = re.search(r"```csv\s*
(.*?)```", text, re.DOTALL)
    if m:
        df = pd.read_csv(StringIO(m.group(1)))
        print(df.to_string(index=False))
    elif "<table" in text.lower():
        df = pd.read_html(StringIO(text))[0]
        print(df.to_string(index=False))
    else:
        print(text)
```

### Chart and Table Tasks

You can pass tags and the chat template handles the rest:

```python
chart_path = hf_hub_download(repo_id=model_id, filename="chart.jpg")
table_path = hf_hub_download(repo_id=model_id, filename="table.png")
chart_img = Image.open(chart_path).convert("RGB")
table_img = Image.open(table_path).convert("RGB")

# Batched chart tasks
chart_prompts = ["<chart2csv>", "<chart2summary>", "<chart2code>"]
chart_results = run_inference(model, processor, [chart_img] * len(chart_prompts), chart_prompts)
for prompt, result in zip(chart_prompts, chart_results):
    print(f"{prompt}:")
    display_table(result)
    print()

# Batched table tasks
table_prompts = ["<tables_html>", "<tables_otsl>"]
table_results = run_inference(model, processor, [table_img] * len(table_prompts), table_prompts)
for prompt, result in zip(table_prompts, table_results):
    print(f"{prompt}:")
    display_table(result)
    print()
```

### Key-Value Pair Extraction (KVP)

For KVP extraction use the [VAREX](https://github.com/udibarzi/varex-bench) prompt format. Provide a JSON Schema describing the fields to extract and the model will return a JSON object with the extracted values.

```python
import json

invoice_path = hf_hub_download(repo_id=model_id, filename="invoice.png")
invoice_img = Image.open(invoice_path).convert("RGB")
schema = {
    "type": "object",
    "properties": {
        "invoice_date": {"type": "string", "description": "The date the invoice was issued"},
        "order_number": {"type": "string", "description": "The unique identifier for the order"},
        "seller_tax_id": {"type": "string", "description": "The tax identification number of the seller"},
    }
}

prompt = f"""Extract structured data from this document.
Return a JSON object matching this schema:

{json.dumps(schema, indent=2)}

Return null for fields you cannot find.
Return ONLY valid JSON.
Return an instance of the JSON with extracted values, not the schema itself."""

result = run_inference(model, processor, [invoice_img], [prompt])[0]
print(result)
```



## Usage with vLLM

Granite Vision 4.1 is supported natively in vLLM as of v0.21.0:

```bash
pip install vllm>=0.21.0
```

### Serving

```bash
vllm serve ibm-granite/granite-vision-4.1-4b --host 0.0.0.0 --port 8000
```

### Client example

Query the running server using the OpenAI-compatible API:

```python
import base64
from openai import OpenAI
from huggingface_hub import hf_hub_download
from PIL import Image

model_id = "ibm-granite/granite-vision-4.1-4b"
client = OpenAI(base_url="http://localhost:8000/v1", api_key="EMPTY")

def run_inference(client, model_id, image_path, tag):
    with open(image_path, "rb") as f:
        image_b64 = base64.b64encode(f.read()).decode("utf-8")
    messages = [
        {"role": "user", "content": [
            {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{image_b64}"}},
            {"type": "text", "text": tag},
        ]}
    ]
    response = client.chat.completions.create(
        model=model_id, messages=messages, max_tokens=4096, temperature=0,
    )
    return response.choices[0].message.content

chart_path = hf_hub_download(repo_id=model_id, filename="chart.jpg")
table_path = hf_hub_download(repo_id=model_id, filename="table.png")

# Chart tasks
for tag in ["<chart2csv>", "<chart2summary>", "<chart2code>"]:
    result = run_inference(client, model_id, chart_path, tag)
    print(f"{tag}:
{result}
")

# Table tasks
for tag in ["<tables_json>", "<tables_html>", "<tables_otsl>"]:
    result = run_inference(client, model_id, table_path, tag)
    print(f"{tag}:
{result}
")
```

## Usage with MLX VLM

[MLX VLM](https://github.com/Blaizzy/mlx-vlm) runs Granite Vision 4.1 4B natively on Apple Silicon via the [MLX](https://github.com/ml-explore/mlx) framework.

### Setup
Install from source for the latest features:
```bash
pip install git+https://github.com/Blaizzy/mlx-vlm.git
```
### CLI
```bash
mlx_vlm generate \
         --model ibm-granite/granite-vision-4.1-4b \
         --image chart.jpg \
         --prompt "<chart2csv>"
```


## Usage with Docling

[Docling](https://github.com/docling-project/docling) integrates Granite Vision for document conversion pipelines:

- [Table extraction](https://github.com/docling-project/docling/blob/v2.90.0/docs/examples/granite_vision_table_structure.py) — uses Granite Vision to extract the layout and content of detected tables.
- [Chart data extraction](https://github.com/docling-project/docling/blob/v2.90.0/docs/examples/chart_extraction.py) — uses Granite Vision to extract structured data from bar, pie, and line charts (`pip install docling[granite_vision]`)


## Training Data

The model was fine-tuned on a curated mixture of extraction-focused datasets spanning
chart understanding, complex table parsing, and document KVP extraction, supplemented
by the general-purpose [Granite Vision instruction-following dataset](https://arxiv.org/abs/2502.09927) for broad visual understanding.

Chart understanding data was created through a novel code‑guided augmentation methodology that produces diverse, 
semantically aligned chart samples containing rendering code, chart images, underlying data CSVs, and natural‑language summaries. 
Using this pipeline, we are also releasing [ChartNet](https://huggingface.co/datasets/ibm-granite/ChartNet), a comprehensive million‑scale multimodal dataset enriched with real‑world, 
human‑annotated, safety, and grounding subsets. The dataset and its methodology are detailed in the paper [ChartNet: A Million-Scale, High-Quality Multimodal Dataset for Robust Chart Understanding](https://huggingface.co/papers/2603.27064).

## Model Architecture

1. **SigLIP2 Vision encoder:** `google/siglip2-so400m-patch16-384`. Input images are tiled into 384×384 patches (with a base downscaled view always included), and each tile is encoded independently. The vision encoder is finetuned with LoRA adapters during training. The chechpoint provides the weights with merged adapters.
2. **Window Q-Former projectors:** Visual features are compressed 4× using windowed Q-Former projectors: each 4×4 patch window is reduced to 2×2 tokens via cross-attention, where the queries are initialized from a downsampled version of the window features. This reduces the visual token count fed to the LLM.
3. **Feature injection:** A variant of [Deepstack](https://arxiv.org/abs/2406.04334) where visual features are additively injected into the LLM hidden states at multiple layers through two complementary mechanisms:
   - *LayerDeepstack:* Features from 4 vision encoder depths are each projected and injected into a different LLM layer. The Q-Former queries are initialized from downsampled features. The mapping is reversed — the deepest (most semantic) vision features feed the earliest LLM layers, providing strong semantic grounding from the start.
   - *SpatialDeepstack:* The deepest vision features at full resolution are split into 4 complementary spatial groups. Each group's Q-Former queries are initialized from the corresponding spatial subset, and injected at a separate later LLM layer, providing fine-grained spatial detail.

   In total, **8 vision-to-LLM injection points** distribute visual information across the network for stronger visual grounding.
4. **Language model:** Granite-4.1 (3B) with LoRA (rank 256) across all self-attention projections and MLP layers. The chechpoint provides the weights with merged adapters.

**Supported input:** English instructions and images (PNG, JPEG).


## Infrastructure

Granite 4.1 Vision was trained on IBM's Blue Vela supercomputing cluster, outfitted with
NVIDIA H100 GPUs. The training was done on 32 GPUs for approximately 200 hours.


## Ethical Considerations and Limitations

The use of vision-language models involves certain risks that should be considered before
deployment:
- **Task scope:** The model is specifically designed for structured extraction tasks and
  may not generalize well to open-ended vision-language tasks.
- **Hallucination:** As with all generative models, outputs should be validated before
  use in automated pipelines, particularly for high-stakes document processing.
- **Language:** The model is trained on English instructions only and may produce
  degraded results for documents in other languages.

To enhance safety in enterprise deployments, we recommend using Granite 4.1 Vision
alongside [Granite Guardian](https://huggingface.co/ibm-granite/granite-guardian-4.1-8b),
a model designed to detect and flag risks in inputs and outputs across key dimensions
outlined in the IBM AI Risk Atlas.


## Resources
- ⭐️ Learn about the latest updates with Granite: https://www.ibm.com/granite
- 🚀 Get started with tutorials, best practices, and prompt engineering advice: https://www.ibm.com/granite/docs/
- 💡 Granite learning resources: https://ibm.biz/granite-learning-resources


## Citation

```bibtex
@misc{granite-vision-4.1-4b,
  title={Granite 4.1 Vision},
  author={IBM Granite Vision Team},
  year={2026},
  url={https://huggingface.co/ibm-granite/granite-vision-4.1-4b}
}

@article{kondic2026chartnet,
  title={ChartNet: A Million-Scale, High-Quality Multimodal Dataset for Robust Chart Understanding},
  author={Kondic, Jovana and Li, Pengyuan and Joshi, Dhiraj and Sanchez, Isaac and Wiesel, Ben and Abedin, Shafiq and Alfassy, Amit and Schwartz, Eli and Caraballo, Daniel and Cinar, Yagmur Gizem and Scheidegger, Florian and Ross, Steven I. and Weidele, Daniel Karl I. and Hua, Hang and Arutyunova, Ekaterina and Herzig, Roei and He, Zexue and Wang, Zihan and Yu, Xinyue and Zhao, Yunfei and Jiang, Sicong and Liu, Minghao and Lin, Qunshu and Staar, Peter and Lastras, Luis and Oliva, Aude and Feris, Rogerio},
  journal={arXiv preprint arXiv:2603.27064},
  year={2026}
}
```