// Capabilities fixtures for document-controlled request boundaries.
pub fn documents(href: &str) -> Vec<(&'static str, String)> {
    let href = href.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;");
    let mut docs = Vec::new();
    for version in ["1.3.0", "1.1.1"] {
        docs.push((
            if version == "1.3.0" { "WMS 1.3 GetMap" } else { "WMS 1.1 GetMap" },
            format!(
                r#"<WMS_Capabilities version="{version}" xmlns:xlink="http://www.w3.org/1999/xlink"><Capability><Request>
<GetMap><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="{href}"/></Get></HTTP></DCPType></GetMap>
<GetFeatureInfo><Format>application/json</Format><DCPType><HTTP><Get><OnlineResource xlink:href="{href}"/></Get></HTTP></DCPType></GetFeatureInfo>
<GetLegendGraphic><Format>image/png</Format><DCPType><HTTP><Get><OnlineResource xlink:href="{href}"/></Get></HTTP></DCPType></GetLegendGraphic>
</Request><Layer queryable="1"><Name>a</Name><CRS>EPSG:3857</CRS><SRS>EPSG:3857</SRS><Style><Name>default</Name><LegendURL><Format>image/png</Format><OnlineResource xlink:href="{href}"/></LegendURL></Style></Layer></Capability></WMS_Capabilities>"#
            ),
        ));
    }
    for kind in ["WMTS KVP GetTile", "WMTS ResourceURL"] {
        let operation = match kind {
            "WMTS KVP GetTile" => format!(r#"<GetTile><DCP><HTTP><Get xlink:href="{href}"/></HTTP></DCP></GetTile>"#),
            _ => String::new(),
        };
        let resource = if kind == "WMTS ResourceURL" {
            format!(r#"<ResourceURL resourceType="tile" format="image/png" template="{href}/{{TileMatrix}}/{{TileRow}}/{{TileCol}}.png"/>"#)
        } else {
            String::new()
        };
        docs.push((kind, format!(
            r#"<Capabilities version="1.0.0" xmlns:xlink="http://www.w3.org/1999/xlink">{operation}<Contents><Layer><Identifier>a</Identifier><Style isDefault="true"><Identifier>default</Identifier><LegendURL format="image/png" xlink:href="{href}"/></Style><Format>image/png</Format><TileMatrixSetLink><TileMatrixSet>gm</TileMatrixSet></TileMatrixSetLink>{resource}</Layer>
<TileMatrixSet><Identifier>gm</Identifier><SupportedCRS>EPSG:3857</SupportedCRS><TileMatrix><Identifier>0</Identifier><ScaleDenominator>559082264.0287178</ScaleDenominator><TopLeftCorner>-20037508.34278925 20037508.34278925</TopLeftCorner><TileWidth>256</TileWidth><TileHeight>256</TileHeight></TileMatrix></TileMatrixSet></Contents></Capabilities>"#
        )));
    }
    docs.push(("WFS 1.0 GetFeature", format!(
        r#"<WFS_Capabilities version="1.0.0"><Capability><Request><GetFeature><DCPType><HTTP><Get onlineResource="{href}"/></HTTP></DCPType></GetFeature></Request></Capability><FeatureTypeList><FeatureType><Name>a</Name><SRS>EPSG:4326</SRS></FeatureType></FeatureTypeList></WFS_Capabilities>"#
    )));
    docs.push(("WFS 2.0 GetFeature", format!(
        r#"<WFS_Capabilities version="2.0.0" xmlns:xlink="http://www.w3.org/1999/xlink"><OperationsMetadata><Operation name="GetFeature"><DCP><HTTP><Get xlink:href="{href}"/></HTTP></DCP></Operation></OperationsMetadata><FeatureTypeList><FeatureType><Name>a</Name></FeatureType></FeatureTypeList></WFS_Capabilities>"#
    )));
    docs.push(("WCS 1.0 GetCoverage", format!(
        r#"<WCS_Capabilities version="1.0.0" xmlns:xlink="http://www.w3.org/1999/xlink"><Capability><Request><GetCoverage><DCPType><HTTP><Get><OnlineResource xlink:href="{href}"/></Get></HTTP></DCPType></GetCoverage></Request></Capability><ContentMetadata><CoverageOfferingBrief><name>a</name></CoverageOfferingBrief></ContentMetadata></WCS_Capabilities>"#
    )));
    docs.push(("WCS 2.0 GetCoverage", format!(
        r#"<Capabilities version="2.0.1" xmlns:xlink="http://www.w3.org/1999/xlink"><OperationsMetadata><Operation name="GetCoverage"><DCP><HTTP><Get xlink:href="{href}"/></HTTP></DCP></Operation></OperationsMetadata><Contents><CoverageSummary><CoverageId>a</CoverageId></CoverageSummary></Contents></Capabilities>"#
    )));
    docs
}
