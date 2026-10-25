---
license: apache-2.0
library_name: terratorch
pipeline_tag: image-feature-extraction
---

#  Model Card for granite-geospatial-canopy-height

[<b><i>>>Try it on Colab<<</i></b>](https://colab.research.google.com/github/ibm-granite/granite-geospatial-canopyheight/blob/main/notebooks/canopyheight_inference_getting_started.ipynb)

<p align="center" width="100%">
<img src="canopyheight_map.png" width="600">
</p>

The granite-geospatial-canopy-height model is a fine-tuned geospatial foundation model for predicting the canopy height (i.e., height of trees and vegetationon the Earth's surface) using optical satellite imagery. 
Canopy Height is an important component to quantify the carbon cycle and is crucial for estimating crop yields, monitoring forest timber production, and quantifying the carbon sequestered by nature-based actions.

The model predicts canopy height from the Harmonized Landsat and Sentinel-2 (HLS) L30 optical satellite imagery and is fine-tuned using training labels from the 
Global Ecosystem Dynamics Investigation (GEDI) L2A. Uniquely, the model has been fine-tuned using HLS and GEDI data collected from 15 biomes across the globe.
Please see the Model Description below for more details.

## How to Get Started with the Model

This model was trained using [Terratorch](https://github.com/IBM/terratorch).

We make the weights as well as the configuration file that defines it available.

You can use it easily with Terratorch through:

```python
from terratorch.cli_tools import LightningInferenceModel

ckpt_path = hf_hub_download(repo_id="ibm-granite/granite-geospatial-canopyheight", filename="pytorch_model.bin")
config_path = hf_hub_download(repo_id="ibm-granite/granite-geospatial-canopyheight", filename="config.yaml")

model = LightningInferenceModel.from_config(config_path, ckpt_path)

inference_results, input_file_names = model.inference_on_dir(<input_directory>)
```

For more details, check out the [Getting Started Notebook](https://github.com/ibm-granite/granite-geospatial-canopyheight/blob/main/notebooks/canopyheight_getting_started.ipynb) which guides the user through three experiments:

1. Zero-shot for all biomes
2. Zero-shot for a single biome
3. Few-shot for a single biome

## Model Description

The granite-geospatial-canopyheight model is a geospatial foundation model that has been fine-tuned using HLS and GEDI data to perform regression.

The base foundation model from which the granite-geospatial-canopyheight model is fine-tuned is similar to that described in this [paper](https://arxiv.org/abs/2310.18660), 
with the exception that the backbone is a Swin-B transformer. We opted for the Swin-B backbone instead of the ViT in the original paper because the Swin-B provides the following advantages:
- a smaller starting patch size which provides a higher effective resolution
- windowed attention which provides better computational efficiency
- hierarchical merging which provides a useful inductive bias

The base foundation model was pretrained using SimMIM, a self-supervised learning strategy based on masking large parts of the input (HLS2) data which are then reconstructed by the model. A small decoder composed of a single convolutional layer and a Pixel Shuffle module was added to the Swin-B backbone for the (pretraining) reconstruction task.

For fine-tuning, we replaced the small decoder with a UPerNet adapted for pixel-wise regression. We opted for the UPerNet because it provides fusion between transformer blocks, a similar intuition to the Unet which is consistently considered state-of-the-art for regression tasks with earth observation data. As the standard UPerNet implementation using the Swin-B backbone predicts a final feature map 4x smaller than the input, we appended two Pixel Shuffle layers to learn the upscaling. More details on the fine-tuned model can be found in this [paper](https://doi.org/10.1109/IGARSS53475.2024.10640630).


## Model Releases (along with the branch name where the models are stored):

- **tag v1 —** - 04/11/2024

- Stay tuned for more models!
 
### Model Sources

- **Repository:** https://github.com/ibm-granite/granite-geospatial-canopyheight/
- **Paper (canopyheight):** https://doi.org/10.1109/IGARSS53475.2024.10640630
- **Paper (foundation model):** https://arxiv.org/abs/2310.18660 

### External Blogs
- https://research.ibm.com/blog/img-geospatial-studio-think

## Training Data

The model was trained on a collection of datasets provided by NASA:
- Harmonized Landsat-Sentinel 2 (HLS) L30: https://lpdaac.usgs.gov/products/hlss30v002/
- Global Ecosystem Dynamics Investigation (GEDI) L4A: https://doi.org/10.3334/ORNLDAAC/1907

For training and testing, the model requires a cloud-free snapshot of an area where all pixels are representative of the spectral bands for that location. The approach we used to create the cloud free images was to acquire HLS data during the leaf-on season for each hemisphere, analyze the timeseries, and select pixels that are not contaminated with clouds. We compute the mean value of each cloud-free pixel during the leaf-on season for each spectral band which is then assembled into a composite image representative for that area. The corresponding GEDI L4A canopy height data obtained made during the same leaf-on season are interpolated to the HLS grid (CRS:4326) such that the measured canopy height points are aligned with HLS data. GEDI data is spatially and temporaly sparse so pixels with no corresponding GEDI measurement are filled with a no data value.


<!-- TODO: add citation -->
## Citation
Kindly cite the following paper, if you intend to use our model or its associated architectures/approaches in your work

**BibTeX:**

```
@inproceedings{da2024geospatial,
  title={Geospatial Foundational Model for Canopy Height Estimates Across Kenya’s Ecoregions},
  author={Da Silva, Ademir Ferreira and Zortea, Maciel and Kuehnert, Julian and Atluri, Anjani and Singh, Gurkwandar and Srinivasan, Harini and Klein, Levente J},
  booktitle={IGARSS 2024-2024 IEEE International Geoscience and Remote Sensing Symposium},
  pages={2853--2857},
  year={2024},
  organization={IEEE}
}
```

**APA:** 
```
Da Silva, A. F., Zortea, M., Kuehnert, J., Atluri, A., Singh, G., Srinivasan, H., & Klein, L. J. (2024, July). Geospatial Foundational Model for Canopy Height Estimates Across Kenya’s Ecoregions. In IGARSS 2024-2024 IEEE International Geoscience and Remote Sensing Symposium (pp. 2853-2857). IEEE.
```


## Model Card Authors

Julian Kuehnert, Levente Klein, Campbell Watson and Thomas Brunschwiler


## IBM Public Repository Disclosure: 

All content in this repository including code has been provided by IBM under the associated 
open source software license and IBM is under no obligation to provide enhancements, 
updates, or support. IBM developers produced this code as an 
open source project (not as an IBM product), and IBM makes no assertions as to 
the level of quality nor security, and will not be maintaining this code going forward.