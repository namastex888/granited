---
license: apache-2.0
library_name: granite-tsfm
pipeline_tag: time-series-forecasting
tags:
  - time-series
  - forecasting
  - probabilistic-forecasting
  - ensemble
  - gift-eval
datasets:
  - Salesforce/GiftEval
---

# Granite Timeseries Ensemble R1

Granite Timeseries Ensemble R1 is an extensible framework for combining quantile
forecasts from multiple pre-trained time-series models. Its default configuration
brings together four complementary, permissively licensed Granite checkpoints:
[PatchTST-FM-r1](https://huggingface.co/ibm-granite/granite-timeseries-patchtst-fm-r1),
[PatchTST-FM-r2](https://huggingface.co/ibm-granite/granite-timeseries-patchtst-fm-r2),
[FlowState-r1.1](https://huggingface.co/ibm-granite/granite-timeseries-flowstate-r1),
and [TTM-r3](https://huggingface.co/ibm-granite/granite-timeseries-ttm-r3).

The default recipe eliminates the model selection burden at the starting point:
developers can obtain point and quantile forecasts without first choosing a
single Granite architecture. It requires no retraining or fitted ensemble weights.
Two methods are provided: the **Probabilistic Ensemble** (uniform probability
pooling), the recommended default, and the **IQR Ensemble** (interquartile-range
weighting), an adaptive alternative to evaluate on representative data.

This repository contains reusable ensemble recipes rather than merged model
weights. Member checkpoints are downloaded from their own Hugging Face
repositories at runtime, and the common forecasting interface can be extended
with other compatible models and aggregation methods.

## Why ensemble?

Forecasting models have different inductive biases, and no single architecture
performs best across every dataset, context length, and forecast horizon.
Granite Timeseries Ensemble combines forecasts from complementary model families,
reducing reliance on the assumptions and failure modes of any one architecture.

The default Probabilistic Ensemble uses uniform aggregation: its combination
weights require no fitting or historical validation data. This makes the aggregation
simple, reproducible, and less exposed to estimation error. Equal-weight
combinations are often surprisingly competitive with combinations fitted to
historical performance—a result known as the [forecast combination
puzzle](https://doi.org/10.1111/j.1468-0084.2008.00541.x) and explained in part by
the [bias-variance trade-off](https://doi.org/10.1287/mnsc.2019.3476) in estimated weights.

Adding diverse, reasonably accurate members can dilute model-specific errors and
improve robustness. Adding weak or highly correlated members is not guaranteed to
help, so member selection should still be validated for the intended application.
Learned aggregation may improve performance when representative training data are
available. These recipes instead prioritize a strong accuracy-simplicity trade-off
for zero-shot use.

## Ensemble methods

### Probabilistic Ensemble: the recommended default

The default `linear_pool` method gives each member equal influence. It flattens the members' quantile values into a single collection (36 values with the default quantile grid: nine from each of four members) and extracts the requested empirical quantiles from that collection. It does not learn ensemble weights.

The Probabilistic Ensemble pools member quantile values into an empirical mixture, allowing disagreement between members to influence the combined forecast spread. Linear pooling can help compensate for underdispersed forecasts, but it does not guarantee calibration and can produce overdispersed forecasts. 

### IQR Ensemble: an adaptive alternative

The `iqr_weighted` method gives greater influence to members with narrower
forecast intervals. Weights are computed separately for every sample, target,
and horizon step, without fitting to historical forecast errors.

For each member $m$, let $q_l$ be the largest available quantile level at or
below 0.25 and $q_u$ the smallest available level at or above 0.75. If either
side is unavailable, the corresponding outermost available level is used.
Before optional weight capping, the method computes:

$$
\begin{aligned}
I_m &= Q_m(q_u) - Q_m(q_l) \\
s_m &= \frac{1}{\tau (I_m + \epsilon)} \\
w_m &= \frac{p_m \exp(s_m)}{\sum_j p_j \exp(s_j)}
\end{aligned}
$$

Here, $\epsilon=10^{-6}$ stabilizes inverse spread, $\tau$ is the temperature,
and $p_m$ is an optional prior member weight. The implementation evaluates the
softmax with numerical stabilization. Equal priors are used when `weights=None`.
With the default quantile grid (0.1, 0.2, …, 0.9), the spread uses 0.2 and 0.8;
when 0.25 and 0.75 are available, it uses the interquartile range exactly.

`config.weights` supplies prior preferences in the same order as `config.members`,
not fixed final combination weights. Equal positive priors leave the adaptive
weighting unchanged; unequal priors favor particular members before normalization.
Final weights still vary by sample, target, and horizon step.

If `max_weight` is set, excess weight is redistributed among uncapped members.
The resulting weights combine corresponding member quantiles, and isotonic
regression enforces nondecreasing output quantiles.

A narrower interval does not necessarily imply greater accuracy or better
calibration. Validate both methods on representative application data.

## Granite-only configuration

Both evaluated methods use the same four Granite checkpoints:

| Member | Family | Checkpoint |
|---|---|---|
| Granite PatchTST-FM-r1 | Transformer | [`ibm-granite/granite-timeseries-patchtst-fm-r1`](https://huggingface.co/ibm-granite/granite-timeseries-patchtst-fm-r1) |
| Granite PatchTST-FM-r2 | Conformer | [`ibm-granite/granite-timeseries-patchtst-fm-r2`](https://huggingface.co/ibm-granite/granite-timeseries-patchtst-fm-r2) |
| Granite FlowState-r1.1 | State-space model | [`ibm-granite/granite-timeseries-flowstate-r1`](https://huggingface.co/ibm-granite/granite-timeseries-flowstate-r1) |
| Granite TTM-r3 | Lightweight mixer | [`ibm-granite/granite-timeseries-ttm-r3`](https://huggingface.co/ibm-granite/granite-timeseries-ttm-r3) |

The root `config.json` matches `recipes/granite-linear` and is the recommended
Probabilistic Ensemble recipe. It returns nine quantiles:
0.1, 0.2, …, 0.9, and uses the 0.5 quantile as its point forecast. TTM revisions are
selected dynamically from the requested context and forecast horizon.

## Installation and usage

```bash
pip install "granite-tsfm>=0.3.11"
```

## Minimal example: recommended defaults

```python
import pandas as pd

from tsfm_public.models.ensemble.modeling_ensemble import QuantileEnsembleForecaster

df = pd.read_csv(
    "https://raw.githubusercontent.com/zhouhaoyi/ETDataset/main/ETT-small/ETTh1.csv",
    parse_dates=["date"],
)

ensemble = QuantileEnsembleForecaster.from_pretrained(
    "ibm-granite/granite-timeseries-ensemble-r1",
)

result = ensemble(
    df.tail(1024),
    timestamp_column="date",
    target_columns=["OT"],
    context_length=1024,
    prediction_length=24,
)

print(result.prediction_outputs) # point forecast
print(result.quantile_outputs)   # quantiles
```

The first call downloads the four member checkpoints; allow extra time and
storage for this initial setup.

Check inference logs for member failures. The ensemble can continue with the remaining members, which changes the effective recipe; reproducing the reported four-member results requires all four members to run successfully.

## Advanced example: user-guided configuration

Evaluated alternatives using different aggregation methods and model combinations are available as recipe subfolders in this repository. Pass the desired path through the subfolder argument.

| Subfolder | Members | Method | Intended use |
|---|---|---|---|
| `recipes/granite-linear` | Granite-only | Probabilistic Ensemble | Recommended default |
| `recipes/granite-iqr` | Granite-only | IQR Ensemble | Permissively licensed alternative |
| `recipes/expanded-linear` | Granite and IBM Research | Probabilistic Ensemble | Research comparison |
| `recipes/expanded-iqr` | Granite and IBM Research | IQR Ensemble | Research comparison |

The optional expanded recipes reference IBM Research checkpoints with separate licensing and usage conditions, including non-commercial or research-use restrictions. Review each member model card before using these recipes.

Load an evaluated alternative with `subfolder`, or modify the configuration
before model checkpoints are loaded:

```python
from tsfm_public.models.ensemble.configuration_ensemble import ProbabilisticEnsembleConfig
from tsfm_public.models.ensemble.modeling_ensemble import QuantileEnsembleForecaster

config = ProbabilisticEnsembleConfig.from_pretrained(
    "ibm-granite/granite-timeseries-ensemble-r1",
    subfolder="recipes/granite-iqr",
)

# Optional changes to adaptive IQR weighting.
config.iqr_weighted_options = {
    "temperature": 0.8,
    "max_weight": 0.4,
}
config.weights = None  # Equal priors; final weights remain adaptive.
config.quantile_levels = [0.1, 0.25, 0.5, 0.75, 0.9]
config.validate()

ensemble = QuantileEnsembleForecaster.from_config(config)
config.save_pretrained("./my-ensemble-recipe")
```

The released IQR recipes use `temperature=1.0` and `max_weight=null` (uncapped).
Increasing `temperature` moves uncapped weights towards the normalized priors
(uniform when priors are equal). Lowering `max_weight` limits any one member's
influence.

Supported member types are `patchtst`, `flowstate`, and `ttm`. A member definition
requires `forecaster_type` and `model_checkpoint`, while FlowState may also specify
`model_revision`, `scale_factor`, and `batch_first`. TTM revisions must remain
dynamically selected and should not be pinned in the recipe.

## GIFT-Eval results

The two Granite-only four-member methods were evaluated over 97 GIFT-Eval dataset
configurations. MASE and the GIFT-Eval probabilistic score (mean_weighted_sum_quantile_loss,
shown here as CRPS) are normalized against Seasonal Naive and geometrically
aggregated across 97 configurations. Lower is better.

The table compares both official ensemble submissions with all four constituent
models. PatchTST-FM-r2 is the strongest individual member on aggregate MASE
and CRPS/MWQL in this snapshot:

| Forecast | MASE ↓ | MASE Rank ↓ | CRPS / MWQL ↓ | CRPS Rank ↓ |
|---|---:|---:|---:|---:|
| Probabilistic Ensemble | **0.683 ★** | **47.175 ★** | 0.471 | 50.948 |
| IQR Ensemble | 0.684 | 48.773 | 0.471 | **47.619 ★** |
| Granite PatchTST-FM-r2 | 0.685 | 51.371 | **0.467 ★** | 48.918 |
| Granite FlowState-r1.1 | 0.701 | 66.474 | 0.490 | 69.485 |
| Granite PatchTST-FM-r1 | 0.717 | 74.794 | 0.488 | 68.134 |
| Granite TTM-r3 | 0.724 | 83.062 | 0.520 | 88.959 |

*★ Best value among the forecasts shown. Lower is better.*

Scores and ranks are taken from the published overall leaderboard snapshot
captured on 20 October 2026. Rank columns average per-configuration ranks against the full reference leaderboard, not just these six forecasts; lower values indicate stronger average relative performance across configurations, and rank values can shift as entries are added.

Both ensembles outperform all four individual members on aggregate MASE and
average MASE Rank. The Probabilistic Ensemble achieves the lowest MASE among
these six forecasts: 0.683 compared with 0.685 for PatchTST-FM-r2. These results support the ensemble as a practical
starting point, without requiring developers to select the strongest individual
architecture in advance.

PatchTST-FM-r2 retains the best aggregate CRPS/MWQL score. The IQR Ensemble
has the lowest average CRPS Rank among the six forecasts, while the Probabilistic
Ensemble has a higher average CRPS Rank than PatchTST-FM-r2. Aggregate error and
average rank can favor different choices.

In the 20 October 2026 comparison, the ensemble approach ranks 3rd among eligible systems on MASE Rank and CRPS Rank. The figure below shows both recipes alongside the ten best eligible reference systems per metric. Some systems, including entries excluded as fine-tuned or non-permissively licensed, score better than either recipe, so the value of the ensemble is not that it tops the leaderboard. It is that a single default delivers strong, consistent results across many series without requiring you to choose a member.


![Granite-only GIFT-Eval rank comparison](assets/gift_eval_ranks_granite.svg)

*GIFT-Eval average-rank standings captured on 20 October 2026. Ten eligible
reference systems per metric are shown alongside both Granite ensembles.
Eligible reference systems have replication code and no declared test-data
leakage. Fine-tuned entries and systems with known non-permissive licenses or
member dependencies are excluded. Filtering changes the displayed
peer set, not the published rank values.*


## Extending the ensemble

The released recipes use four Granite models, but the ensemble framework is not
limited to these members. Additional forecasters can be integrated by implementing
the common `Forecaster` interface and returning predictions at the quantile levels
expected by the ensemble.

This makes it possible to:

- add other Granite or compatible external forecasting models;
- create recipes for different accuracy, latency, or licensing requirements;
- compare uniform pooling with alternative aggregation methods; and
- reuse the ensemble interface without merging or retraining member checkpoints.

New members should produce compatible forecast shapes and quantile levels. Their
accuracy, calibration, inference cost, and license compatibility should be
evaluated before deployment.

## Intended use and limitations

Granite Timeseries Ensemble R1 is intended for zero-shot time-series
forecasting and probabilistic forecasting experiments.

- Multivariate channels are forecast independently; cross-channel information is not used.
- Loading four checkpoints increases latency and memory use.
- Member context limits, horizons, preprocessing, and missing-value behavior differ.
- Aggregate gains do not mean every series benefits; average ranks do not establish calibration.
- Neither aggregation method guarantees calibration or robustness under distribution shift.
- IQR weighting can favor an overconfident member.
- Application use requires validation on representative data and ongoing monitoring.

## License

The ensemble recipe and Granite TSFM implementation are provided under Apache-2.0.
Member weights remain governed by their own repositories. The default Granite-only recipe references only Granite checkpoints. The optional expanded recipes reference separately licensed IBM Research checkpoints; review their individual model cards and license terms before use.

## References

- [Granite TSFM repository](https://github.com/ibm-granite/granite-tsfm)
- [GIFT-Eval](https://huggingface.co/spaces/Salesforce/GIFT-Eval)
- [FlowState: Sampling-Rate-Equivariant Time-Series Forecasting](https://arxiv.org/abs/2508.05287)
- [Tiny Time Mixers (TTMs): Fast Pre-trained Models for Enhanced Zero/Few-Shot Forecasting of Multivariate Time Series](https://arxiv.org/abs/2401.03955)
- [Wang et al. (2023), *Forecast combinations: An over 50-year review*](https://doi.org/10.1016/j.ijforecast.2022.11.005)
- [Smith and Wallis (2009), *A Simple Explanation of the Forecast Combination Puzzle*](https://doi.org/10.1111/j.1468-0084.2008.00541.x)
- [Gneiting and Ranjan (2013), *Combining Predictive Distributions*](https://arxiv.org/abs/1106.1638)
