---
license: cdla-permissive-2.0
library_name: terratorch
pipeline_tag: image-to-image
---
# Model card for granite-geospatial-wxc-downscaling

<!-- [<b><i>>>Try it on Colab<<</i></b> (Please select the T4 GPU runtime)](https://colab.research.google.com/github/IBM/granite-wxc/blob/main/examples/granitewxc_downscaling/notebooks/granitewxc_downscaling_inference.ipynb)   -->

`granite-geospatial-wxc-downscaling` is a fine-tuned foundation model for the downscaling of weather and climate data. It is based on the [Prithvi WxC foundation model](https://huggingface.co/collections/ibm-nasa-geospatial/prithvi-for-weather-and-climate-6740a9252d5278b1c75b3418). `granite-geospatial-downscaling` has been used to downscale both MERRA-2 data, ECCC data as well as EURO-CORDEX climate simulations. The weights for the former are included here.

<b>6x downscaling of MERRA-2 2m temperature</b>

<center><img src="downscaling_T2M_coolwarm_animated.gif" alt="Downscaling of MERRA-2 T2M" width=462></center>

<b>8x downscaling of ECCC's u10 wind component</b>

<center><img src="downscaling_eccc_u10.png" alt="Downscaling of ECCC's u10 Wind Component" width=462></center>


More information: [Code](https://github.com/IBM/granite-wxc), [base model](https://huggingface.co/collections/ibm-nasa-geospatial/prithvi-for-weather-and-climate-6740a9252d5278b1c75b3418), paper (to appear).

## Architecture

From an architecture point of view, we embed Prithvi WxC's transformer layers into a series of convolutional layers. That is, we typically increase resolution before and after the pre-trained transformer stages.

## Data - MERRA-2

As a reference and baseline how to use Prithvi WxC as well as the downscaling architecture, we have used `granite-geospatial-downscaling` for 6x downscaling of MERRA-2 2m temperature data. That is, we take MERRA-2 data of 0.5 x 0.625 degrees resolution, coarsen it by a factor of six along each axis and then apply an additional smoothing filter via a 3x3 convolution. Subsequently we fine-tune the above architecture to recover the high resolution data. The weights for this are included here.

## Data - ECCC (Environment and Climate Change Canada)

We use Prithvi WxC for the downscaling task on Canada’s operational Numerical Weather Prediction (NWP) systems. Specifically, the goal is to downscale forecasts from the Global Deterministic Prediction System (GDPS)—which provides 10-day forecasts at ~15 km resolution—to the High-Resolution Deterministic Prediction System (HRDPS), which produces 48-hour forecasts at ~2.5 km resolution. The weights for this are included here.

## Further applications - EURO-CORDEX

In addition, we have used the same architecture with different hyperparameter choices for a 12x downscaling of a subset of EURO-CORDEX climate simulation.