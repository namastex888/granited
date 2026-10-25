---
license: apache-2.0
pipeline_tag: time-series-forecasting
tags:
  - time series
  - forecasting
  - pretrained models
  - foundation models
  - time series foundation models
  - time-series
  - state space model
  - ssm
---
# FlowState
[Paper](https://www.arxiv.org/abs/2508.05287) | [HuggingFace Model Card](https://huggingface.co/ibm-granite/granite-timeseries-flowstate-r1) | [GitHub Model Code](https://github.com/ibm-granite/granite-tsfm/tree/main/tsfm_public/models/flowstate)

![Illustration](figs/FlowState.png)
FlowState is the first time-scale adjustable Time Series Foundation Model (TSFM), open-sourced by IBM Research.
Combining a State Space Model (SSM) Encoder with a Functional Basis Decoder allows FlowState to transition into a timescale invariant coefficient space and make a continuous forecast from this space.
This allows FlowState to seamlessly adjust to all possible sampling rates.
Therefore, training in one time-scale helps for inference at all scales, allowing for drastically improved utilization of training data across time-scales.
This innovation leads to a significant improvement in performance, making FlowState the new state-of-the art in zero-shot time series forecasting.
## Update: Changes from 1.0 to 1.1
- Inclusion of synthetic pre-training data following [CauKer](https://github.com/ShifengXIE/CauKer)
- Increased pre-training context 2048 -> 4096
- Improved S5 Layer with an output gating mechanism
- Optimized hyperparameters
- Larger MLP layer
- Total number of parameters: 18.5M

**Important**: To use FlowState-r1.1 include the `revision=r1.1` argument when loading the model, as demonstrated in the `Recommended Use` section.
If no `revision` argument is provided, the version 1.0 is used.
## Key Features
- **FlowState**: We present an SSM-based time series foundation model that can be dynamically adjusted to the specific characteristics of the time series during evaluation.
- **Functional Basis Decoder (FBD)**: We propose a novel decoder, as a critical component of FlowState, that utilizes a set of continuous basis functions to make continuous forecasts and allow seamless adjustment to specific input characteristics. 
- **Flexible temporal adaptation**: FlowState can dynamically adjust the context and target length to the timescale of the provided time series.  
- **Compact and high-performing**: With fewer than 10M parameters and the ability to forecast multiple consecutive patches in parallel, FlowState delivers state-of-the-art accuracy with exceptional efficiency.

This model card contains the model weights for the Granite version of the FlowState model, which is suitable for commercial and enterprise use. A research version of the model (available [here](https://huggingface.co/ibm-research/flowstate)) is also provided and is intended for research use only. The Granite and research versions achieve the same MASE performance on GIFT‑Eval, indicating comparable point forecasting accuracy. The research version demonstrates slightly improved CRPS performance, which evaluates the quality of probabilistic forecasts.
## Benchmark Highlights
![Illustration](figs/flowstate_performance.png)

Despite being **more than 10x smaller** than the next best models, 
FlowState is **better than all Zero-Shot models** with publicly available source code and model weights on the [GIFT-Eval Leaderboard](https://huggingface.co/spaces/Salesforce/GIFT-Eval) in terms of MASE performance, as of Apr. 28th 2026.
The Figure compares GIFT MASE Performance vs. model size for FlowState and the most competitive Zero-Shot Models.

To reproduce the ranking above on the GIFT-Eval Leaderboard, take the following steps: 1) select only "zero-shot" under "Model Types", 2) uncheck "No" under "Replication Code", 3) sort by MASE in ascending order.
## Model Details
Model Details can be found in our [Paper](https://www.arxiv.org/abs/2508.05287).
Currently FlowState only supports zero-shot forecasting.
## Recommended Use
FlowState can be used to make predictions as follows:
```Python
from tsfm_public import FlowStateForPrediction
import torch
device= 'cuda'
# Download the FlowState checkpoint:
predictor = FlowStateForPrediction.from_pretrained("ibm-granite/granite-timeseries-flowstate-r1", revision="r1.1").to(device)
time_series = torch.randn((2048, 32, 1), device=device) # context, batch, n_ch
forecast = predictor(time_series, scale_factor=0.25, prediction_length=960, batch_first=False)
print(forecast.prediction_outputs.shape) # torch.Size([32, 9, 48, 1]) (batch, quantiles, forecast_length, n_ch)
```
It is recommended for users to determine a suitable scale factor for their specific time series data, as explained in the next section.
#### Temporal Scaling
For common sampling rates, we recommend the following scaling factors.
| Sampling Rate | Recommended Scale Factor  |
|---------------|---------------------------|
| 15 min        | 0.25                      |
| 30 min        | 0.5                       |
| Hourly        | 1.0                       |
| Daily         | 3.43 if data has a weekly cylce, else 0.0656 |
| Weekly        | 0.46                      |
| Monthly       | 2                         |

For optimal performance it is recommended to first determine the seasonality of their data and to calculate the scale factor.

Assuming data has repeating structures every N=96 time steps (such as quarter hourly sampled data with a daily cycle), resulting in seasonality 96, the scale factor can be calculated as follows:

scale_factor = Base Seasonality / N = 24 / 96 = 0.25

Where 24 is the base seasonality used during pretraining.
If the seasonality is unclear, it is best to experiment with different scale factors and select what works best.
We recommend forecasting no more than 30 seasons (in our example 96*30=2880 time steps).
Afterward, forecasting quality declines.
## Installation
To run FlowState follow the installation instructions [here](https://github.com/ibm-granite/granite-tsfm/?tab=readme-ov-file#initial-setup).
For the GIFT evaluation notebook we recommend using python 3.11, and installing gift-eval according to their [repo](https://github.com/SalesforceAIResearch/gift-eval). 
## Example Recipes and Notebooks
- Getting started notebook: [here](https://github.com/ibm-granite/granite-tsfm/tree/main/notebooks/hfdemo/flowstate_getting_started.ipynb)
- GIFT Eval Notebook: [here](https://github.com/ibm-granite/granite-tsfm/tree/main/notebooks/hfdemo/flowstate_gift_eval.ipynb)
## Pretraining Data
As pretraining data, we used a subset of [Gift-Eval Pretrain](https://huggingface.co/datasets/Salesforce/GiftEvalPretrain), and a subset of the [Chronos Pretraining Data Corpus](https://huggingface.co/datasets/autogluon/chronos_datasets).
None of the used datasets (or sub/up-sampled versions thereof) are contained in Gift-Eval (neither train, validation nor test split).
All our Gift-Eval results are Zero-Shot.
## Citation
Please cite the following paper if you intend to use our model or its associated architectures/approaches in your work.
### BibTeX:
```
@article{graf2025flowstate,
title={FlowState: Sampling Rate Invariant Time Series Forecasting},
  author={Graf, Lars and Ortner, Thomas and Wo{\'L}{\c{s}}niak, Stanis{\'L} and Pantazi, Angeliki and others},
  journal={arXiv preprint arXiv:2508.05287},
  year={2025}
}
```
## Model Card Authors
Lars Graf, Thomas Ortner, Stanislaw Wozniak, Angeliki Pantazi
##  IBM Public Repository Disclosure 
All content in this repository including code has been provided by IBM under the associated open source software license and IBM is under no obligation to provide enhancements, updates, or support. IBM developers produced this code as an open source project (not as an IBM product), and IBM makes no assertions as to the level of quality nor security, and will not be maintaining this code going forward.