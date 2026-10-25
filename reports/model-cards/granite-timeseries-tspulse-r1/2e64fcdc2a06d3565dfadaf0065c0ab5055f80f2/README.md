---
license: apache-2.0
tags:
- time series
- pretrained models
- foundation models
- time series foundation models
- time-series
library_name: granite-tsfm
---

# Granite-TimeSeries-TSPulse-R1 Model Card

<p align="center" width="100%">
<img src="tspulse_overview.webp" width="600">
</p>

> 🚀 **Update (Mar 2026): TSPulse has been accepted at ICLR 2026.**

**TSPulse** models are ultra-compact pre-trained models for time-series data, featuring just **1M parameters** and supporting **GPU-free inference**. Designed for versatility, TSPulse excels across a wide range of tasks including **classification**, **anomaly detection (AD)**, **imputation**, and **similarity search**.

At the architecture level, TSPulse introduces a novel dual-space masked reconstruction strategy that learns jointly from both time and frequency domains —- capturing complementary patterns in a unified embedding space. This is coupled with a dual-embedding disentanglement mechanism that produces both fine-grained embeddings for detailed analysis and semantic embeddings for broader understanding. These semantic embeddings are inherently robust to variations in time, magnitude, and noise, making them ideal for time-series retrieval.

At the task level, TSPulse integrates several innovations:
- **TSLens**: A fine-tuning module for task-aware feature extraction.
- **Multi-head triangulation**: Fuses outputs from multiple prediction streams to enhance anomaly detection robustness.
- **Hybrid masking**: Reduces bias during pre-training, improving zero-shot imputation.

## Highlights

TSPulse demonstrates significant improvements on several benchmarks:
- **+5–16%** on UEA classification benchmarks  
- **+20%** on the TSB-AD benchmark for anomaly detection (rank #1 in both univariate and multivariate)  
- **+50%**  in zero-shot imputation  
- **+25%**  in semantic similarity search  

Despite being **10–100× smaller** than competing models, TSPulse sets a new standard in efficient time-series foundation modeling—combining strong out-of-the-box performance, minimal compute requirements, and rapid adaptability to downstream tasks.

### Links

- **Paper:** [ArXiV](https://arxiv.org/pdf/2505.13033)
- **Repository:** https://github.com/ibm-granite/granite-tsfm
- **PyPI project:** https://pypi.org/project/granite-tsfm/
- **Model architecture:** https://github.com/ibm-granite/granite-tsfm/tree/main/tsfm_public/models/tspulse
- **Time Series Cookbook:** https://github.com/ibm-granite-community/granite-timeseries-cookbook
- **Paper Reproducability Scripts** https://github.com/ibm-granite/granite-tsfm/tree/main/notebooks/hfdemo/tspulse



## Model Specialization

Inspired by the success of small, task-specialized pre-trained models in the language/vision domain which achieve strong performance 
through minimal task-specific adaptations —- we extend this strategy to time-series. Since TSPulse offers multiple heads we specialize 
the pre-training for every task through reweighting loss objectives to prioritize heads most relevant to the target task. This enables 
TSPulse to refine task-specific representations while maintaining its lightweight design, facilitating efficient transfer learning 
across any datasets for the specified downstream task.

### 🎯 Specialized TSPulse Variants

We release three pre-trained variants optimized for specific time-series tasks. 


- **`tspulse-hybrid-allhead-512-p8-r1`**  
  🔹 *Recommended for:* **Anomaly Detection**  
  🔹 *Branch:* `main`

- **`tspulse-hybrid-dualhead-512-p8-r1`**  
  🔹 *Recommended for:* **Imputation and Search**  
  🔹 *Branch:* `tspulse-hybrid-dualhead-512-p8-r1`

- **`tspulse-block-dualhead-512-p16-r1`**  
  🔹 *Recommended for:* **Classification**  
  🔹 *Branch:* `tspulse-block-dualhead-512-p16-r1`


To load a specific variant, use the revision argument in the from_pretrained call as illustrated below:

# Example

```python
from tsfm_public.models.tspulse import TSPulseForReconstruction

# ✅ For Imputation and Semantic Search
model = TSPulseForReconstruction.from_pretrained(
    "ibm-granite/granite-timeseries-tspulse-r1",
    revision="tspulse-hybrid-dualhead-512-p8-r1"
)

# ✅ For Anomaly Detection
model = TSPulseForReconstruction.from_pretrained(
    "ibm-granite/granite-timeseries-tspulse-r1",
    revision="main"
)

# ✅ For Classification
from tsfm_public.models.tspulse import TSPulseForClassification

model = TSPulseForClassification.from_pretrained(
    "ibm-granite/granite-timeseries-tspulse-r1",
    revision="tspulse-block-dualhead-512-p16-r1"
)
```


### 🧩 Model Naming Convention

Each model name follows this format:  
**`tspulse-<mask_type>-<head_type>-<context_length>-p<patch_length>-r<release_number>`**

- **`mask_type`**: Type of masking used during pre-training  
  → `hybrid` or `block`
- **`head_type`**: Prediction heads enabled during pre-training  
  → `allhead` = all heads enabled  
  → `dualhead` = time and prob heads enabled
- **`context_length`**: Base Context length of the model
  → e.g., `512`
- **`patch_length`**: Length of time-series patches used during training  
  → e.g., `p8` or `p16`
- **`release_number`**: Model release version  
  → e.g., `r1`



### Example recipes and notebooks

- Zero-shot Anomaly Detection [[Example]](https://github.com/ibm-granite/granite-tsfm/blob/main/notebooks/hfdemo/tspulse_anomaly_detection.ipynb)
- Zero-shot Imputation [[Example]](https://github.com/ibm-granite/granite-tsfm/blob/main/notebooks/hfdemo/tspulse_imputation.ipynb) [[Cookbook]](https://github.com/ibm-granite-community/granite-timeseries-cookbook/blob/main/recipes/Imputation/Getting_Started_with_TSPulse_Imputation.ipynb)
- Zero-shot Similarity Search [[Simple Search Example]](https://github.com/ibm-granite/granite-tsfm/blob/main/notebooks/hfdemo/tspulse_search_simple_example.ipynb) [[Indexing Example]](https://github.com/ibm-granite/granite-tsfm/blob/main/notebooks/hfdemo/tspulse_search_with_faiss.ipynb)
- Multivariate Classification [[Example]](https://github.com/ibm-granite/granite-tsfm/blob/main/notebooks/hfdemo/tspulse_classification.ipynb) [[Cookbook]](https://github.com/ibm-granite-community/granite-timeseries-cookbook/blob/main/recipes/Classification/Getting_Started_with_TSPulse_Classification.ipynb)
- Finetuning script (to be added soon)
  

### Usage guidelines

The model is trained with a base context length of **512**. Different tasks have varying minimum input length requirements:

- **Anomaly Detection (AD):**  
  Requires time-series of at least **3–4× the base context length** (i.e., **~1536–2048 points**) to ensure stable and robust anomaly detection.

- **Imputation & Semantic Search:**  
  Require a **minimum of 512 points** to align with the model's context window and enable meaningful processing.

- **Classification:**  
  Supports **any input length**. Internally, the model resamples or interpolates the input to a fixed length of **512** before inference.


    
## Benchmarks

Benchmarking scripts to reproduce paper results will be added soon. Stay tuned!



## Training Data

TSpulse r1 was trained on a collection of datasets as follows:
 - Australian Electricity Demand: https://zenodo.org/records/4659727 
 - Australian Weather: https://zenodo.org/records/4654822 
 - Bitcoin: https://zenodo.org/records/5122101 
 - KDD Cup 2018: https://zenodo.org/records/4656756 
 - London Smart Meters: https://zenodo.org/records/4656091 
 - Saugeen River Flow: https://zenodo.org/records/4656058
 - Solar Power: https://zenodo.org/records/4656027 
 - Sunspots: https://zenodo.org/records/4654722
 - Solar: https://zenodo.org/records/4656144 
 - US Births: https://zenodo.org/records/4656049 
 - Wind Farms Production: https://zenodo.org/records/4654858 
 - Wind Power: https://zenodo.org/records/4656032
 - PEMSD3, PEMSD4, PEMSD7, PEMSD8, PEMS_BAY: https://drive.google.com/drive/folders/1g5v2Gq1tkOq8XO0HDCZ9nOTtRpB6-gPe
 - LOS_LOOP: https://drive.google.com/drive/folders/1g5v2Gq1tkOq8XO0HDCZ9nOTtRpB6-gPe 


## Citation
Please cite the following paper if you intend to use our model or its associated architectures/approaches in your 
work.

**BibTeX:**

```
@misc{ekambaram2025tspulsedualspacetiny,
      title={TSPulse: Dual Space Tiny Pre-Trained Models for Rapid Time-Series Analysis}, 
      author={Vijay Ekambaram and Subodh Kumar and Arindam Jati and Sumanta Mukherjee and Tomoya Sakai and Pankaj Dayama and Wesley M. Gifford and Jayant Kalagnanam},
      year={2025},
      eprint={2505.13033},
      archivePrefix={arXiv},
      primaryClass={cs.LG},
      url={https://arxiv.org/abs/2505.13033}, 
}
```

## Model Card Authors

Vijay Ekambaram, Subodh Kumar, Arindam Jati, Sumanta Mukherjee, Tomoya Sakai, Pankaj Dayama, Wesley M. Gifford, and Jayant Kalagnanam


## IBM Public Repository Disclosure

All content in this repository including code has been provided by IBM under the associated 
open source software license and IBM is under no obligation to provide enhancements, 
updates, or support. IBM developers produced this code as an 
open source project (not as an IBM product), and IBM makes no assertions as to 
the level of quality nor security, and will not be maintaining this code going forward.
