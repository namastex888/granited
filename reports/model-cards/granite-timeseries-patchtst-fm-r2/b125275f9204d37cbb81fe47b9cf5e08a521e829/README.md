---
license: openmdw-1.0
pipeline_tag: time-series-forecasting
tags:
- time series
- forecasting
- pretrained models
- foundation models
- time series foundation models
- time-series
---

# Granite TimeSeries PatchTST-FM-r2 Model Card

## Model Description

PatchTST-FM-r2, a state-of-the-art zero-shot time series foundation model, represents a continuation of the well-recognized PatchTST model series, building on the original [PatchTST](https://arxiv.org/abs/2211.14730) and its zero-shot variant [PatchTST-FM-r1](https://huggingface.co/ibm-granite/granite-timeseries-patchtst-fm-r1). PatchTST-FM-r2 brings architectural enhancements as well as an expanded training base on top of its predecessor PatchTST-FM-r1. 
As of August 31, 2026 Granite-TimeSeries-PatchTST-FM-r2 is the **top performing** zero-shot model released under a permissive, commercial-friendly open-source license on the [GIFT-Eval](https://huggingface.co/spaces/Salesforce/GIFT-Eval) benchmark. Granite-TimeSeries-PatchTST-FM-r2 ranks #2 when considering all zero-shot, replicable models (see below for more details).

The architectural changes in r2 include:
 - Introduction of a convolution layer after the multi-head self-attention (MHSA) layer, sandwiched between two "half-step" FFNs. This block, known as the [Conformer](https://arxiv.org/pdf/2005.08100) block, previously gained popularity in speech recognition. The convolution sublayer tends to capture near-term information between patches thus enabling the MHSA to focus on long-term relationships.  
 - Plain-transformer blocks are replaced by Conformer blocks with alternating convolution kernel sizes of 3 and 5, in a repeating pattern {5, 5, 3, 3}
 - Overlapping patches of size 16 with stride of 8, combined with a Hamming window loss weighting at training time, and an overlap-and-add forecasting approach at inference time
 - The overall number of blocks increased to 30 (from 20) to absorb expanded training data
 - A pre-head layer norm is applied for enhanced training stability
 - Training is expanded by synthetic data generated using the [CauKer method](https://arxiv.org/pdf/2508.02879), as used in the training of the [Granite-FlowState](https://huggingface.co/ibm-granite/granite-timeseries-flowstate-r1) model
 
As with PatchTST-FM-r1, the r2 model was trained on a diverse set of data (see details below) using context length of 8192. The model hidden dimension is set to 1024, patch_length is 16, and the quantile head spans 99 quantiles. In total, the model has ~385M parameters.

The current implementation of the new model architecture is available in the [IBM TSFM repository](https://github.com/ibm-granite/granite-tsfm/) and remains backward compatible with checkpoints for PatchTST-FM-r1. Previous implementations of PatchTST can be found here:
- [Official HuggingFace implementation](https://huggingface.co/docs/transformers/model_doc/patchtst): Contributed by members of the IBM TSFM team.
- [Original PatchTST Implementation](https://github.com/yuqinie98/PatchTST): Implemented by an IBM Research intern who was part of the IBM TSFM team, and the first author of the original PatchTST paper.

## License
This model is dual‑licensed under the OpenMDW License version 1.0 and the Apache License 2.0. Users may choose to use the model under either license.

## Usage

Install the Granite-TSFM package
```
pip install "granite-tsfm>=0.3.9"
```

Make zero-shot predictions using the HF Pipeline-based API:
```python
import pandas as pd
from tsfm_public import PatchTSTFMForPrediction, TimeSeriesForecastingPipeline

# Load model weights
model = PatchTSTFMForPrediction.from_pretrained("ibm-granite/granite-timeseries-patchtst-fm-r2")

# Read some sample data from ETTh
df = pd.read_csv(
    "https://raw.githubusercontent.com/zhouhaoyi/ETDataset/main/ETT-small/ETTh1.csv",
    parse_dates=["date"],
)

# Set up the forecasting pipeline
pipe = TimeSeriesForecastingPipeline(
    model=model,
    id_columns=[],
    timestamp_column="date",
    target_columns=["HUFL"],
    max_context_length=model.config.context_length,
    context_length=512,
    prediction_length=64,
    impute_method=None,
    quantile_levels=[0.1, 0.5, 0.9],
    explode_forecasts=True,
    freq="1h",
)

# Create a forecast from the last 512 samples of the input dataframe
forecast = pipe(df.iloc[-512:])
```

## Performance

As of August 31, 2026 Granite-TimeSeries-PatchTST-FM-r2 **ranks number two** on the GIFT-Eval benchmark for CRPS and MASE when considering replicable, zero-shot models. Results for both CRPS and MASE performance are shown below.
The GIFT-Eval benchmark compares a variety of different model types. To reproduce the ranking below, take the following steps: 1) select only "zero-shot" under "Model Types", 2) uncheck "Yes" under "Test Leak", and 3) uncheck "No" under "Replication Code". Note that Granite-PatchTST-FM-r2 results are in a pending PR submitted to the GIFT-Eval benchmark.

<div style="display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; min-height: 100vh;">
  <img src="figures/crps_zero.png" alt="model ranking" style="width: 90%; height: auto;" />
  <img src="figures/mase_zero.png" alt="model ranking" style="width: 90%; height: auto;" />
  <br>
  <em style="display: block; margin-top: 0.5rem; text-align: center;">
    The geometric mean CRPS and MASE scores of the top replicable, zero-shot models on the <a href="https://huggingface.co/spaces/Salesforce/GIFT-Eval">GIFT-Eval</a> benchmark (results as of August 31, 2026), lower is better. Models shown in blue are released by IBM TSFM.
  </em>
</div>


When including pretrained along with zero-shot replicable models, PatchTST-FM-r2 ranks 3rd and 4th in CRPS and MASE, respectively. Recall that pretrained models are allowed to use the train portion of the GIFT-Eval evaluation datasets in their pretraining corpus. As can be seen from the charts, PatchTST-FM-r2 exceeds the performance of several pretrained models, including Chronos-2, and Timer-S1, and Toto variants, some of which are significantly larger models.

<div style="display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; min-height: 100vh;">
  <img src="figures/crps_zero_pretrained.png" alt="model ranking" style="width: 90%; height: auto;" />
  <img src="figures/mase_zero_pretrained.png" alt="model ranking" style="width: 90%; height: auto;" />
  <br>
  <em style="display: block; margin-top: 0.5rem; text-align: center;">
    The geometric mean CRPS and MASE scores of the top replicable, zero-shot & pretrained models on the <a href="https://huggingface.co/spaces/Salesforce/GIFT-Eval">GIFT-Eval</a> benchmark (results as of August 31, 2026), lower is better. Models shown in blue are released by IBM TSFM.
  </em>
</div>


## Training Data

The training data composes four separate sources:
 - Selected subset of datasets from [GiftEvalPretrain](https://huggingface.co/datasets/Salesforce/GiftEvalPretrain),
 - Custom synthesized data: Based on [KernelSynth](https://github.com/amazon-science/chronos-forecasting/blob/main/scripts/kernel-synth.py) with a different set of periodic kernels (as suggested by the Tirex paper), and a small volume of augmentations
 - A TSMixup dataset, based on the same process described in the [Chronos paper](https://arxiv.org/abs/2403.07815), but using only datasets which are not in the GIFT-Eval evaluation set.
 - CauKer dataset. ~0.5M sequences generated in-house using the [method](https://arxiv.org/pdf/2508.02879). Each sequence has a length of 4096.


## Citation

Please cite the following paper if you intend to use our model or its associated architectures/approaches in your work.

```BibTeX:
@misc{wen2026revisitingtransformer,
      title={Revisiting the Generic Transformer: Deconstructing a Strong Baseline for Time Series Foundation Models}, 
      author={Yunshi Wen and Wesley M. Gifford and Chandra Reddy and Lam M. Nguyen and Jayant Kalagnanam and Anak Agung Julius},
      year={2026},
      eprint={2602.06909},
      archivePrefix={arXiv},
      primaryClass={cs.LG},
      url={https://arxiv.org/abs/2602.06909}, 
}
```

## Model Card Authors
Jiri Navratil, Wesley M. Gifford, Yunshi Wen, Chandra K. Reddy, Agung Julius

## Acknowledgements
This work builds upon previous joint effort between Rensselaer Polytechnic Institute and IBM (see the PatchTST-FM-r1 [modelcard](https://huggingface.co/ibm-granite/granite-timeseries-patchtst-fm-r1)).

The CauKer dataset was generated by the FlowState team: Lars Graf, Thomas Ortner, Stanislaw Wozniak, Angeliki Pantazi.

##  IBM Public Repository Disclosure 
All content in this repository including code has been provided by IBM under the associated open source software license and IBM is under no obligation to provide enhancements, updates, or support. IBM developers produced this code as an open source project (not as an IBM product), and IBM makes no assertions as to the level of quality nor security, and will not be maintaining this code going forward.
