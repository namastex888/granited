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
  - tinytimemixer
  - granite-ttm-r3
---

# Granite-TTM-R3 Model Card

![](./ttm_image.webp)

Building on top of **TTM-R1** and **TTM-R2**, we introduce the next generation of TinyTimeMixer under the Granite time-series foundation model family — **Granite-TTM-R3**. This release incorporates several novel *tiny-neural architectural innovations* designed to push the limits of **accuracy in high-speed forecasting**, a critical requirement for real-world production deployments.

---

## Supported Tasks & Capabilities

**Granite-TTM-R3** is a family of pretrained models supporting multiple real-world forecasting scenarios:

- **Zero-shot forecasting** across unseen datasets
- **Few-shot adaptation** effective with as few as ~1K samples
- **Full fine-tuning** for domain-specific optimization
- **Multivariate time-series forecasting**
- **Exogenous / control variable integration**
- **High-throughput batch inference** for production systems

---

## Architecture Overview

Granite-TTM adopts a mixture-of-experts paradigm composed of models with varying complexities [1-35M and Lite: 1-18M parameters] — coupled with a lightweight routing mechanism that automatically selects or blends the most suitable expert based on input data characteristics.

This enables adaptive model selection, improving both accuracy and efficiency across diverse time-series scenarios.

The architecture is built on efficient mixer-based designs that avoid expensive self-attention. Instead, Granite-TTM-R3 leverages linear gating-based attention mechanisms to capture temporal dependencies with significantly lower computational overhead.

This combination allows **Granite-TTM-R3** to deliver scalable, adaptive, and ultra-fast forecasting performance suitable for real-time and large-scale deployments.

---

## Accuracy & Speed

**Granite-TTM-R3** is engineered to achieve a strong balance between **state-of-the-art accuracy** and **extreme inference efficiency**, making it well-suited for real-world, high-throughput deployments.

### Accuracy on GIFT-Eval 

- Maintains **top-tier performance** on the GIFT-Eval leaderboard. 
- **Fine-tuned FM**:  
  - MASE: **0.718** | CRPS: **0.514**  
  - *(Lite)* MASE: **0.719** | CRPS: **0.514**
- **Pre-trained FM**:  
  - MASE: **0.727** | CRPS: **0.523**  
  - *(Lite)* MASE: **0.733** | CRPS: **0.524**

---

## Inference Throughput

**Granite-TTM-R3** delivers orders-of-magnitude faster inference compared to existing popular SOTA models.

#### 🖥️ GPU Throughput
- Typical SOTA models: **~20–500 samples/sec**
- **Granite-TTM-R3**: **~7,500 samples/sec**
- **Granite-TTM-R3 Lite**: **~18,000 samples/sec**

#### 💻 CPU Throughput
- Typical SOTA models: **~1–20 samples/sec**
- **Granite-TTM-R3**: **~180 samples/sec**
- **Granite-TTM-R3 Lite**: **~800 samples/sec**

👉 **Granite-TTM-R3 models achieves ~15–50× speedup** over many existing approaches, **without compromising accuracy**, setting a new benchmark for fast and reliable time-series forecasting.


---

## What’s New in Granite-TTM-R3

- **Trend–Residual Decomposition**  
  Separately models long-term trends and high-frequency residuals for improved structural learning.

- **Three-Stage Pre-Training**  
  Sequential training of trend, residual, and joint components for better stability and convergence.

- **Student-Teacher Pretraining**  
  Student-teacher based pretraining for stable learning in noisy datasets.

- **Enhanced Data Augmentation**  
  Structured perturbations improve robustness across domains.

- **Improved Normalization Strategy**  
  Stabilizes training across scale shifts and heterogeneous datasets.

- **GLU Gating**  
  Dynamic information flow control within mixer blocks.

- **Multi-Resolution Temporal Layer**  
  Captures dependencies across short-, medium-, and long-term horizons.

- **FFT-Based Embeddings**  
  Incorporates frequency-domain signals to model periodicity and seasonality.

- **Register Tokens**  
  Learnable global tokens to encode sequence-level semantics.

- **Multi-Quantile Forecasting Head**  
  Enables probabilistic forecasting with multiple quantiles.

- **Refined Loss Weighting**  
  Balances trend, residual, and quantile objectives for improved calibration.

---

## ⚡ Why Granite-TTM-R3

**Granite-TTM-R3** is purpose-built for production-grade time-series systems where:

- **Low latency** is critical for real-time inference
- **High scale** is required for millions of forecasts
- **Data is heterogeneous and evolving**
- **Compute constraints exist**, including CPU-friendly deployment

It provides a practical combination of compactness, speed, and accuracy, making it suitable for industrial deployment.

---

## Example Recipes and Notebooks

To be released.

---

## Publication

- **Granite-TTM-R3 Paper**: To be released.
- **Base TTM Paper**: [Tiny Time Mixers (TTMs): Fast Pre-trained Models for Enhanced Zero/Few-Shot Forecasting of Multivariate Time Series](https://proceedings.neurips.cc/paper_files/paper/2024/hash/874a4d89f2d04b4bcf9a2c19545cf040-Abstract-Conference.html)

---

## Training Data

- Select Datasets from [GiftEvalPretrain](https://huggingface.co/datasets/Salesforce/GiftEvalPretrain) and [GIFT-Eval Train](https://huggingface.co/datasets/Salesforce/GiftEval), using non-leaking historical context.
- Custom synthesized data based on [KernelSynth](https://github.com/amazon-science/chronos-forecasting/blob/main/scripts/kernel-synth.py).

---

## Model Card Authors

Vijay Ekambaram, Arindam Jati, Haoxiang Qiu, Takayuki Katsuki, Tomoya Sakai, Priyanshul Govil, Pankaj Dayama

---

## Citation

Please cite the following paper if you intend to use this model or its associated architectures and approaches in your work.

```bibtex
@inproceedings{ekambaram2024tinytimemixersttms,
  title={Tiny Time Mixers (TTMs): Fast Pre-trained Models for Enhanced Zero/Few-Shot Forecasting of Multivariate Time Series},
  author={Vijay Ekambaram and Arindam Jati and Pankaj Dayama and Sumanta Mukherjee and Nam H. Nguyen and Wesley M. Gifford and Chandra Reddy and Jayant Kalagnanam},
  booktitle={Advances in Neural Information Processing Systems (NeurIPS 2024)},
  year={2024},
}