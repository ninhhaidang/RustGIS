
# Description of This File

This is a list of all datasets available in the JAXA Earth API.
This is published at this URL: https://data.earth.jaxa.jp/app/mcp/catalog.v2.md
Last Update: 2026-07-01T11:12:51.619Z

# Main Recommended Data

The data with the IDs below are representative data that are easy to use among the JAXA Earth API.
Please make active use of them.
For details on the data associated with each ID, please see the "Dataset List" section.

## Land Surface Data

These datasets are observed by the ALOS series of satellites (the Advanced Land Observing Satellite-2 "Daichi-2" (ALOS-2) and the Advanced Land Observing Satellite "Daichi" (ALOS) ). Approximately 30m spatial resolution datasets are available.

- JAXA.EORC_ALOS.PRISM_AW3D30.v4.1_global
- JAXA.EORC_ALOS-2.PALSAR-2_FNF.v2.1.0_global_yearly
- JAXA.EORC_ALOS_HRLULC.v25.04_japan

## Precipitation

The Global Satellite Mapping of Precipitation (GSMaP) datasets are available, which shows the global precipitation, by using data from the GPM core satellite, meteorological satellites around the world and so on. It contains data (monthly, half-monthly and daily averages) for over 25 years since March 2000.

- JAXA.EORC_GSMaP_standard.Gauge.v6_hourly
- JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_monthly
- JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_half-monthly
- JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_daily

## Land Surface Temperature

Land surface temperature (LST) observed by the Global Change Observation Mission - Climate "SHIKISAI" (GCOM-C). When there are clouds, observations are not possible and data is missing. JAXA Earth API publishes data with a spatial resolution of about 5km. Daytime data was observed around 10:30am local time in each area, and nighttime data was observed around 10:30pm local time in each area.

- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_monthly
- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_monthly
- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_daily
- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_daily

## Sea Surface Temperature (by GCOM-C)

Sea surface temperature (SST) observed by the Global Change Observation Mission - Climate "SHIKISAI" (GCOM-C). It is characterized by its ability to observe coastal areas with a spatial resolution of approximately 250m. When there are clouds, observations are not possible and data is missing. JAXA Earth API publishes global data with a spatial resolution of approximately 250m. Daytime data was observed around 10:30am local time in each area, and nighttime data was observed around 10:30pm local time in each area.

JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.daytime.v3_global_monthly
JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.nighttime.v3_global_monthly
JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.daytime.v3_global_daily
JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.nighttime.v3_global_daily

## Sea Surface Temperature (by GCOM-W)

Sea surface temperature (SST) observed by the Global Change Observation Mission - Water "SHIZUKU" (GCOM-W). It can be observed even when there are clouds. The spatial resolution is lower than that of GCOM-C, and data is missing near the coast because it cannot observe. Daytime data was observed around 1:30pm local time in each area, and nighttime data was observed around 1:30am local time in each area. This data uses a 5-day moving average to reduce missing values due to outside the observation range, etc.

- JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SST.daytime.v4_global_daily
- JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SST.nighttime.v4_global_daily

## Chlorophyll-a Concentration

Chlorophyll-a concentration (CHLA) observed by the Global Change Observation Mission - Climate "SHIKISAI" (GCOM-C). It shows the distribution of phytoplankton. It is characterized by its ability to observe up to the coast with a spatial resolution of about 250m. When there are clouds, observations are not possible and data is missing. JAXA Earth API publishes global data with a spatial resolution of about 250m.

- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_monthly
- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_half-monthly
- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_daily

## Normalized Difference Vegetation Index

Normalized difference vegetation index (NDVI) observed by the Global Change Observation Mission - Climate "SHIKISAI" (GCOM-C). It shows the distribution and health of plants (leaves). When there are clouds, observations are not possible and data is missing. JAXA Earth API publishes data with a spatial resolution of about 5km.

- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_monthly
- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_half-monthly
- JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_daily

## Soil Moisture Content

Soil moisture content (SMC) observed by the Global Change Observation Mission - Water "SHIZUKU" (GCOM-W).

- JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_monthly
- JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_monthly
- JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_daily
- JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_daily

## Sea Ice Concentration
Sea Ice Concentration (IC) observed by the Global Change Observation Mission - Water "SHIZUKU" (GCOM-W).

- JAXA.JASMES_GCOM-W.AMSR2_ic0.v201_north_daily
- JAXA.JASMES_GCOM-W.AMSR2_ic0.v201_south_daily

# How to create useful related URLs

Detailed information about each dataset can be accessed by replacing <id> with the id in the following URL format:
https://data.earth.jaxa.jp/en/datasets/#/id/<id>

If you want to check the STAC/COG file structure in more detail, access the following URL by replacing <collection> with the URL of collection.json of the dataset.
https://data.earth.jaxa.jp/app/stac/v2/?url=<collection>

To open a simple map display, replace the following <collection> and <band> with those of each dataset and access the URL created. <band>, <date>, <lng>, <lat>, and <zoom> are optional.
https://data.earth.jaxa.jp/app/viewer/v2/?collection=<collection>&band=<band>&date=<date>&lng=<lng>&lat=<lat>&zoom=<zoom>

Example: Display the topography around Mount Fuji.
https://data.earth.jaxa.jp/app/viewer/v2/?collection=https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_ALOS.PRISM_AW3D30.v4.1_global/collection.json&band=DSM&date=2024-04-01T00:00:00.000Z&lng=138.7384&lat=35.3660&zoom=10.5

Detailed information about all datasets can be accessed by the following URL:
https://data.earth.jaxa.jp/en/datasets/

Detailed information about the JAXA Earth API can be accessed by the following URL:
https://data.earth.jaxa.jp/

# Dataset List

Below is a detailed description of all the data.
The explanation of each parameter is as follows:

- id: The ID used to uniquely identify the dataset.
- collection: The URL to the dataset's collection.json.
- title: The title of the dataset.
- description: A description of the dataset.
- keywords: Keywords associated with the dataset, such as the name of the satellite that observed the data or the name of the space agency that manages it.
- dataProvider: This represents a name of the data provider.
- bands: The IDs of the data included in the dataset. If multiple, they are separated by commas (,).
- bbox: The geographic extent (bounding box) of the dataset. For EPSG:4326: [min longitude, min latitude, max longitude, max latitude]. For EPSG:3995 or EPSG:3031: [min X, min Y, max X, max Y].
- epsg: The EPSG code representing the projection of the dataset.
- spatialResolution: The approximate spatial resolution of the dataset (in kilometers). A smaller value indicates higher spatial resolution, meaning finer details can be distinguished.
- startDate: A string in ISO8601 format representing the start date of the dataset's time range. It indicates that data is available from this date onward.
- endDate: A string in ISO8601 format representing the end date of the dataset's time range. If the value is 'present', it means the dataset is still being updated daily.
- temporalResolution: This represents the temporal resolution of the dataset.
- normal: If true, it represents the normal value (climate value), i.e., the average value for a certain period in the past (the period written in startDate to endDate).

## Land Cover Class (Yearly)
* id: Copernicus.C3S_PROBA-V_LCCS_global_yearly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/Copernicus.C3S_PROBA-V_LCCS_global_yearly/collection.json
* title: Land Cover Class (Yearly)
* description: The 300m CCI-LC Maps (22 LCCS classes) were obtained from the processing of the full archives of 300m MERIS, 1km SPOT-VEGETATION, 1km PROBA-V and 1km AVHRR.
* keywords: Landcover, VGT-P, PROBA-V, ESA
* dataProvider: Copernicus C3S
* bands: LCCS
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2019-01-01T00:00:00Z
* endDate: 2019-12-31T23:59:59Z
* temporalResolution: yearly
* normal: false

## Forest Non Forest map
* id: JAXA.EORC_ALOS-2.PALSAR-2_FNF.v2.1.0_global_yearly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_ALOS-2.PALSAR-2_FNF.v2.1.0_global_yearly/collection.json
* title: Forest Non Forest map
* description: The global 25-meter resolution PALSAR-2 Forest/Non-forest map (FNF) is based on data acquired by the L-band Synthetic Aperture Radar (PALSAR-2) onboard the Advanced Land Observing Satellite-2 (ALOS-2). The PALSAR-2 Forest/Non-forest map (FNF) is a data set that shows the global forest distribution. The L-band SAR backscatter coefficient is sensitive to vegetation, especially in forests. Large values indicate forested areas, while small values indicate non-forested areas and water bodies. The Earth Observation Research Center of the Japan Aerospace Exploration Agency (JAXA) has been working to create a global forest distribution map by classifying images based on these characteristics. The purpose of this dataset is to make the results available to the public and to use them to understand forest distribution and its time-series changes.
* keywords: Forest, ALOS, JAXA
* dataProvider: JAXA EORC
* bands: FNF
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.031
* startDate: 2017-01-01T00:00:00Z
* endDate: present
* temporalResolution: yearly
* normal: false

## Digital Surface Model
* id: JAXA.EORC_ALOS.PRISM_AW3D30.v3.2_global
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_ALOS.PRISM_AW3D30.v3.2_global/collection.json
* title: Digital Surface Model
* description: JAXA released 'ALOS World 3D-30m' (AW3D30) dataset, the global digital surface model (DSM) dataset with a horizontal resolution of approximately 30-meter mesh (1 arcsec. latitude and longitude) generated from 5m resolution DSM,free of charge in May 20164). Void height values in cloud and show pixels between 60-degree North and 60-degree South are filled with existing DEMs using the Delta Surface Fill*2 method in the update in March 2017. This dataset is highly expected to be used in scientific research and geospatial information application services.
* keywords: Elevation, ALOS, JAXA
* dataProvider: JAXA EORC
* bands: DSM, MSK
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.031
* startDate: 2021-02-01T00:00:00Z
* endDate: 2021-02-28T23:59:59Z
* temporalResolution: none
* normal: false

## ALOS World 3D - 30m (AW3D30) (Released in April 2024 / Version 4.1)
* id: JAXA.EORC_ALOS.PRISM_AW3D30.v4.1_global
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_ALOS.PRISM_AW3D30.v4.1_global/collection.json
* title: ALOS World 3D - 30m (AW3D30) (Released in April 2024 / Version 4.1)
* description: JAXA released 'ALOS World 3D-30m' (AW3D30) dataset is a global digital surface model (DSM) with horizontal resolution of approximately 30 meters (basically 1 arcsecond) by the Panchromatic Remote-sensing Instrument for Stereo Mapping (PRISM), which was an optical sensor on board the Advanced Land Observing Satellite 'ALOS'. Any of the commercial and non-commercial purposes can be used free of charge under the Terms of Use (https://earth.jaxa.jp/en/data/policy/).
* keywords: Elevation, ALOS, JAXA
* dataProvider: JAXA EORC
* bands: DSM, MSK
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.031
* startDate: 2024-04-01T00:00:00Z
* endDate: 2024-04-30T23:59:59Z
* temporalResolution: null
* normal: false

## High-Resolution Land-Use and Land-Cover Map of Japan [2024, 2022, 2020] (Released in April 2025 / Version 25.04)
* id: JAXA.EORC_ALOS_HRLULC.v25.04_japan
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_ALOS_HRLULC.v25.04_japan/collection.json
* title: High-Resolution Land-Use and Land-Cover Map of Japan [2024, 2022, 2020] (Released in April 2025 / Version 25.04)
* description: Japan Aerospace Exploration Agency (JAXA) Earth Observation Research Center (EORC) creates High-Resolution Land Cover classification Maps (HRLULC) at the regional and national levels by using Earth observation data by satellites. These are basic information for regional and national land conservation such as ecosystem evaluation (growth and habitat of animals and plants, various ecosystem services), resource management (agriculture, forestry and fisheries, landscapes, etc.), disaster countermeasures (floods, sediment disasters, etc.), and so on.
* keywords: LULC, Sentinel-2, ALOS-2, Landsat-8, ALOS, JAXA
* dataProvider: JAXA EORC
* bands: HRLULC
* bbox: [122,20,154,46]
* epsg: 4326
* spatialResolution: 0.012
* startDate: 2020-01-01T00:00:00Z
* endDate: 2024-12-31T23:59:59Z
* temporalResolution: null
* normal: false

## Extreme Rainfall (Daily)
* id: JAXA.EORC_GSMaP_NRT.Gauge.Ext.00Z-23Z.v6_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_GSMaP_NRT.Gauge.Ext.00Z-23Z.v6_daily/collection.json
* title: Extreme Rainfall (Daily)
* description: GSMaP has multi-satellite global precipitation map under the Global Precipitation Measurement (GPM) Mission, by using Dual-frequency Precipitation Radar (DPR) onboard GPM core satellites, other GPM constellation satellites, and Geostationary satellites.
* keywords: Extreme Rainfall, NRT, GSMaP, GPM, JAXA
* dataProvider: JAXA EORC
* bands: EXT
* bbox: [-180,-60,180,60]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2000-03-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Precipitation Rate (Daily)
* id: JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_daily/collection.json
* title: Precipitation Rate (Daily)
* description: GSMaP has multi-satellite global precipitation map under the Global Precipitation Measurement (GPM) Mission, by using Dual-frequency Precipitation Radar (DPR) onboard GPM core satellites, other GPM constellation satellites, and Geostationary satellites.
* keywords: Precipitation , GSMaP, GPM, JAXA
* dataProvider: JAXA EORC
* bands: PRECIP
* bbox: [-180,-60,180,60]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2000-03-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Precipitation Rate (Half-monthly-normal)
* id: JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_half-monthly-normal/collection.json
* title: Precipitation Rate (Half-monthly-normal)
* description: GSMaP has multi-satellite global precipitation map under the Global Precipitation Measurement (GPM) Mission, by using Dual-frequency Precipitation Radar (DPR) onboard GPM core satellites, other GPM constellation satellites, and Geostationary satellites.
* keywords: Precipitation, DPR, GPM, NASA, JAXA
* dataProvider: JAXA EORC
* bands: PRECIP_2012_2021
* bbox: [-180,-60,180,60]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Precipitation Rate (Half-monthly)
* id: JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_half-monthly/collection.json
* title: Precipitation Rate (Half-monthly)
* description: GSMaP has multi-satellite global precipitation map under the Global Precipitation Measurement (GPM) Mission, by using Dual-frequency Precipitation Radar (DPR) onboard GPM core satellites, other GPM constellation satellites, and Geostationary satellites.
* keywords: Precipitation , GSMaP, GPM, JAXA
* dataProvider: JAXA EORC
* bands: PRECIP
* bbox: [-180,-60,180,60]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2000-03-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Precipitation Rate (Monthly-normal)
* id: JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_monthly-normal/collection.json
* title: Precipitation Rate (Monthly-normal)
* description: GSMaP has multi-satellite global precipitation map under the Global Precipitation Measurement (GPM) Mission, by using Dual-frequency Precipitation Radar (DPR) onboard GPM core satellites, other GPM constellation satellites, and Geostationary satellites.
* keywords: Precipitation, DPR, GPM, NASA, JAXA
* dataProvider: JAXA EORC
* bands: PRECIP_2012_2021
* bbox: [-180,-60,180,60]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Precipitation Rate (Monthly)
* id: JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_GSMaP_standard.Gauge.00Z-23Z.v6_monthly/collection.json
* title: Precipitation Rate (Monthly)
* description: GSMaP has multi-satellite global precipitation map under the Global Precipitation Measurement (GPM) Mission, by using Dual-frequency Precipitation Radar (DPR) onboard GPM core satellites, other GPM constellation satellites, and Geostationary satellites.
* keywords: Precipitation , GSMaP, GPM, JAXA
* dataProvider: JAXA EORC
* bands: PRECIP
* bbox: [-180,-60,180,60]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2000-03-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Precipitation Rate (Hourly)
* id: JAXA.EORC_GSMaP_standard.Gauge.v6_hourly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.EORC_GSMaP_standard.Gauge.v6_hourly/collection.json
* title: Precipitation Rate (Hourly)
* description: GSMaP has multi-satellite global precipitation map under the Global Precipitation Measurement (GPM) Mission, by using Dual-frequency Precipitation Radar (DPR) onboard GPM core satellites, other GPM constellation satellites, and Geostationary satellites.
* keywords: Precipitation , GSMaP, GPM, JAXA
* dataProvider: JAXA EORC
* bands: PRECIP
* bbox: [-180,-60,180,60]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2000-03-01T00:00:00Z
* endDate: present
* temporalResolution: null
* normal: false

## Soil Moisture Content (Daytime/Daily)
* id: JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.daytime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.daytime.v3_global_daily/collection.json
* title: Soil Moisture Content (Daytime/Daily)
* description: Soil Moisture Content product in Aqua MODIS Level-3 Standard product Version 3.
* keywords: SMC, AMSR-E, Aqua, NASA, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2002-06-01T00:00:00Z
* endDate: 2011-10-04T23:59:59Z
* temporalResolution: daily
* normal: false

## Soil Moisture Content (Daytime/Half-monthly)
* id: JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.daytime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.daytime.v3_global_half-monthly/collection.json
* title: Soil Moisture Content (Daytime/Half-monthly)
* description: Soil Moisture Content product in Aqua MODIS Level-3 Standard product Version 3.
* keywords: SMC, AMSR-E, Aqua, NASA, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2002-06-01T00:00:00Z
* endDate: 2011-10-04T23:59:59Z
* temporalResolution: half-monthly
* normal: false

## Soil Moisture Content (Daytime/Monthly)
* id: JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.daytime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.daytime.v3_global_monthly/collection.json
* title: Soil Moisture Content (Daytime/Monthly)
* description: Soil Moisture Content product in Aqua MODIS Level-3 Standard product Version 3.
* keywords: SMC, AMSR-E, Aqua, NASA, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2002-06-01T00:00:00Z
* endDate: 2011-10-04T23:59:59Z
* temporalResolution: monthly
* normal: false

## Soil Moisture Content (Nighttime/Daily)
* id: JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.nighttime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.nighttime.v3_global_daily/collection.json
* title: Soil Moisture Content (Nighttime/Daily)
* description: Soil Moisture Content product in Aqua MODIS Level-3 Standard product Version 3.
* keywords: SMC, AMSR-E, Aqua, NASA, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2002-06-01T00:00:00Z
* endDate: 2011-10-04T23:59:59Z
* temporalResolution: daily
* normal: false

## Soil Moisture Content (Nighttime/Half-monthly)
* id: JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.nighttime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.nighttime.v3_global_half-monthly/collection.json
* title: Soil Moisture Content (Nighttime/Half-monthly)
* description: Soil Moisture Content product in Aqua MODIS Level-3 Standard product Version 3.
* keywords: SMC, AMSR-E, Aqua, NASA, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2002-06-01T00:00:00Z
* endDate: 2011-10-04T23:59:59Z
* temporalResolution: half-monthly
* normal: false

## Soil Moisture Content (Nighttime/Monthly)
* id: JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.nighttime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_Aqua.AMSR-E_standard.L3-SMC.nighttime.v3_global_monthly/collection.json
* title: Soil Moisture Content (Nighttime/Monthly)
* description: Soil Moisture Content product in Aqua MODIS Level-3 Standard product Version 3.
* keywords: SMC, AMSR-E, Aqua, NASA, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2002-06-01T00:00:00Z
* endDate: 2011-10-04T23:59:59Z
* temporalResolution: monthly
* normal: false

## Aerosol Optical Depth at 500 nm (Daytime/Daily)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-AROT.daytime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-AROT.daytime.v3_global_daily/collection.json
* title: Aerosol Optical Depth at 500 nm (Daytime/Daily)
* description: Aerosol optical thickness product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: AROT, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: AROT
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Aerosol Optical Depth at 500 nm (Daytime/Half-monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-AROT.daytime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-AROT.daytime.v3_global_half-monthly/collection.json
* title: Aerosol Optical Depth at 500 nm (Daytime/Half-monthly)
* description: Aerosol optical thickness product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: AROT, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: AROT
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Aerosol Optical Depth at 500 nm (Daytime/Monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-AROT.daytime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-AROT.daytime.v3_global_monthly/collection.json
* title: Aerosol Optical Depth at 500 nm (Daytime/Monthly)
* description: Aerosol optical thickness product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: AROT, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: AROT
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Chlorophyll-a concentration (Daytime/Daily)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_daily/collection.json
* title: Chlorophyll-a concentration (Daytime/Daily)
* description: Chlorophyll-a concentration product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: CHLA, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: CHLA
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Chlorophyll-a concentration (Daytime/Half-monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_half-monthly/collection.json
* title: Chlorophyll-a concentration (Daytime/Half-monthly)
* description: Chlorophyll-a concentration product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: CHLA, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: CHLA
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Chlorophyll-a concentration (Daytime/Monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-CHLA.daytime.v3_global_monthly/collection.json
* title: Chlorophyll-a concentration (Daytime/Monthly)
* description: Chlorophyll-a concentration product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: CHLA, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: CHLA
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Land Surface Temperature (Daytime/Daily)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_daily/collection.json
* title: Land Surface Temperature (Daytime/Daily)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Land Surface Temperature (Daytime/Half-monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_half-monthly/collection.json
* title: Land Surface Temperature (Daytime/Half-monthly)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Land Surface Temperature (Daytime/Monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_monthly/collection.json
* title: Land Surface Temperature (Daytime/Monthly)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Land Surface Temperature (Nighttime/Daily)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_daily/collection.json
* title: Land Surface Temperature (Nighttime/Daily)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Land Surface Temperature (Nighttime/Half-monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_half-monthly/collection.json
* title: Land Surface Temperature (Nighttime/Half-monthly)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Land Surface Temperature (Nighttime/Monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_monthly/collection.json
* title: Land Surface Temperature (Nighttime/Monthly)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Normalized Difference Vegetation Index (Daytime/Daily)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_daily/collection.json
* title: Normalized Difference Vegetation Index (Daytime/Daily)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: NDVI, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: NDVI
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Normalized Difference Vegetation Index (Daytime/Half-monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_half-monthly/collection.json
* title: Normalized Difference Vegetation Index (Daytime/Half-monthly)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: NDVI, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: NDVI
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Normalized Difference Vegetation Index (Daytime/Monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-NDVI.daytime.v3_global_monthly/collection.json
* title: Normalized Difference Vegetation Index (Daytime/Monthly)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: NDVI, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: NDVI
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 2.783
* startDate: 2018-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Sea surface temperature (Daytime/Daily)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.daytime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.daytime.v3_global_daily/collection.json
* title: Sea surface temperature (Daytime/Daily)
* description: Sea surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: SST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Sea surface temperature (Daytime/Half-monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.daytime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.daytime.v3_global_half-monthly/collection.json
* title: Sea surface temperature (Daytime/Half-monthly)
* description: Sea surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: SST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Sea surface temperature (Daytime/Monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.daytime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.daytime.v3_global_monthly/collection.json
* title: Sea surface temperature (Daytime/Monthly)
* description: Sea surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: SST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Sea surface temperature (Nighttime/Daily)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.nighttime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.nighttime.v3_global_daily/collection.json
* title: Sea surface temperature (Nighttime/Daily)
* description: Sea surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: SST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Sea surface temperature (Nighttime/Half-monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.nighttime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.nighttime.v3_global_half-monthly/collection.json
* title: Sea surface temperature (Nighttime/Half-monthly)
* description: Sea surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: SST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Sea surface temperature (Nighttime/Monthly)
* id: JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.nighttime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-C.SGLI_standard.L3-SST.nighttime.v3_global_monthly/collection.json
* title: Sea surface temperature (Nighttime/Monthly)
* description: Sea surface temperature product in GCOM-C SGLI Level-3 Standard product Version 3.
* keywords: SST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA G-Portal
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2024-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Soil Moisture Content (Daytime/Daily)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_daily/collection.json
* title: Soil Moisture Content (Daytime/Daily)
* description: Soil Moisture Content product in GCOM-W AMSR2 Level-3 Standard product Version 3.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Soil Moisture Content (Daytime/Half-monthly-normal)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_half-monthly-normal/collection.json
* title: Soil Moisture Content (Daytime/Half-monthly-normal)
* description: Volumetric water content over global land areas including arid and cold regions, except areas covered by vegetation with 2kg/m2 water equivalent. Accuracy is defined as mean absolute error of instantaneous observations.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Soil Moisture Content (Daytime/Half-monthly)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_half-monthly/collection.json
* title: Soil Moisture Content (Daytime/Half-monthly)
* description: Soil Moisture Content product in GCOM-W AMSR2 Level-3 Standard product Version 3.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Soil Moisture Content (Daytime/Monthly-normal)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_monthly-normal/collection.json
* title: Soil Moisture Content (Daytime/Monthly-normal)
* description: Volumetric water content over global land areas including arid and cold regions, except areas covered by vegetation with 2kg/m2 water equivalent. Accuracy is defined as mean absolute error of instantaneous observations.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Soil Moisture Content (Daytime/Monthly)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.daytime.v3_global_monthly/collection.json
* title: Soil Moisture Content (Daytime/Monthly)
* description: Soil Moisture Content product in GCOM-W AMSR2 Level-3 Standard product Version 3.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Soil Moisture Content (Nighttime/Daily)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_daily/collection.json
* title: Soil Moisture Content (Nighttime/Daily)
* description: Soil Moisture Content product in GCOM-W AMSR2 Level-3 Standard product Version 3.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-02T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Soil Moisture Content (Nighttime/Half-monthly-normal)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_half-monthly-normal/collection.json
* title: Soil Moisture Content (Nighttime/Half-monthly-normal)
* description: Volumetric water content over global land areas including arid and cold regions, except areas covered by vegetation with 2kg/m2 water equivalent. Accuracy is defined as mean absolute error of instantaneous observations.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Soil Moisture Content (Nighttime/Half-monthly)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_half-monthly/collection.json
* title: Soil Moisture Content (Nighttime/Half-monthly)
* description: Soil Moisture Content product in GCOM-W AMSR2 Level-3 Standard product Version 3.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-02T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Soil Moisture Content (Nighttime/Monthly-normal)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_monthly-normal/collection.json
* title: Soil Moisture Content (Nighttime/Monthly-normal)
* description: Volumetric water content over global land areas including arid and cold regions, except areas covered by vegetation with 2kg/m2 water equivalent. Accuracy is defined as mean absolute error of instantaneous observations.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Soil Moisture Content (Nighttime/Monthly)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SMC.nighttime.v3_global_monthly/collection.json
* title: Soil Moisture Content (Nighttime/Monthly)
* description: Soil Moisture Content product in GCOM-W AMSR2 Level-3 Standard product Version 3.
* keywords: SMC, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SMC
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-02T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Sea Surface Wind Speed (Daytime/Daily)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.daytime.v4_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.daytime.v4_global_daily/collection.json
* title: Sea Surface Wind Speed (Daytime/Daily)
* description: Sea Surface Wind Speed product in GCOM-W AMSR2 Level-3 Standard product Version 4.
* keywords: SSW, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SSW
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Sea Surface Wind Speed (Daytime/Half-monthly)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.daytime.v4_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.daytime.v4_global_half-monthly/collection.json
* title: Sea Surface Wind Speed (Daytime/Half-monthly)
* description: Sea Surface Wind Speed product in GCOM-W AMSR2 Level-3 Standard product Version 4.
* keywords: SSW, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SSW
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Sea Surface Wind Speed (Daytime/Monthly)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.daytime.v4_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.daytime.v4_global_monthly/collection.json
* title: Sea Surface Wind Speed (Daytime/Monthly)
* description: Sea Surface Wind Speed product in GCOM-W AMSR2 Level-3 Standard product Version 4.
* keywords: SSW, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SSW
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Sea Surface Wind Speed (Nighttime/Daily)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.nighttime.v4_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.nighttime.v4_global_daily/collection.json
* title: Sea Surface Wind Speed (Nighttime/Daily)
* description: Sea Surface Wind Speed product in GCOM-W AMSR2 Level-3 Standard product Version 4.
* keywords: SSW, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SSW
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Sea Surface Wind Speed (Nighttime/Half-monthly)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.nighttime.v4_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.nighttime.v4_global_half-monthly/collection.json
* title: Sea Surface Wind Speed (Nighttime/Half-monthly)
* description: Sea Surface Wind Speed product in GCOM-W AMSR2 Level-3 Standard product Version 4.
* keywords: SSW, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SSW
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Sea Surface Wind Speed (Nighttime/Monthly)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.nighttime.v4_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SSW.nighttime.v4_global_monthly/collection.json
* title: Sea Surface Wind Speed (Nighttime/Monthly)
* description: Sea Surface Wind Speed product in GCOM-W AMSR2 Level-3 Standard product Version 4.
* keywords: SSW, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SSW
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 11.132
* startDate: 2012-07-03T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Shortwave Radiation (Daily)
* id: JAXA.JASMES_Aqua.MODIS_swr.v811_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Aqua.MODIS_swr.v811_global_daily/collection.json
* title: Shortwave Radiation (Daily)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-07-03T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Shortwave Radiation (Aqua MODIS/Half-monthly-normal)
* id: JAXA.JASMES_Aqua.MODIS_swr.v811_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Aqua.MODIS_swr.v811_global_half-monthly-normal/collection.json
* title: Shortwave Radiation (Aqua MODIS/Half-monthly-normal)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Shortwave Radiation (Half-monthly)
* id: JAXA.JASMES_Aqua.MODIS_swr.v811_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Aqua.MODIS_swr.v811_global_half-monthly/collection.json
* title: Shortwave Radiation (Half-monthly)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-07-03T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Shortwave Radiation (Aqua MODIS/Monthly-normal)
* id: JAXA.JASMES_Aqua.MODIS_swr.v811_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Aqua.MODIS_swr.v811_global_monthly-normal/collection.json
* title: Shortwave Radiation (Aqua MODIS/Monthly-normal)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Shortwave Radiation (Monthly)
* id: JAXA.JASMES_Aqua.MODIS_swr.v811_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Aqua.MODIS_swr.v811_global_monthly/collection.json
* title: Shortwave Radiation (Monthly)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-07-03T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## 8 days total evapotranspiration [mm/day]
* id: JAXA.JASMES_GCOM-C.SGLI_ET.v1_global_8-day
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_GCOM-C.SGLI_ET.v1_global_8-day/collection.json
* title: 8 days total evapotranspiration [mm/day]
* description: Kazuhisa TANADA, Masahiro TASUMI, Hiroshi MURAKAMI, Masao MORIYAMA, Yoshiyuki TAKAHASHI, Reiko IDE, Kentaro TAKAGI, Implementation and evaluation of a global evapotranspiration index algorithm based on land surface temperature, Journal of Agricultural Meteorology, 2026, Volume 82, Issue 1, Pages 15-30. doi:10.2480/agrmet.D-25-00022
* keywords: ET, GCOM-C, SGLI, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ET
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2018-01-17T00:00:00Z
* endDate: present
* temporalResolution: 8-day
* normal: false

## Half a month total evapotranspiration [mm/day]
* id: JAXA.JASMES_GCOM-C.SGLI_ET.v1_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_GCOM-C.SGLI_ET.v1_global_half-monthly/collection.json
* title: Half a month total evapotranspiration [mm/day]
* description: Kazuhisa TANADA, Masahiro TASUMI, Hiroshi MURAKAMI, Masao MORIYAMA, Yoshiyuki TAKAHASHI, Reiko IDE, Kentaro TAKAGI, Implementation and evaluation of a global evapotranspiration index algorithm based on land surface temperature, Journal of Agricultural Meteorology, 2026, Volume 82, Issue 1, Pages 15-30. doi:10.2480/agrmet.D-25-00022
* keywords: ET, GCOM-C, SGLI, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ET
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2018-01-17T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## 1 month total evapotranspiration [mm/day]
* id: JAXA.JASMES_GCOM-C.SGLI_ET.v1_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_GCOM-C.SGLI_ET.v1_global_monthly/collection.json
* title: 1 month total evapotranspiration [mm/day]
* description: Kazuhisa TANADA, Masahiro TASUMI, Hiroshi MURAKAMI, Masao MORIYAMA, Yoshiyuki TAKAHASHI, Reiko IDE, Kentaro TAKAGI, Implementation and evaluation of a global evapotranspiration index algorithm based on land surface temperature, Journal of Agricultural Meteorology, 2026, Volume 82, Issue 1, Pages 15-30. doi:10.2480/agrmet.D-25-00022
* keywords: ET, GCOM-C, SGLI, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ET
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2018-01-17T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## 8 days evapotranspiration index [N/A]
* id: JAXA.JASMES_GCOM-C.SGLI_ETidx.v1_global_8-day
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_GCOM-C.SGLI_ETidx.v1_global_8-day/collection.json
* title: 8 days evapotranspiration index [N/A]
* description: Kazuhisa TANADA, Masahiro TASUMI, Hiroshi MURAKAMI, Masao MORIYAMA, Yoshiyuki TAKAHASHI, Reiko IDE, Kentaro TAKAGI, Implementation and evaluation of a global evapotranspiration index algorithm based on land surface temperature, Journal of Agricultural Meteorology, 2026, Volume 82, Issue 1, Pages 15-30. doi:10.2480/agrmet.D-25-00022
* keywords: ET Index, GCOM-C, SGLI, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ETidx
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2018-01-17T00:00:00Z
* endDate: present
* temporalResolution: 8-day
* normal: false

## Half a month evapotranspiration index [N/A]
* id: JAXA.JASMES_GCOM-C.SGLI_ETidx.v1_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_GCOM-C.SGLI_ETidx.v1_global_half-monthly/collection.json
* title: Half a month evapotranspiration index [N/A]
* description: Kazuhisa TANADA, Masahiro TASUMI, Hiroshi MURAKAMI, Masao MORIYAMA, Yoshiyuki TAKAHASHI, Reiko IDE, Kentaro TAKAGI, Implementation and evaluation of a global evapotranspiration index algorithm based on land surface temperature, Journal of Agricultural Meteorology, 2026, Volume 82, Issue 1, Pages 15-30. doi:10.2480/agrmet.D-25-00022
* keywords: ET Index, GCOM-C, SGLI, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ETidx
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2018-01-17T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## 1 month evapotranspiration index [N/A]
* id: JAXA.JASMES_GCOM-C.SGLI_ETidx.v1_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_GCOM-C.SGLI_ETidx.v1_global_monthly/collection.json
* title: 1 month evapotranspiration index [N/A]
* description: Kazuhisa TANADA, Masahiro TASUMI, Hiroshi MURAKAMI, Masao MORIYAMA, Yoshiyuki TAKAHASHI, Reiko IDE, Kentaro TAKAGI, Implementation and evaluation of a global evapotranspiration index algorithm based on land surface temperature, Journal of Agricultural Meteorology, 2026, Volume 82, Issue 1, Pages 15-30. doi:10.2480/agrmet.D-25-00022
* keywords: ET Index, GCOM-C, SGLI, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ETidx
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2018-01-17T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Sea Ice Concentration (Daily)
* id: JAXA.JASMES_GCOM-W.AMSR2_ic0.v201_north_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_GCOM-W.AMSR2_ic0.v201_north_daily/collection.json
* title: Sea Ice Concentration (Daily)
* description: Sea ice concentrarion products generated by JAXA EORC based on AMSR2 data and published in data provision site, JASMES
* keywords: ic0, AMSR2, GCOM-W, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: IC0
* bbox: [-8388608,-8388608,8388608,8388608]
* epsg: 3995
* spatialResolution: 8.192
* startDate: 2012-07-02T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Sea Ice Concentration (Daily)
* id: JAXA.JASMES_GCOM-W.AMSR2_ic0.v201_south_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_GCOM-W.AMSR2_ic0.v201_south_daily/collection.json
* title: Sea Ice Concentration (Daily)
* description: Sea ice concentrarion products generated by JAXA EORC based on AMSR2 data and published in data provision site, JASMES
* keywords: ic0, AMSR2, GCOM-W, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: IC0
* bbox: [-8388608,-8388608,8388608,8388608]
* epsg: 3031
* spatialResolution: 8.192
* startDate: 2012-07-02T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Normalized Difference Vegetation Index (Half-monthly-normal)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_ndvi.v811_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_ndvi.v811_global_half-monthly-normal/collection.json
* title: Normalized Difference Vegetation Index (Half-monthly-normal)
* description: NDVI products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: NDVI, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ndvi_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Normalized Difference Vegetation Index (Half-monthly)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_ndvi.v811_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_ndvi.v811_global_half-monthly/collection.json
* title: Normalized Difference Vegetation Index (Half-monthly)
* description: NDVI products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: NDVI, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ndvi
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Normalized Difference Vegetation Index (Monthly-normal)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_ndvi.v811_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_ndvi.v811_global_monthly-normal/collection.json
* title: Normalized Difference Vegetation Index (Monthly-normal)
* description: NDVI products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: NDVI, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ndvi_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Normalized Difference Vegetation Index (Monthly)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_ndvi.v811_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_ndvi.v811_global_monthly/collection.json
* title: Normalized Difference Vegetation Index (Monthly)
* description: NDVI products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: NDVI, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: ndvi
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Aerosol Optical Depth at 500 nm (Daily)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_daily/collection.json
* title: Aerosol Optical Depth at 500 nm (Daily)
* description: TAUA products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: AOD, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: T500
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2013-08-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Aerosol Optical Depth at 500 nm (Terra & Aqua MODIS average/Half-monthly-normal)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_half-monthly-normal/collection.json
* title: Aerosol Optical Depth at 500 nm (Terra & Aqua MODIS average/Half-monthly-normal)
* description: TAUA products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: AOD, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: taua_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Aerosol Optical Depth at 500 nm (Half-monthly)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_half-monthly/collection.json
* title: Aerosol Optical Depth at 500 nm (Half-monthly)
* description: TAUA products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: AOD, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: T500
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Aerosol Optical Depth at 500 nm (Terra & Aqua MODIS average/Monthly-normal)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_monthly-normal/collection.json
* title: Aerosol Optical Depth at 500 nm (Terra & Aqua MODIS average/Monthly-normal)
* description: TAUA products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: AOD, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: taua_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Aerosol Optical Depth at 500 nm (Monthly)
* id: JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS-Aqua.MODIS_taua.v811_global_monthly/collection.json
* title: Aerosol Optical Depth at 500 nm (Monthly)
* description: TAUA products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: AOD, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: T500
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Shortwave Radiation (Daily)
* id: JAXA.JASMES_Terra.MODIS_swr.v811_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS_swr.v811_global_daily/collection.json
* title: Shortwave Radiation (Daily)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-07-03T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Shortwave Radiation (Terra MODIS/Half-monthly-normal)
* id: JAXA.JASMES_Terra.MODIS_swr.v811_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS_swr.v811_global_half-monthly-normal/collection.json
* title: Shortwave Radiation (Terra MODIS/Half-monthly-normal)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Shortwave Radiation (Half-monthly)
* id: JAXA.JASMES_Terra.MODIS_swr.v811_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS_swr.v811_global_half-monthly/collection.json
* title: Shortwave Radiation (Half-monthly)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-01-01T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Shortwave Radiation (Terra MODIS/Monthly-normal)
* id: JAXA.JASMES_Terra.MODIS_swr.v811_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS_swr.v811_global_monthly-normal/collection.json
* title: Shortwave Radiation (Terra MODIS/Monthly-normal)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Shortwave Radiation (Monthly)
* id: JAXA.JASMES_Terra.MODIS_swr.v811_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.JASMES_Terra.MODIS_swr.v811_global_monthly/collection.json
* title: Shortwave Radiation (Monthly)
* description: SWR products generated by JAXA EORC based on MODIS data and published in data provision site, JASMES
* keywords: SWR, MODIS, JASMES, JAXA
* dataProvider: JAXA JASMES
* bands: swr
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2002-01-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Floodplain Water Depth Hourly Japan [m]
* id: JAXA.UTokyo_TE_MSM.FLDDPH.v2_japan_hourly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.UTokyo_TE_MSM.FLDDPH.v2_japan_hourly/collection.json
* title: Floodplain Water Depth Hourly Japan [m]
* description: Floodplain water depth (unit: m) estimated by the hydrological simulation system "Today's Earth - Japan". It represents the depth of the floodplain water that overflows from river channels (distance from river water surface to floodplain) defined within each 1/60-degree latitude-longitude grid of the model. It can be used to monitor water depth on floodplains during flood events.
* keywords: Floodplain water depth, Japan, Today's Earth, UTokyo, JAXA
* dataProvider: JAXA UTokyo
* bands: FLDDPH
* bbox: [123,24,148,46]
* epsg: 4326
* spatialResolution: 1.237
* startDate: 2019-11-29T00:00:00Z
* endDate: present
* temporalResolution: null
* normal: false

## Flood Fraction Hourly Japan [N/A]
* id: JAXA.UTokyo_TE_MSM.FLDFRC.v2_japan_hourly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.UTokyo_TE_MSM.FLDFRC.v2_japan_hourly/collection.json
* title: Flood Fraction Hourly Japan [N/A]
* description: Fraction of the flooded area to the unit-catchment area of each grid cell (unit: m2/m2) estimated by the hydrological simulation "Today's Earth - Japan". It is data downscaled to a 15-arcsec grid along the detailed unit-catchment shapes defined within each 1/60-deg. latitude-longitude grid of the system. It can be used to monitor the spatial extent of river flooding.
* keywords: Flood fraction, Japan, Today's Earth, UTokyo, JAXA
* dataProvider: JAXA UTokyo
* bands: FLDFRC
* bbox: [123,24,148,46]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2019-11-29T00:00:00Z
* endDate: present
* temporalResolution: null
* normal: false

## Floodplain Flow Hourly Japan [m^3/s]
* id: JAXA.UTokyo_TE_MSM.FLDOUT.v2_japan_hourly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.UTokyo_TE_MSM.FLDOUT.v2_japan_hourly/collection.json
* title: Floodplain Flow Hourly Japan [m^3/s]
* description: Floodplain flow (unit: m3/s) estimated by the hydrological simulation system "Today's Earth - Japan". It represents the amount of water that overflows from river channels defined within each 1/60-degree latitude-longitude grid of the model and flows across the floodplain.
* keywords: Floodplain flow, Japan, Today's Earth, UTokyo, JAXA
* dataProvider: JAXA UTokyo
* bands: FLDOUT
* bbox: [123,24,148,46]
* epsg: 4326
* spatialResolution: 1.237
* startDate: 2019-11-29T00:00:00Z
* endDate: present
* temporalResolution: null
* normal: false

## Total Discharge Hourly Japan [m^3/s]
* id: JAXA.UTokyo_TE_MSM.OUTFLW.v2_japan_hourly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.UTokyo_TE_MSM.OUTFLW.v2_japan_hourly/collection.json
* title: Total Discharge Hourly Japan [m^3/s]
* description: Total river flow (River channel flow+Floodplain flow; unit: m3/s) estimated by the hydrological simulation system "Today's Earth - Japan". It represents the total amount of water flowing through rivers defined within each 1/60-degree latitude-longitude grid of the model. It can be used to continuously monitor total river flow from normal conditions through flood events.
* keywords: Total discharge, Japan, Today's Earth, UTokyo, JAXA
* dataProvider: JAXA UTokyo
* bands: OUTFLW
* bbox: [123,24,148,46]
* epsg: 4326
* spatialResolution: 1.237
* startDate: 2019-11-29T00:00:00Z
* endDate: present
* temporalResolution: null
* normal: false

## River Water Depth Hourly Japan [m]
* id: JAXA.UTokyo_TE_MSM.RIVDPH.v2_japan_hourly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.UTokyo_TE_MSM.RIVDPH.v2_japan_hourly/collection.json
* title: River Water Depth Hourly Japan [m]
* description: River channel water depth (unit: m) estimated by the hydrological simulation system "Today's Earth - Japan". It represents the depth of the river water (distance from river water surface to the river bed) defined within each 1/60-degree latitude-longitude grid of the model. It can be used to continuously monitor river channel water depth from normal conditions through flood events.
* keywords: River water depth, Japan, Today's Earth, UTokyo, JAXA
* dataProvider: JAXA UTokyo
* bands: RIVDPH
* bbox: [123,24,148,46]
* epsg: 4326
* spatialResolution: 1.237
* startDate: 2019-11-29T00:00:00Z
* endDate: present
* temporalResolution: null
* normal: false

## River Discharge Hourly Japan [m^3/s]
* id: JAXA.UTokyo_TE_MSM.RIVOUT.v2_japan_hourly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.UTokyo_TE_MSM.RIVOUT.v2_japan_hourly/collection.json
* title: River Discharge Hourly Japan [m^3/s]
* description: River channel flow (unit: m3/s) estimated by the hydrological simulation system "Today's Earth - Japan". It represents the amount of water flowing through rectangular-shaped river channels defined within each 1/60-degree latitude-longitude grid of the model.
* keywords: River discharge, Japan, Today's Earth, UTokyo, JAXA
* dataProvider: JAXA UTokyo
* bands: RIVOUT
* bbox: [123,24,148,46]
* epsg: 4326
* spatialResolution: 1.237
* startDate: 2019-11-29T00:00:00Z
* endDate: present
* temporalResolution: null
* normal: false

## Return Period of River Water Depth Hourly Japan [year]
* id: JAXA.UTokyo_TE_MSM.RPRIVDPH.v2_japan_hourly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/JAXA.UTokyo_TE_MSM.RPRIVDPH.v2_japan_hourly/collection.json
* title: Return Period of River Water Depth Hourly Japan [year]
* description: Return period of river channel water depth (unit: year) estimated by the hydrological simulation system "Today's Earth - Japan". The return period represents the average interval between events of a given magnitude (e.g., a 100-year return period means that such an event is expected to occur once every 100 years on average). It is data downscaled to a 15-arcsec grid along the detailed unit-catchment shapes defined within each 1/60-deg. lat-lon grid of the system. It can be used as an indicator of flood risk.
* keywords: Return period of river water depth, Japan, Today's Earth, UTokyo, JAXA
* dataProvider: JAXA UTokyo
* bands: RPRIVDPH
* bbox: [123,24,148,46]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2019-11-29T00:00:00Z
* endDate: present
* temporalResolution: null
* normal: false

## Land Surface Temperature (Aqua MODIS/Daytime/Half-monthly-normal)
* id: NASA.EOSDIS_Aqua.MODIS_MYD11C1-LST.daytime.v061_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Aqua.MODIS_MYD11C1-LST.daytime.v061_global_half-monthly-normal/collection.json
* title: Land Surface Temperature (Aqua MODIS/Daytime/Half-monthly-normal)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Land Surface Temperature (Aqua MODIS/Daytime/Monthly-normal)
* id: NASA.EOSDIS_Aqua.MODIS_MYD11C1-LST.daytime.v061_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Aqua.MODIS_MYD11C1-LST.daytime.v061_global_monthly-normal/collection.json
* title: Land Surface Temperature (Aqua MODIS/Daytime/Monthly-normal)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Land Surface Temperature (Aqua MODIS/Nighttime/Half-monthly-normal)
* id: NASA.EOSDIS_Aqua.MODIS_MYD11C1-LST.nighttime.v061_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Aqua.MODIS_MYD11C1-LST.nighttime.v061_global_half-monthly-normal/collection.json
* title: Land Surface Temperature (Aqua MODIS/Nighttime/Half-monthly-normal)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Land Surface Temperature (Aqua MODIS/Nighttime/Monthly-normal)
* id: NASA.EOSDIS_Aqua.MODIS_MYD11C1-LST.nighttime.v061_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Aqua.MODIS_MYD11C1-LST.nighttime.v061_global_monthly-normal/collection.json
* title: Land Surface Temperature (Aqua MODIS/Nighttime/Monthly-normal)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Land Surface Temperature (Daytime/Daily)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.daytime.v061_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.daytime.v061_global_daily/collection.json
* title: Land Surface Temperature (Daytime/Daily)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, EOSDIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-02-27T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Land Surface Temperature (Terra MODIS/Daytime/Half-monthly-normal)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.daytime.v061_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.daytime.v061_global_half-monthly-normal/collection.json
* title: Land Surface Temperature (Terra MODIS/Daytime/Half-monthly-normal)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Land Surface Temperature (Daytime/Half-monthly)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.daytime.v061_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.daytime.v061_global_half-monthly/collection.json
* title: Land Surface Temperature (Daytime/Half-monthly)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, EOSDIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-02-27T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Land Surface Temperature (Terra MODIS/Daytime/Monthly-normal)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.daytime.v061_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.daytime.v061_global_monthly-normal/collection.json
* title: Land Surface Temperature (Terra MODIS/Daytime/Monthly-normal)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Land Surface Temperature (Nighttime/Daily)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.nighttime.v061_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.nighttime.v061_global_daily/collection.json
* title: Land Surface Temperature (Nighttime/Daily)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, EOSDIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-02-27T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Land Surface Temperature (Terra MODIS/Nighttime/Half-monthly-normal)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.nighttime.v061_global_half-monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.nighttime.v061_global_half-monthly-normal/collection.json
* title: Land Surface Temperature (Terra MODIS/Nighttime/Half-monthly-normal)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: half-monthly
* normal: true

## Land Surface Temperature (Nighttime/Half-monthly)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.nighttime.v061_global_half-monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.nighttime.v061_global_half-monthly/collection.json
* title: Land Surface Temperature (Nighttime/Half-monthly)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, EOSDIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-02-27T00:00:00Z
* endDate: present
* temporalResolution: half-monthly
* normal: false

## Land Surface Temperature (Terra MODIS/Nighttime/Monthly-normal)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.nighttime.v061_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C1-LST.nighttime.v061_global_monthly-normal/collection.json
* title: Land Surface Temperature (Terra MODIS/Nighttime/Monthly-normal)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Daily L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C1.061
* keywords: LST, MODIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST_2012_2021
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2012-01-01T00:00:00Z
* endDate: 2021-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Land Surface Temperature (Daytime/Monthly)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C3-LST.daytime.v061_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C3-LST.daytime.v061_global_monthly/collection.json
* title: Land Surface Temperature (Daytime/Monthly)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Monthly L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C3.061
* keywords: LST, MODIS, EOSDIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-02-27T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Land Surface Temperature (Nighttime/Monthly)
* id: NASA.EOSDIS_Terra.MODIS_MOD11C3-LST.nighttime.v061_global_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds/cog/v1/NASA.EOSDIS_Terra.MODIS_MOD11C3-LST.nighttime.v061_global_monthly/collection.json
* title: Land Surface Temperature (Nighttime/Monthly)
* description: Wan, Z., Hook, S., Hulley, G. (2021). MODIS/Terra Land Surface Temperature/Emissivity Monthly L3 Global 0.05Deg CMG V061 [Data set]. NASA EOSDIS Land Processes DAAC. Accessed 2021-12-21 from https://doi.org/10.5067/MODIS/MOD11C3.061
* keywords: LST, MODIS, EOSDIS, NASA
* dataProvider: NASA EOSDIS
* bands: LST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-02-27T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## SPI - Standardized Precipitation Index (Monthly Averaged)
* id: JAXA.EORC_GSMaP_SPI.climate.gnrt6_monthly
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.EORC_GSMaP_SPI.climate.gnrt6_monthly/collection.json
* title: SPI - Standardized Precipitation Index (Monthly Averaged)
* description: GSMaP has multi-satellite global precipitation map under the Global Precipitation Measurement (GPM) Mission, by using Dual-frequency Precipitation Radar (DPR) onboard GPM core satellites, other GPM constellation satellites, and Geostationary satellites.
* keywords: SPI, DPR, GPM, GSMaP, JAXA
* dataProvider: JAXA EORC
* bands: SPI
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 22.264
* startDate: 2019-06-01T00:00:00Z
* endDate: present
* temporalResolution: monthly
* normal: false

## Sea Surface Temperature (Daytime/Daily)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SST.daytime.v4_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SST.daytime.v4_global_daily/collection.json
* title: Sea Surface Temperature (Daytime/Daily)
* description: Sea Surface Temperature (Daytime/Daily)
* keywords: SST, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 22.264
* startDate: 2000-03-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Sea Surface Temperature (Nighttime/Daily)
* id: JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SST.nighttime.v4_global_daily
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.G-Portal_GCOM-W.AMSR2_standard.L3-SST.nighttime.v4_global_daily/collection.json
* title: Sea Surface Temperature (Nighttime/Daily)
* description: Sea Surface Temperature (Nighttime/Daily)
* keywords: SST, AMSR2, GCOM-W, JAXA
* dataProvider: JAXA G-Portal
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 22.264
* startDate: 2000-03-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: false

## Land Surface Temperature (Daytime/8day-normal/Japan)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.daytime.v3_japan_8-day-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.daytime.v3_japan_8-day-normal/collection.json
* title: Land Surface Temperature (Daytime/8day-normal/Japan)
* description: Daily average land surface temperature was generated by Terra MODIS LST and SGLI LST
* keywords: LST, SGLI, GCOM-C, JAXA
* dataProvider: JAXA JASMES
* bands: LST_2000_2022
* bbox: [123,24,150,50]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2000-01-01T00:00:00Z
* endDate: 2022-12-31T23:59:09Z
* temporalResolution: 8-day
* normal: true

## Land surface temperature (Daytime/8-day/Japan)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.daytime.v3_japan_8-day
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.daytime.v3_japan_8-day/collection.json
* title: Land surface temperature (Daytime/8-day/Japan)
* description: Land surface temperature product in GCOM-C SGLI Level-2 Standard product
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA JASMES
* bands: LST_AVE
* bbox: [123,24,150,50]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2020-01-01T00:00:00Z
* endDate: present
* temporalResolution: 8-day
* normal: false

## Land Surface Temperature (Daytime/monthly-normal/Japan)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.daytime.v3_japan_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.daytime.v3_japan_monthly-normal/collection.json
* title: Land Surface Temperature (Daytime/monthly-normal/Japan)
* description: Average land surface temperature was generated by Terra MODIS LST and SGLI LST
* keywords: LST, SGLI, GCOM-C, JAXA
* dataProvider: JAXA JASMES
* bands: LST_2000_2022
* bbox: [123,24,150,50]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2000-01-01T00:00:00Z
* endDate: 2022-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Land Surface Temperature (Nighttime/8day-normal/Japan)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.nighttime.v3_japan_8-day-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.nighttime.v3_japan_8-day-normal/collection.json
* title: Land Surface Temperature (Nighttime/8day-normal/Japan)
* description: Daily average land surface temperature was generated by Terra MODIS LST and SGLI LST
* keywords: LST, SGLI, GCOM-C, JAXA
* dataProvider: JAXA JASMES
* bands: LST_2000_2022
* bbox: [123,24,150,50]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2000-01-01T00:00:00Z
* endDate: 2022-12-31T23:59:09Z
* temporalResolution: 8-day
* normal: true

## Land surface temperature (Nighttime/8-day/Japan)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.nighttime.v3_japan_8-day
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.nighttime.v3_japan_8-day/collection.json
* title: Land surface temperature (Nighttime/8-day/Japan)
* description: Land surface temperature product in GCOM-C SGLI Level-2 Standard product
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA JASMES
* bands: LST_AVE
* bbox: [123,24,150,50]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2020-01-01T00:00:00Z
* endDate: present
* temporalResolution: 8-day
* normal: false

## Land Surface Temperature (Nighttime/monthly-normal/Japan)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.nighttime.v3_japan_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L2-LST.nighttime.v3_japan_monthly-normal/collection.json
* title: Land Surface Temperature (Nighttime/monthly-normal/Japan)
* description: Average land surface temperature was generated by Terra MODIS LST and SGLI LST
* keywords: LST, SGLI, GCOM-C, JAXA
* dataProvider: JAXA JASMES
* bands: LST_2000_2022
* bbox: [123,24,150,50]
* epsg: 4326
* spatialResolution: 0.309
* startDate: 2000-01-01T00:00:00Z
* endDate: 2022-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Land Surface Temperature (Daytime/8day-normal/Global)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_8-day-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_8-day-normal/collection.json
* title: Land Surface Temperature (Daytime/8day-normal/Global)
* description: Daily average land surface temperature was generated by Terra MODIS LST and SGLI LST
* keywords: LST, SGLI, GCOM-C, JAXA
* dataProvider: JAXA JASMES
* bands: LST_2000_2022
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-01-01T00:00:00Z
* endDate: 2022-12-31T23:59:09Z
* temporalResolution: 8-day
* normal: true

## Land surface temperature (Daytime/8-day/Global)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_8-day
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_8-day/collection.json
* title: Land surface temperature (Daytime/8-day/Global)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA JASMES
* bands: LST_AVE
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2020-01-01T00:00:00Z
* endDate: present
* temporalResolution: 8-day
* normal: false

## Land Surface Temperature (Daytime/monthly-normal/Global)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.daytime.v3_global_monthly-normal/collection.json
* title: Land Surface Temperature (Daytime/monthly-normal/Global)
* description: Average land surface temperature was generated by Terra MODIS LST and SGLI LST
* keywords: LST, SGLI, GCOM-C, JAXA
* dataProvider: JAXA JASMES
* bands: LST_2000_2022
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-01-01T00:00:00Z
* endDate: 2022-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Land Surface Temperature (Nighttime/8day-normal/Global)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_8-day-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_8-day-normal/collection.json
* title: Land Surface Temperature (Nighttime/8day-normal/Global)
* description: Daily average land surface temperature was generated by Terra MODIS LST and SGLI LST
* keywords: LST, SGLI, GCOM-C, JAXA
* dataProvider: JAXA JASMES
* bands: LST_2000_2022
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-01-01T00:00:00Z
* endDate: 2022-12-31T23:59:09Z
* temporalResolution: 8-day
* normal: true

## Land surface temperature  (Nighttime/8-day/Global)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_8-day
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_8-day/collection.json
* title: Land surface temperature  (Nighttime/8-day/Global)
* description: Land surface temperature product in GCOM-C SGLI Level-3 Standard product
* keywords: LST, SGLI, GCOM-C, GCOM, JAXA
* dataProvider: JAXA JASMES
* bands: LST_AVE
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2020-01-01T00:00:00Z
* endDate: present
* temporalResolution: 8-day
* normal: false

## Land Surface Temperature (Nighttime/monthly-normal/Global)
* id: JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_monthly-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JAXA.JASMES_GCOM-C.SGLI_standard.L3-LST.nighttime.v3_global_monthly-normal/collection.json
* title: Land Surface Temperature (Nighttime/monthly-normal/Global)
* description: Average land surface temperature was generated by Terra MODIS LST and SGLI LST
* keywords: LST, SGLI, GCOM-C, JAXA
* dataProvider: JAXA JASMES
* bands: LST_2000_2022
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 5.566
* startDate: 2000-01-01T00:00:00Z
* endDate: 2022-12-31T23:59:09Z
* temporalResolution: monthly
* normal: true

## Sea Surface Temperature (Daily-normal)
* id: JMA_COBE-SST-interpolation_SST.v2_global_daily-normal
* collection: https://s3.ap-northeast-1.wasabisys.com/je-pds2/cog/v1/JMA_COBE-SST-interpolation_SST.v2_global_daily-normal/collection.json
* title: Sea Surface Temperature (Daily-normal)
* description: Sea Surface Temperature (Daily-normal)
* keywords: SST, COBE-SST, JMA, JAXA
* dataProvider: JMA
* bands: SST
* bbox: [-180,-90,180,90]
* epsg: 4326
* spatialResolution: 22.264
* startDate: 1991-01-01T00:00:00Z
* endDate: present
* temporalResolution: daily
* normal: true
