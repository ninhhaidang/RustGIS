# Web Data

**Home › Add › Web Data** gathers public web services and catalogs: the ones that cover the whole world
in one alphabetical list, and those that cover one country in a submenu named after it. Each
opens in the Web Data pane on the right, beside the map it searches. Unless a section says
otherwise, the panes work in the web app too, provided the servers allow browser requests
(CORS). The command-line examples below are for the desktop CLI; many network commands cannot
fetch synchronously in the browser, where the panes fetch in the background instead.
Commands that reach the network need the [AI Assistant](assistant.md)'s approval when it runs
them, and the network permission when a plugin does. `ui.dialog` opens a panel on a search with `"search"`
(`{"name": "nasaEarthdata", "search": "sea surface temperature"}`).

| Entry | What it gives | Section |
|---|---|---|
| ArcGIS Portal… | Public items on ArcGIS Online or another portal | [Web services](#web-services) |
| Boundaries… | A country's administrative boundaries (geoBoundaries) | [OpenStreetMap and administrative boundaries](#openstreetmap-and-administrative-boundaries) |
| Dynamical… | Weather and climate Zarr stores | [Web services](#web-services) |
| Earthdata GIS… | NASA's Earth science imagery, maps and features | [Web services](#web-services) |
| Fields of the World… | Agricultural field boundaries, worldwide | [Fields of the World](#fields-of-the-world) |
| GeoLens… | A GeoLens server's vector datasets | [Web services](#web-services) |
| Hugging Face… | Geospatial files in Hugging Face datasets | [Web services](#web-services) |
| NASA Earthaccess… | NASA Earthdata granules (ICESat-2, GEDI, HLS…) | [NASA Earthaccess](#nasa-earthaccess) |
| NASA Earthdata… | NASA GIBS imagery, day by day | [NASA Earthdata](#nasa-earthdata) |
| Natural Earth… | Natural Earth map data | [Natural Earth](#natural-earth) |
| Ocean Data Platform… | HUB Ocean's marine datasets | [Web services](#web-services) |
| OpenAerialMap… | Drone and aerial images | [Web services](#web-services) |
| OpenStreetMap… | OpenStreetMap features where the map is | [OpenStreetMap and administrative boundaries](#openstreetmap-and-administrative-boundaries) |
| Overture Maps… | Buildings, places, transportation and more | [Overture Maps](#overture-maps) |
| Planet Open Data… | Planet's disaster imagery | [Planet Open Data](#planet-open-data) |
| Portolan… | Open data catalogs published with Portolan | [Portolan](#portolan) |
| Satellite Embeddings… | AlphaEarth, Earth Index and Tessera embeddings | [Satellite embeddings](#satellite-embeddings) |
| Search Open Data… | data.gov, CKAN, Socrata and ArcGIS Hub datasets | [Open-data portals](#open-data-portals) |
| Sentinel-2 Explorer… | Recent, clear Sentinel-2 scenes | [Sentinel-2 Explorer](#sentinel-2-explorer) |
| Source Cooperative… | Files on Source Cooperative | [Source Cooperative](#source-cooperative) |
| STAC Search… | Any STAC API or static catalog | [STAC catalogs](#stac-catalogs) |
| Vantor… | Vantor's (formerly Maxar's) disaster imagery | [Vantor](#vantor) |
| France › IGN LiDAR HD… | IGN's LiDAR HD point clouds | [Web services](#web-services) |
| United States › … | EnviroAtlas, FEMA NFHL, Tennessee GIS, The National Map, US Federal/State/Local GIS, USGS 3DEP DEM, USGS LiDAR, USGS NLDI | [United States](#united-states) |

Data added by its address, from a web server or an S3 bucket, is described in
[Data](data.md#s3-buckets) and [Files on the web](data-formats.md#files-on-the-web).

## STAC catalogs

**Home › Add › Web Data › STAC Search…** searches a [STAC](https://stacspec.org) API for imagery and
other assets: by collection, dates, the most cloud cover and (by default) where the map is. The
default API is Element 84's [Earth Search](https://earth-search.aws.element84.com/v1);
Microsoft's [Planetary Computer](https://planetarycomputer.microsoft.com/api/stac/v1) is offered
beside it, and other APIs with GET item search work. **Collections** lists what the API offers,
and **Find Items** shows up to 50 items as cards
(with "Showing 50 of 1234 items" when the API says how many match) and draws their footprints as
a layer. Each card has the item's preview picture when it has one, its id, a collection · date ·
cloud cover line, and **Zoom** (to the item), the asset to add, **Add** and **Details** (the item's
properties). The asset chosen at first is `visual` (true colour) when there is one, else the first
GeoTIFF, else the first point cloud, else the first data asset, preferring HTTP(S) assets.
Cloud-optimised GeoTIFF, GeoParquet and FlatGeobuf assets are read in place, and PMTiles a tile
at a time; other assets download whole (see [Files on the web](data-formats.md#files-on-the-web)).

The commands do the same: `stac.collections` (`"url"`), `stac.search` (`"collections"`,
`"bbox"` as `[west, south, east, north]` or `"view"`, `"datetime"` such as
`2024-06-01/2024-06-30`, `"limit"` from 1 to 200, default 10, `"maxCloud"`, `"footprints"`, and
`"properties": true` for the scalar item properties rather than just date and cloud cover),
and `stac.addAsset` with an item returned by `stac.search` (`"item"`) or an item's address
(`"url"`) and `"asset"`. API searches return one page and a `"next"` address when there is another:

```sh
rustgis-cli run --cmd 'stac.search={"collections":"sentinel-2-c1-l2a","bbox":[7.4,46.9,7.5,47],"datetime":"2024-07-01/2024-07-31","maxCloud":20}' --cmd 'stac.addAsset={"url":"https://earth-search.aws.element84.com/v1/collections/sentinel-2-c1-l2a/items/S2B_T32TLS_20240727T103439_L2A","asset":"scl"}'
```

Assets can use HTTP(S) or `s3://` addresses. S3 assets use the saved connection serving their
bucket, or anonymous AWS access when none matches; see [S3 buckets](data.md#s3-buckets).
An item on the web can't point into the local network. A Planetary
Computer asset (in Azure Blob Storage) is signed first: RustGIS asks the Planetary Computer's
token service for its storage container's token and reuses it for half an hour. The token is part
of the layer's address, so a project saved with one reads it again only while the token lasts
(about an hour): add the asset again after that. A `?stac=` link opens the web app on an item or a
collection ([web app links](../installation.md)). Searches reach the network, so the [AI
Assistant](assistant.md) asks before it runs them, and plugins need the network permission.

### Data cubes on the time slider

When the items found span several dates, **Add as Time Series** (desktop; `stac.addCube`) adds a
band of them as a data cube: a collection's cloud-optimised GeoTIFFs, with one file per tile, date
and band, such as INPE's [Brazil Data Cube](https://data.inpe.br/bdc/) or Sentinel-2 on Earth Search.

- **Search:** the collection is searched over the area (the map's, or `"bbox"`) and the period
  (`"start"`, `"end"`), page after page, up to 2,000 items.
- **Grouping:** items go by the grid tile they cover (`grid:code`, `s2:mgrs_tile`, Landsat's WRS
  path and row, the Brazil Data Cube's `bdc:tiles`, else their bounding box), and by day.
- **Layers:** each tile becomes a tile layer read in place, a file per date. The time slider shows
  the last date in its window, and a tile with no date in it draws nothing. The slider turns on
  over the dates when it is off.
- **Range:** a fixed `"range"` (`[0, 0.9]` for NDVI) is the same for every date and tile, so a grey
  means one value throughout. It is in the band's own units: its `raster:bands` (or `eo:bands`)
  scale and offset turn it into the files' stored values. Without one, each file is stretched on
  its own, between its 2nd and 98th percentiles.
- **Saved with the project:** each tile's dates and addresses.
- **Files in public buckets:** a collection whose items give their files as `s3://` addresses
  (Digital Earth Australia, Digital Earth Africa, the Copernicus DEM on Earth Search) is read
  from AWS at the files' web addresses. Reads are anonymous unless an S3 connection explicitly
  names the bucket, in which case its credentials are used, as for other S3 reads. A
  bucket in a region AWS's global endpoint doesn't reach (Digital Earth Africa's, in Cape Town) is
  read at its own region's, found by reading one byte of its first file. Without a matching
  connection, a bucket that refuses anonymous reads (a private one, or one that charges the
  reader, as Landsat's does) is refused, naming it.
- **A static collection laid out by date:** JAXA Earth's collections have no search, so `"url"` is
  a `collection.json`, and only the period's date folders are read: one date's folders to find the
  items over the area, then those items at each date, about one document per date and tile (a
  tile that date doesn't have is missed in the others too). Documents that can't be read, or are
  refused, are told in a notice. Collections with no year in their dates (climatologies) and hourly
  ones are refused. A month or year folder is taken whole when the period reaches into it, so a
  period from 15 January still has January's file. The codes a collection gives for a failed
  retrieval (`dn.error`) draw on the map as values, inside its range or stretch, and a notice says
  how many files have them; Trend Analysis and cube summaries skip them.
- **Web app:** creating a cube with **Add as Time Series** is desktop-only. Saved cube tile
  layers can draw in the web app when their servers allow browser requests (CORS).
- **Its trend:** the same search, given to Trend Analysis (`cube.trend` with `collection`,
  `asset`, `bbox`, `start` and `end`), reads the values over the area and works out each cell's
  trend: see [Trend analysis](analysis.md#trend-analysis).

```sh
rustgis-cli run --cmd 'stac.addCube={"url":"https://data.inpe.br/bdc/stac/v1","collection":"S2-16D-2","asset":"NDVI","bbox":[-55.9,-12.6,-55.5,-12.35],"start":"2024-08-01","end":"2025-06-30","range":[0,0.9]}' --cmd 'time.export={"path":"ndvi.gif"}'
```

### A catalog of data cubes

A data cube needs more than an API's address: the right collection and band, and often a range
to show it with and a cell size to read it at. The catalog of data cubes lists cubes ready to
open, each with those filled in, so none of them has to be looked up in a provider's catalog.

**Home › Add › Web Data › Data Cubes** lists them, filtered by theme, by words, and (ticked to
begin with) to those with data where the map is. Each shows its provider, period, step,
resolution, source and licence, with a link to the provider's page, and two buttons:

- **Add to Time Slider** (desktop) adds it as `cube.open` does (below): a STAC cube where the map
  is, a Zarr cube whole. It is added over the period typed in **From** and **To**, or its last
  full calendar year when both are empty.
- **Trend Analysis…** (desktop) opens Trend Analysis on it, with the period typed: a STAC cube is
  read where the map is, a Zarr cube whole, and **Use layers instead** goes back to the map's
  layers. A trend over a cube needs its period.

The list is kept up to date between releases with the app's other assets, so a cube can be added,
or an address fixed, without a new version: the app reads it the first time the pane opens, and
uses the copy built into it until then, offline, or when the list doesn't read whole. Commands,
scripts and the assistant use the same list.

- **Finding a cube** (`cube.catalog`): by `"theme"` (vegetation, land cover, water, climate, ocean,
  atmosphere or elevation), by area (`"bbox"`, or `"view"`: the entries with data there; with
  neither, anywhere) and by words in the name, provider or id (`"query"`). Each entry gives its
  provider, themes, area, period, step, resolution, source and licence, with a link to the
  provider's page.
- **Opening one** (desktop; `cube.open` with the entry's `"id"`): a STAC entry is added as
  `stac.addCube` adds a cube, over the map's extent (or `"bbox"`), with the entry's collection,
  band, range and cloud cover cap (`"range"` and `"maxCloud"` override the last two). A Zarr entry
  is added as `layer.addZarr` adds a variable that follows the time slider; its variable is read
  whole, but an area given must meet the entry's. The period is `"start"` and `"end"`, given together; with neither, it
  is the entry's last full calendar year (the year before this one while the cube is kept up to
  date). An end given as a day, a month or a year takes all of it, so an hourly variable's last day
  is read whole. An area the entry has no data in, or a period outside its dates, is refused,
  naming them. **Undo** takes the whole cube away.
- **Its trend or summary:** Trend Analysis and the summary over time take an entry too
  (`cube.trend` or `cube.reduce` with `"cube"`), filling in the collection's API, band, cell size
  and cloud cap (a `"cellSize"` or `"maxCloud"` given is kept), or the Zarr store and variable.
  Data named alongside it (`layer`, `url`, `collection`…) is refused, since the entry would take its
  place.

The entries, all public and read in place:

| Provider | Cubes |
|---|---|
| [Brazil Data Cube](https://data.inpe.br/bdc/) (INPE) | MODIS NDVI and EVI (MOD13Q1 v6.1, 232 m, from 2000); the 16-day NDVI cubes of Sentinel-2 (10 m, from 2017), Landsat (30 m) and CBERS-4 WFI (64 m, from 2016) |
| [Earth Search](https://element84.com/earth-search/) (Element 84) | Sentinel-2 L2A Collection 1 scenes' red and near-infrared bands (10 m, from 2015), searched with at most 20% cloud cover; the Copernicus DEM GLO-30 (one date) |
| [Digital Earth Australia](https://www.dea.ga.gov.au/) and [Digital Earth Africa](https://www.digitalearthafrica.org/) | the yearly frequency of water seen by Landsat (30 m, from 1986 and 1984) |
| [ARCO-ERA5](https://cloud.google.com/storage/docs/public-datasets/era5) (Google Cloud, ECMWF) | hourly air temperature at 2 m, total precipitation and top-layer soil moisture (0.25°, from 1940), as a Zarr store the desktop reads |

Only cubes RustGIS reads today are listed. Collections whose files sit behind signed addresses
(Planetary Computer), need a sign-in (Copernicus Data Space, NASA Earthdata) or charge the reader
(Landsat Collection 2 on Earth Search) are left out. Digital Earth Australia's catalog sits behind
a firewall that may ask a program making many requests in a row for a browser check for a while;
its searches then fail until it lets them through again.

An entry, as the list holds it (built in from `crates/tiles/src/cubes.json`, and kept with the app's
assets); a cube is proposed by adding one:

```json
{"id": "bdc-mod13q1-ndvi", "name": "MODIS NDVI, 16-day composites (MOD13Q1 v6.1)",
 "provider": "Brazil Data Cube (INPE)", "theme": ["vegetation"], "bbox": [-100, -60, -21.3, 20],
 "start": "2000-02-18", "end": null, "step": "16 days", "resolution": "232 m",
 "format": "stac", "url": "https://data.inpe.br/bdc/stac/v1/", "collection": "mod13q1-6.1",
 "asset": "NDVI", "range": [0, 0.9], "cellSize": 232,
 "license": "NASA LP DAAC: no restrictions on use", "about": "https://data.inpe.br/bdc/"}
```

A Zarr entry has `"format": "zarr"` and `"variable"` in place of `"collection"` and `"asset"`, and
no `"range"`, `"cellSize"` or `"maxCloud"`. `"end"` is `null` while a cube is kept up to date. Every
address is a web address (https or http), the dates are `YYYY-MM-DD`, and each id is in the list
once; the list is checked when it is read.

```sh
rustgis-cli run --cmd 'cube.catalog={"theme":"vegetation","bbox":[-43,-6,-42.5,-5.5]}' --cmd 'cube.open={"id":"bdc-mod13q1-ndvi","bbox":[-43,-6,-42.5,-5.5]}'
```

### Saved connections

Once you have a STAC connection (Data Sources › Connections), its name appears in the API list at the top of STAC Search below the built-in APIs: picking it fills in the address and clears the collections and items of the previous one. Add From Service, Add From Database and S3 Buckets have the same **Saved connection** list for their kinds. An API you typed in by hand stays usable; once its collections are listed, **Save as Connection** keeps it under Data Sources › Connections (nothing is saved unless you click it, and never a password or key). Editing the address by hand drops the collections and items listed for the old one.

### Static catalogs

Many catalogs are plain files rather than an API: a `catalog.json` linking to child catalogs and
`collection.json` files, which link to their items. Give STAC Search the address of one (or pick
Planet Open Data, Portolan or Vantor Open Data from its list) and the **Collection** row becomes
**Catalog**: its child catalogs and collections are listed, and picking one shows its own children
(an **↑** entry goes back up; **Browse** reads the list again). **Find Items** walks the catalog from
the one picked, several documents at a time, and keeps the items that match the dates, the most
cloud cover and, with **Only where the map is** ticked, the map's extent. It lists the first 50 items it finds that match, sorted by date (the walk goes in the catalog's
order, so in a large catalog these aren't the earliest or latest: pick a catalog further down for
others). Undated items, and items without a footprint, are kept. A collection whose data
are its own assets (as many Portolan collections' GeoParquet and PMTiles files are) is listed as
an item of its own. A walk reads at most 400 documents and goes at most 8 catalogs deep; when it
stops early, it says so: pick a catalog further down. Items are added as from an API: a
cloud-optimised GeoTIFF is read in place, GeoParquet and FlatGeobuf by range, PMTiles a tile at a
time.

`stac.collections` with a static catalog's `"url"` lists its children (`"id"` is each one's
address), and `stac.search` walks one, with `"phase"` (`"pre"` or `"post"`) for catalogs of events:

```sh
rustgis-cli run --cmd 'stac.search={"url":"https://vantor-opendata.s3.amazonaws.com/events/catalog.json","phase":"post"}'
```

The services below are STAC Search opened on one API or catalog, with the dialog titled after
the service. Their commands (`ui.stac.planet`, `ui.stac.portolan`, `ui.stac.sentinel2`,
`ui.stac.vantor`, `ui.stac.usgsLidar`) take `"catalog"` (a catalog's or collection's address to
start from), `"phase"` and `"search": true` (find items at once).

### Planet Open Data

**Home › Add › Web Data › Planet Open Data…** browses Planet's [disaster data
releases](https://source.coop/planet/disasterdata) on Source Cooperative: imagery of earthquakes,
floods, fires and storms from PlanetScope, SkySat and Pelican, before and after the event, licensed
CC BY-NC 4.0. Pick an event (and, below it, its pre-event or post-event imagery or one collection),
then **Find Items**; **Add** adds a scene's `visual` image.

### Portolan

**Home › Add › Web Data › Portolan…** opens the [Portolan
registry](https://github.com/portolan-sdi/portolan-registry), a catalog of the open data catalogs
published with Portolan (cities, regions and agencies, most in Europe). Pick a publisher's catalog,
then a theme or collection, then **Find Items**: each collection's files (GeoParquet, PMTiles and
others) are listed, and **Add** adds the one chosen (`visual` first, often a PMTiles archive).

### Sentinel-2 Explorer

<figure markdown="span">
  ![The Sentinel-2 Explorer in the Web Data pane over Bern: the clearest recent scenes as cards with previews, and their footprints drawn on the map](https://assets.rustgis.app/images/web-data.webp)
  <figcaption>The Sentinel-2 Explorer: the clearest recent scenes where the map is, with their footprints.</figcaption>
</figure>

**Home › Add › Web Data › Sentinel-2 Explorer…** searches Earth Search's Sentinel-2 Collection 1 Level-2A
scenes (`sentinel-2-c1-l2a`) where the map is, over the last 90 days and with at most 20 % cloud
cover, clearest first (sorted by cloud cover). **Add** adds the true colour image (`visual`);
the asset list beside each scene offers its bands (`red`, `nir`, `swir16`, `scl`…), each a
cloud-optimised GeoTIFF read in place. `stac.search` sorts with `"sortby"`
(`"eo:cloud_cover"`, `"-datetime"` for newest first) on APIs that sort.

### Vantor

**Home › Add › Web Data › Vantor…** browses [Vantor](https://vantor.com)'s open data (formerly Maxar's
Open Data program): very high resolution imagery of disasters, one collection per event. Pick an
event and a phase (before or after the event), then **Find Items**; **Add** adds a scene's
`visual` cloud-optimised GeoTIFF.

### USGS LiDAR

**Home › Add › Web Data › United States › USGS LiDAR…** searches the USGS 3D Elevation Program's LiDAR
point clouds on the Planetary Computer (`3dep-lidar-copc`) where the map is: zoom to a town first.
Each tile's `data` asset is a COPC file, signed with the Planetary Computer's token and added as a
LiDAR point cloud (an evenly spread sample of its points, see [Formats](data.md#formats)). A tile is
downloaded whole, often tens of megabytes, so adding one takes a while.

## Overture Maps

**Home › Add › Web Data › Overture Maps…** adds [Overture Maps](https://overturemaps.org)' open data:
buildings, places, transportation (segments and connectors), administrative divisions, base
layers (land use, land, land cover, water, infrastructure, bathymetry) and addresses, from the
latest release (or one named with `"release"`). Two ways:

- **The features where the map is.** A theme's type is a few hundred GeoParquet files, each
  covering an area; Overture's [STAC catalog](https://stac.overturemaps.org) gives every file's
  extent, so only the files that cover the map's view are read, in place by range like any
  GeoParquet by address (see [Files on the web](data-formats.md#files-on-the-web)), and each
  becomes a layer read a view at a time. Zoom to a city or smaller: a view that takes more than six
  files is refused (a country takes dozens).
- **Map tiles of the whole world**, from the theme's PMTiles archive, drawn a layer per type.
  Some themes' tiles hold their features only when zoomed in (places from zoom 14).

`overture.add` does the same (`"theme"`, `"type"`, `"bbox"` or the view, `"tiles": true` for the
tiles):

```sh
rustgis-cli run --cmd 'map.zoomToExtent={"extent":[7.40,46.93,7.47,46.97]}' --cmd 'overture.add={"theme":"buildings"}' --cmd 'overture.add={"theme":"transportation"}' --cmd 'share.exportMap={"path":"bern.png"}'
```

The data's licences differ by theme (OpenStreetMap's ODbL for most, CDLA Permissive 2.0 for
places); the layer says which.

## Source Cooperative

**Home › Add › Web Data › Source Cooperative…** lists [Source Cooperative](https://source.coop)'s
featured products (or an account's), browses a product's folders and adds a file by its address on
the data proxy (`https://data.source.coop/<account>/<product>/…`): GeoParquet, FlatGeobuf,
cloud-optimised GeoTIFFs and PMTiles are read in place, other files downloaded.
`sourcecoop.products` (`"account"`) and `sourcecoop.list` (`"account"`, `"product"`, `"path"`,
and `"token"` for the next page returned by a listing) list them; a listed file's `"url"` is
added with `layer.addUrl`. These commands reach the network: the AI Assistant asks first and
plugins need the network permission.

## Natural Earth

**Home › Add › Web Data › Natural Earth…** is the Source Cooperative browser locked to
[Natural Earth](https://www.naturalearthdata.com)'s public domain map data as Source Cooperative
holds it (`opengeos/natural-earth`): countries, states, coastlines, land, ocean, rivers, lakes,
glaciers, urban areas, airports, railways and more. It opens on the `pmtiles/` folder, whose
archives are added as tile layers read in place; the `geoparquet/`, `geojson/`, `gpkg/` and
`shapefiles/` folders hold the same themes as files, added as layers. With commands:
`sourcecoop.list` (`{"account": "opengeos", "product": "natural-earth", "path": "pmtiles/"}`), then
`layer.addUrl` with a file's `"url"`.

## Fields of the World

**Home › Add › Web Data › Fields of the World…** adds the agricultural field boundaries the
[Fields of the World](https://fieldsofthe.world) model found in Sentinel-2 imagery, worldwide, one
year at a time (2017 to 2025; 2nd Edition, CC BY 4.0). Each year is a PMTiles archive on Source
Cooperative read in place. Fields are drawn from zoom 9, filled and outlined by the model's score
(0 to 100: red below 45, through orange and yellow, to green from 80); zoomed out, field density
cells are drawn instead, coloured by the share of each cell covered by fields: **Add field
boundaries** and **Add field density** add each. **Score threshold** hides the fields scoring below
it. `fieldsOfTheWorld.add` does the same (`"year"`, `"draw"`:
`both`, `fields` or `density`, `"minScore"`):

```sh
rustgis-cli run --cmd 'map.goToXY={"lon":-93.6,"lat":42.03,"scale":40000}' --cmd 'fieldsOfTheWorld.add={"year":2024,"minScore":40}' --cmd 'share.exportMap={"path":"fields.png"}'
```

The look is the tile layer's own style layers (`map.addTiles` takes MapLibre style layers as
`"styleLayers"`, drawing vector tiles in place of their default colours).

## OpenStreetMap and administrative boundaries

**Home › Add › Web Data › OpenStreetMap…** (`osm.download`) downloads OpenStreetMap data where the map
is from an [Overpass](https://overpass-api.de) server: a preset (buildings, roads, railways, water,
land use, parks, points of interest or administrative boundaries) or tag filters such as
`amenity=cafe, shop`. The answer becomes up to three layers, points, lines and polygons: closed
ways that are areas (buildings, land use and the like, as OSM reads their tags) are polygons, and
multipolygon and boundary relations are assembled from their members. A layer's fields are its 25
most common tags, the rest in `other_tags` as GDAL writes them. The area can be up to 0.25 square
degrees (about 50 × 50 km), and an answer up to 50,000 elements: the public servers are shared, so
a larger area is better read from an extract (`.osm.pbf`). `"url"` names another Overpass server
when the public one is busy.

**Home › Add › Web Data › Boundaries…** (`boundaries.add`) adds a country's administrative boundaries from
[geoBoundaries](https://www.geoboundaries.org) (CC BY 4.0): the country itself (`ADM0`), its states
or provinces (`ADM1`) and finer divisions down to `ADM5` where they exist, simplified for the map.
`boundaries.countries` lists the countries.

```sh
rustgis-cli run --cmd 'osm.download={"bbox":[7.44,46.944,7.456,46.952],"preset":"buildings"}' --cmd 'boundaries.add={"country":"KEN","level":"ADM1"}'
```

Both reach the network: the [AI Assistant](assistant.md) asks before it runs them, and plugins need
the network permission. OpenStreetMap data is © OpenStreetMap contributors (ODbL).

## NASA

NASA's data comes two ways: **NASA Earthdata…** adds imagery that NASA serves as map tiles, and
**NASA Earthaccess…** finds the data files themselves. **Earthdata GIS…** (see
[Web services](#web-services)) searches NASA's portal of map and feature services.

### NASA Earthdata

**Home › Add › Web Data › NASA Earthdata…** lists the layers of NASA's
[Global Imagery Browse Services](https://www.earthdata.nasa.gov/engage/open-data-services-software/earthdata-developer-portal/gibs-api)
(GIBS) in Web Mercator: more than a thousand layers of satellite imagery and science data (MODIS,
VIIRS, Landsat, sea surface temperature, aerosols, fires…), grouped by mission or instrument (the
first part of their names) and searched by words. Most of them change day by day: they go on the
time slider, as every layer with a time dimension does (see
[Layers that change with time](#layers-that-change-with-time)).

### NASA Earthaccess

**Home › Add › Web Data › NASA Earthaccess…** searches [NASA Earthdata](https://www.earthdata.nasa.gov)
as GeoLibre's Earthaccess panel does, through NASA's Common Metadata Repository (CMR):

1. **Dataset**: pick one of the **Popular datasets** (ICESat-2 ATL03/ATL06/ATL08, GEDI L2A/L2B/L4A,
   HLS Landsat and Sentinel-2, NASADEM), or search by keyword (`earthdata.collections`), keeping
   to **Earthdata Cloud only** if you like, and pick one of the results. A popular dataset is its
   cloud-hosted, newest version.
2. **Granules**: set the start and end dates (the last 30 days at first) and press **Search this
   view** (`earthdata.granules`): the granules where the map is, newest first, 25 at a time
   (**Load more**). Their footprints are drawn as a layer.
3. Each granule is a card with its preview, date, size, cloud cover and number of files, and
   **Add** (its first GeoTIFF, desktop only, read in place when tiled), **Download** (a link to
   its first file), **Zoom** and **Details** (its page on Earthdata Search). **Files** lists every
   file, each GeoTIFF with its own desktop **Add**.

Searching needs no account, but NASA's data servers ask for an
[Earthdata Login](https://urs.earthdata.nasa.gov) token to read most files. Paste one in the pane's
**Earthdata Login** section (or run `earthdata.login`). It is never put in the project, and is sent
only over HTTPS to NASA's Earthdata data servers (`*.earthdatacloud.nasa.gov` and
`*.earthdata.nasa.gov`), never to the login server itself; a redirect away from them (to the
presigned address the file is served from) goes without it. A file the token doesn't open says
so (an expired token, or a dataset whose EULA you haven't accepted on Earthdata Login).

The token is kept until RustGIS closes, unless you tick **Remember on this computer** (off by
default; `earthdata.login` with `"remember": true`). The desktop app then keeps it in the system's
credential store (macOS Keychain, Windows Credential Manager, or the Secret Service on Linux) and
reads it back the next time it is needed: when the pane opens or a file is read from a data
server. A token saved without the box ticked deletes the one remembered before. An expired token
is forgotten, here and in the credential store, and the pane asks for a new one.
`earthdata.status` reports whether a token is kept, its expiry, whether it is remembered and
whether an expired one was forgotten; `earthdata.logout` (**Remove token**) forgets it, deletes
the remembered one and empties the download cache, so files and documents read with the token
are read again when next needed. The command line (`--use-saved-secrets`) and Python (`use_saved_secrets=True`)
use the remembered token too; without them, and in the web app, a token lasts for the session.

In the web app, searching and footprints work, but NASA's data servers don't let web pages read
their files (they send no CORS headers), so GeoTIFFs are added in the desktop app; **Download**
still opens a file in a new tab, where Earthdata Login asks you to sign in.

```sh
rustgis-cli run --cmd 'earthdata.collections={"keyword":"GEDI biomass","cloudHosted":true}'
rustgis-cli run --sample --cmd 'earthdata.granules={"shortName":"HLSL30","bbox":[-90,42.5,-89,43.5],"start":"2024-06-01","end":"2024-06-30"}' --cmd 'share.exportMap={"path":"hls.png"}'
rustgis-cli run --sample --cmd 'earthdata.login={"token":"…"}' --cmd 'layer.addUrl={"url":"https://data.lpdaac.earthdatacloud.nasa.gov/lp-prod-protected/HLSL30.020/…/….B04.tif"}'
rustgis-cli run --sample --use-saved-secrets --cmd 'layer.addUrl={"url":"https://data.lpdaac.earthdatacloud.nasa.gov/lp-prod-protected/HLSL30.020/…/….B04.tif"}'
```

## Open-data portals

**Home › Add › Web Data › Search Open Data…** (`portal.search`) finds datasets by text in an open-data
portal and lists, for each, the resources RustGIS can add, the best format first:

- **data.gov and CKAN catalogues**: [catalog.data.gov](https://catalog.data.gov) by default,
  using data.gov's own DCAT search, or a CKAN site by its address
  (`https://open.canada.ca/data`, `https://data.gov.uk`…). GeoJSON, FlatGeobuf, GeoParquet,
  GeoPackage, KML/KMZ, zipped Shapefiles, GeoTIFF, CSV, WMS, WFS and ArcGIS REST resources are
  offered; web pages, PDFs and the like are left out.
- **Socrata**: every Socrata site through its discovery API, or one site such as
  `data.cityofnewyork.us`. Datasets with a location column are added as their GeoJSON export (up
  to 50,000 rows).
- **ArcGIS Hub**: [hub.arcgis.com](https://hub.arcgis.com) or an organisation's Hub site; its
  feature services.

**Only where the map is** keeps to datasets covering the map's view. A file is added by its address
(`portal.add`, read in place when it can be, as **Add Data From URL…** does); a service opens
**Add From Service…** connected to it, to pick its layer (`portal.add` takes the first layer with
features unless `"layer"` names one, or an ArcGIS map or image service's whole map). A search
lists up to 50 datasets.

```sh
rustgis-cli run --cmd 'portal.search={"portal":"socrata","url":"data.cityofnewyork.us","query":"parks"}'
rustgis-cli run --sample --cmd 'portal.add={"url":"https://data.cityofnewyork.us/resource/enfh-gkve.geojson?$limit=50000","name":"Parks","portal":"https://api.us.socrata.com"}'
```

An address a portal lists is judged as one found in a document on the web: it may not point into
the local network. Both commands reach the network, so the AI Assistant asks before it runs them,
and plugins need the network permission. A dataset's licence is the portal's: its page (the
**Page** link) says what it is.

### Web services

Several **Home › Add › Web Data** entries open the same search on one service, with its own choices
(`portal.search` with the `"portal"` in brackets):

| Entry | What it finds | Added as |
|---|---|---|
| **ArcGIS Portal…** (`arcgis-portal`) | Public items on [ArcGIS Online](https://www.arcgis.com), or on the portal whose address is given: feature, map, image and vector tile services, and GeoJSON files | The service (Add From Service), or the file |
| **Dynamical…** (`dynamical`) | Weather and climate datasets from [dynamical.org](https://dynamical.org) (NOAA GFS, GEFS and HRRR, ECMWF, DWD ICON…); **Add** opens the dataset's Zarr store in the pane to pick a variable and its time steps (a dataset published only as Icechunk says so) | A Zarr variable's map |
| **Earthdata GIS…** (`earthdata-gis`) | NASA's [Earthdata GIS](https://gis.earthdata.nasa.gov/portal) portal: Earth science imagery, maps and features | The service |
| **GeoLens…** (`geolens`) | A [GeoLens](https://getgeolens.com) server's vector datasets (`https://datasets.geolibre.app` by default) | Features, the first 1,000 |
| **Hugging Face…** (`huggingface`) | Datasets on [Hugging Face](https://huggingface.co/datasets) by name, or one by `owner/name`; **Browse Files** (`portal.files`) lists its GeoTIFFs, GeoParquet, GeoJSON, FlatGeobuf, GeoPackage, PMTiles, LiDAR and other files (public, not gated) | The file |
| **Ocean Data Platform…** (`ocean-data`) | [HUB Ocean](https://app.hubocean.earth)'s marine datasets, searched by words | Features, up to 10,000 |
| **OpenAerialMap…** (`openaerialmap`) | Drone and aerial images on [OpenAerialMap](https://openaerialmap.org) where the map is, newest first | A COG, read in place |
| **France › IGN LiDAR HD…** (`ign-lidar`) | [IGN](https://geoservices.ign.fr/lidarhd)'s 1 km LiDAR HD tiles where the map is (a view of at most 2 square degrees) | A COPC point cloud downloaded whole and sampled, like [USGS LiDAR](#usgs-lidar) |
| **United States › Tennessee GIS…** | Tennessee's own portal, [Tennessee GeoData](https://geodata.tn.gov): its public items, newest first, as it opens | The service, or the file |
| **United States › US Federal GIS…**, **US State GIS…**, **US Local GIS…** | Curated public GIS portals of federal agencies (by department), states and the District of Columbia, and counties and cities (by state): pick a group and a portal; an ArcGIS organisation's public items (`arcgis-portal` with `"org"`), or a Socrata site | The service, or the file |
| **United States › USGS 3DEP DEM…** (`usgs-dem`) | USGS elevation models from [The National Map](https://apps.nationalmap.gov) where the map is: 1 m, 1/9, 1/3 (the default) or 1 arc-second, or Alaska's 5 m (`"dataset"`), each tile's newest current edition | A GeoTIFF, read in place |

Results are cards, as in GeoLibre: a picture when the service has one, what the item is, who
shared it and when, and **Details** (its description, id and extent), **Portal** (its page),
**Zoom** (to its extent) and **Add**. A web map's **Add** lists its layers to add one by one. ArcGIS
portals keep a search to **All**, **Imagery**, **Maps**, **Features** or **Web maps**, list their
newest items when opened, and page with **Load more**.

The search by place (OpenAerialMap, LiDAR HD, 3DEP) is always where the map is, and its words, when
given, keep to the images or tiles that have them: OpenAerialMap's to 200 images where the map is
at a time (**Load more** looks at older ones), LiDAR HD's to the first 50 tiles. A Hugging Face
dataset lists its first 1,000 files, and the Ocean Data Platform's catalogue is read whole (a few
hundred datasets) and searched in RustGIS. In the web app, the Ocean Data Platform's features are
read through GeoLibre's relay (`tiles.geolibre.app`), the platform answering only its own site, so
those requests pass through it.

```sh
rustgis-cli run --cmd 'portal.search={"portal":"usgs-dem","bbox":[-122.25,46.15,-122.15,46.25]}'
rustgis-cli run --cmd 'portal.files={"dataset":"giswqs/geospatial"}'
rustgis-cli run --sample --cmd 'portal.add={"url":"https://data.geopf.fr/telechargement/download/LiDARHD-NUALID/NUALHD_1-0__LAZ_LAMB93_KE_2025-06-06/LHD_FXX_0652_6864_PTS_LAMB93_IGN69.copc.laz","name":"LiDAR HD 0652-6864"}'
```

When forwarding query options to addresses supplied by a service, RustGIS copies only
`collections`, `datetime`, `subset`, `subset-crs`, `crs`, `bgcolor` and `transparent`.
Other settings, including keys, are inherited only by sub-addresses RustGIS constructs within
the original source path. Some discovery paths report withheld settings in a counted notice.
An advertised WMS GetMap address can replace the discovery address without inheriting its query;
this does not produce that notice.
If it needs authentication, supply an explicitly authorized address for that source rather than
relying on a token being copied from the discovery address. Destination addresses keep settings
they already contain.

### Layers that change with time

A WMS or WMTS layer with a time dimension, from any service, goes on the
[time slider](workspace.md) by default: **Follow the time slider**, which the dialogs tick, and
adding it from the Connections tree. Its requests then take the time of the slider's step, written
as the layer writes its times (`1995`, `19951231`, `2024-06-01`, `2026-08-31T23:59:59Z`), its
GetFeatureInfo requests too, so identify answers for that time; `time.export` animates it.

- **A slider that is off** is turned on with the layer's times: every step of a period it lists
  (`2000-02-24/2026-10-28/P1D`, a day at a time; when it lists only a period longer than the
  slider's 20,000 steps, every second, fourth… step), or the times it lists, at the layer's default
  (or its latest, when its default is no date). A slider already on keeps its steps; a service
  that lists other times may draw nothing at some of them, and a notice says so.
- **At one time instead**: untick **Follow the time slider**. **Date** then takes one of the times
  the layer lists (hover over its "map by date" to see them, and its default), and empty adds it
  at the service's default. The same layer can be added at several times, each a layer named
  with its time.
- **A WMS layer** (WMS 1.3.0 Annex C, 1.1.1) asks with `TIME`. Its default may be an interval;
  where the service lets a layer take several times at once (`multipleValues`), a list
  (`1990,2000`) or an interval (`1990/2020`, `1990/2020/P10Y`) can be asked for too, otherwise one
  time.
- **One time only**: a layer whose dimension lists a single time, a name (OGC's `current`, as most
  of swisstopo's WMTS layers list) or one date (`1946`), has no other to show: it is listed as a
  plain "map", without the dialogs' **Follow the time slider**, and added at its service's default
  (usually that time).
- **A time named, not dated**: a layer whose times are only names (`current`, `default`), whose
  times are one period finer than the slider's hour (`…/PT30M`), or whose list of times would take
  more than the slider's 20,000 steps, isn't stepped through: it is added at its default.

With commands, `service.add` follows the slider when `"time"` is left out (or is `"slider"`, or
`true`); a time of the layer's draws it at that time, and `"default"` (or `false`) at the service's
default. For example, a daily layer at one day, a yearly map on the slider exported as an
animation, and a layer whose only time is `current`:

```sh
rustgis-cli run --cmd 'service.add={"url":"https://gibs.earthdata.nasa.gov/wmts/epsg3857/best/1.0.0/WMTSCapabilities.xml","layer":"MODIS_Terra_CorrectedReflectance_TrueColor","time":"2024-06-01"}' --cmd 'share.exportMap={"path":"gibs.png"}'
rustgis-cli run --cmd 'service.add={"url":"https://wms.geo.admin.ch/?SERVICE=WMS&REQUEST=GetCapabilities&VERSION=1.3.0","layer":"ch.swisstopo.pixelkarte-farbe-pk25.noscale"}' --cmd 'map.goToXY={"lon":7.4474,"lat":46.948,"scale":25000}' --cmd 'time.set={"interval":15}' --cmd 'time.export={"path":"bern.gif"}'
rustgis-cli run --cmd 'service.add={"url":"https://wmts.geo.admin.ch/EPSG/3857/1.0.0/WMTSCapabilities.xml","layer":"ch.swisstopo.pixelkarte-farbe"}' --cmd 'share.exportMap={"path":"map.png"}'
rustgis-cli run --cmd 'service.add={"url":"https://wmts.geo.admin.ch/EPSG/2056/1.0.0/WMTSCapabilities.xml","layer":"ch.swisstopo.pixelkarte-farbe"}' --cmd 'map.projection={"type":"EPSG:2056"}' --cmd 'map.goToXY={"lon":7.4474,"lat":46.948,"scale":20000}' --cmd 'share.exportMap={"path":"bern-lv95.tif"}'
```

The last writes the view as a GeoTIFF in Swiss LV95 (EPSG:2056), placed on the ground, with the
service's credits in its Copyright tag (see [Exporting](sharing.md#exporting)).

## United States

**Home › Add › Web Data › United States** holds services covering the United States: the
portals in the [Web services](#web-services) table (Tennessee GIS, US Federal, State and Local GIS,
USGS 3DEP DEM), [USGS LiDAR](#usgs-lidar), and the panels below.

### EnviroAtlas, FEMA NFHL and The National Map

**EnviroAtlas…** shows the US EPA's [EnviroAtlas](https://www.epa.gov/enviroatlas) services
(`https://enviroatlas.epa.gov/arcgis/rest/services`) as a tree: folders, their services (**Add**
adds a service's whole map or image), and each service's layers, each with an **Add** of its own
(drawn alone). A search looks through the services of every folder and of the folders in them, a few folders at a time. **Add From Service** also browses
any ArcGIS services directory: its folders and the map, feature, image and vector tile services in them are listed
with **Open**, which lists that folder or service (**Back** returns). A map service offers its
whole map, each of its layers drawn alone (`… (map)`) and its layers' features. With commands,
`service.layers` on a directory lists its folders and services, each with the `url` to list next.

**FEMA NFHL…** lists the layers of FEMA's
[National Flood Hazard Layer](https://www.fema.gov/flood-maps/national-flood-hazard-layer) map
service (`https://hazards.fema.gov/arcgis/rest/services/public/NFHL/MapServer`), searched by
words: **Add** draws the whole map or one layer such as Flood Hazard Zones alone, and
**Features** downloads a layer's features in the view (up to the server's limit). Its flood layers are drawn only from zoom 15 (closer than about 1:36,000),
and FEMA's server can be slow to answer.

**The National Map…** lists 31 map services of the USGS
[The National Map](https://www.usgs.gov/programs/national-geospatial-program/national-map) and
partners by category (basemaps, hydrography, elevation, imagery, cartography, hazards, other data,
indexes), searched by words; **Add** adds one as a tile layer (the basemaps from their tile caches,
the others as images the server draws). `nationalMap.services` lists them and `nationalMap.add`
(`"service"`: an id or title) adds one:

```sh
rustgis-cli run --cmd 'nationalMap.add={"service":"basemap/USGSTopo"}' --cmd 'nationalMap.add={"service":"hydro/wbd"}' --cmd 'map.goToXY={"lon":-105.27,"lat":40.0,"scale":300000}' --cmd 'share.exportMap={"path":"topo.png"}'
```

### USGS NLDI

**Home › Add › Web Data › United States › USGS NLDI…** uses the USGS
[Network-Linked Data Index](https://api.water.usgs.gov/nldi) on the NHDPlus river network, as
GeoLibre's NLDI panel does. Each result is a layer:

- **Flowline**: choose the whole flowline, or its part upstream or downstream of the point, and
  click the map (or type a longitude and latitude and press **Trace**, or **Map centre**). The
  flowline a raindrop falling there would reach is added, with the raindrop's path to it
  (`nldi.flowline`, the NLDI's flowtrace; when that fails, the nearest flowline and the way to it).
- **Basin from hydrolocation**: the basin draining to the flowline (`nldi.basin`).
- **Navigation**: pick upstream or downstream, main stem, tributaries or diversions, and a
  distance (1–9,999 km), and press **Load sources & plot**: the sources the navigation can list are
  loaded (`nldi.sources`) and the NHDPlus flowlines along it plotted (`nldi.navigate`). Pick another
  source (stream gages, water quality sites, wells, HUC12 pour points…) and press **Plot another
  navigation layer** to add it too.
- **Clear NLDI result** removes the layers the pane added.

The NLDI's server is sometimes slow or fails for a point it hasn't seen before: try again a little
later, or another point.

```sh
rustgis-cli run --sample --cmd 'nldi.flowline={"lon":-89.36083,"lat":43.08944}' --cmd 'nldi.basin={"comid":13293750}' --cmd 'nldi.navigate={"comid":13293750,"mode":"UM","distance":100}' --cmd 'nldi.navigate={"comid":13293750,"mode":"DM","distance":50,"source":"nwissite"}' --cmd 'share.exportMap={"path":"nldi.png"}'
```

## Satellite embeddings

**Home › Add › Web Data › Satellite Embeddings…** shows pre-computed satellite embeddings: vectors a
geospatial foundation model made for every pixel or image patch, so that places alike have values
alike. Pick a dataset (its provider, model, resolution, years, coverage, licence and links are
shown), a year, and **Search where the map is**; the results are cards with **Zoom** and **Add**,
their footprints drawn as a layer.

| Dataset | What RustGIS does with it |
|---|---|
| **AlphaEarth Foundations** (Google's Satellite Embedding; 64 values a 10 m pixel, 2017–2025) | Finds the year's UTM tiles where the map is in the dataset's GeoParquet index on [Source Cooperative](https://source.coop/tge-labs/aef), read in place (its bounding boxes, then the rows that meet the map). **Add** shows a tile as a cloud-optimised GeoTIFF read in place: three bands (**Bands**, from 1; A01, A16 and A09 by default) as red, green and blue, dequantised (`sign(v)·(v/127.5)²`) and stretched over ±**Stretch** (0.3). Only the three bands shown are read, a band at a time. **Apply to layers** shows other bands or another stretch on the tiles already added. |
| **Earth Index** (Earth Genome; 384 values every ~320 m, 2024) | Names the Sentinel-2 (MGRS) tiles where the map is and keeps those with a file. **Add points** reads the file's points in the view in place (every point's position, then the vectors of those in view: zoom to a few kilometres) and colours them by their vectors' first three principal components (64 colours); **Keep value** keeps one of each vector's values as the points' `value` field. |
| **Tessera** (Cambridge; 128 values a 10 m pixel, 2017–2025) | **Add view** reads three bands (1, 2 and 3 by default) of the view from Tessera's [Zarr store](https://data.source.coop/tessera/tessera/zarr/v1.1-dclimate) in place (the shard's index, then the chunks the view needs: at most about 6 km a side, its centre when the view is larger), as a 32-bit raster in UTM. |
| **Clay**, **Major TOM**, **Copernicus-Embed** | Listed with links to their data and papers; RustGIS doesn't read them. |

AlphaEarth's tiles are tile layers that keep their look in their address
(`#bands=2,17,10&range=-0.3,0.3&dequantize=aef`), which `map.setTiles` changes (`"bands"`,
`"range"`, `"dequantize"`); other cloud-optimised GeoTIFFs are raster layers read in place, styled
in the Symbology pane. GeoTIFFs with their bands stored apart (planar), signed bytes and bottom-up
images are read in place too. The commands: `embeddings.datasets`, `embeddings.search` (`"dataset"`,
`"year"`, `"bbox"` or the view, `"footprints"`) and `embeddings.add` (`"dataset"`, `"url"`,
`"bands"`, `"stretch"`, `"dimension"`, `"bbox"`, `"year"`):

```sh
rustgis-cli run --cmd 'map.zoomToExtent={"extent":[-122.55,37.7,-122.3,37.85]}' --cmd 'embeddings.search={"dataset":"alphaearth","year":2024}' --cmd 'embeddings.add={"dataset":"alphaearth","url":"https://data.source.coop/tge-labs/aef/v1/annual/2024/10N/xcyo46pot2fg6a61t-0000008192-0000000000.tiff"}' --cmd 'share.exportMap={"path":"aef.png"}'
rustgis-cli run --cmd 'map.zoomToExtent={"extent":[-83.96,35.94,-83.9,35.98]}' --cmd 'embeddings.add={"dataset":"tessera","year":2024}' --cmd 'share.exportMap={"path":"tessera.png"}'
```

These reach the network: the AI Assistant asks first and plugins need the network permission.
