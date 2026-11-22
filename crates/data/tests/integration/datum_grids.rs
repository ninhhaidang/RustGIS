//! A datum grid put in RustGIS's grid folder is used once RustGIS is told it is there
//! ([`crs::grids_changed`]), as PROJ uses it (#2032 A4). Its own test binary: grids are
//! process-wide, and the other tests' reference values are those without grids.
//!
//! References: PROJ 9.8.1 (pyproj 3.8.0), network off, EPSG:4267 to EPSG:4326 at (-100, 40):
//! without grids "NAD27 to WGS 84 (6)", with `us_noaa_conus.tif` (PROJ-data, NOAA, public
//! domain; `tests/data/grids/`, with `us_noaa_ethpgn.tif` for Texas's HARN) "NAD27 to WGS 84 (79)":
//! `pyproj.datadir.append_data_dir(dir); Transformer.from_crs("EPSG:4267", "EPSG:4326", always_xy=True).transform(-100, 40)`.

use rustgis_data::Coord;
use rustgis_data::raster::{Crs, crs};

#[test]
fn a_grid_in_the_grid_folder_is_used_once_it_is_there() {
    // Only a folder of the test's own: none from this computer (PROJ's), whose grids would
    // change the values without grids below.
    let root = tempfile::tempdir().unwrap();
    assert_eq!(crs::grid_dir(), None);
    crs::set_grid_folders(vec![root.path().join("datum-grids")]);
    let dir = crs::grid_dir().unwrap();
    assert!(dir.starts_with(root.path()), "{dir:?}");
    let at = Coord::new(-100.0, 40.0);
    let nad27 = |crs: Crs| crs.transformer().unwrap().to_lonlat(at).unwrap();
    let close = |a: Coord, (x, y): (f64, f64)| (a.x - x).abs() < 1e-11 && (a.y - y).abs() < 1e-11;

    // Without the grid: the next transformation, and a note that the best one needs a grid.
    let before = nad27(Crs::Epsg(4267));
    assert!(close(before, (-100.00041558862637, 39.99999688243282)), "{before:?}");
    let generation = crs::grid_generation();
    // Texas South Central on NAD27, whose best transformation (NAD27 to NAD83 (1) + NAD83 to
    // WGS 84 (38)) needs `us_noaa_conus.tif` and `us_noaa_ethpgn.tif`, says so. EPSG:4267 is asked
    // about in the middle of its area of use, in Oregon, where its HARN grid (`us_noaa_WO.tif`)
    // stays missing.
    let texas = Crs::Epsg(32040);
    let _ = texas.transformer().unwrap();
    assert!(crs::grid_note(texas).is_some());
    // What to download for it, as PROJ names the grids of that transformation.
    let mut names: Vec<(String, String)> = crs::missing_grids(texas).into_iter().map(|g| (g.name, g.url)).collect();
    names.sort();
    assert_eq!(
        names,
        [
            ("us_noaa_conus.tif".to_string(), "https://cdn.proj.org/us_noaa_conus.tif".to_string()),
            ("us_noaa_ethpgn.tif".to_string(), "https://cdn.proj.org/us_noaa_ethpgn.tif".to_string())
        ]
    );

    // A transformer kept from before (a streamed layer's, a tile warp's) follows the grids too.
    let kept = Crs::Epsg(4267).transformer().unwrap();
    // As kept by a PROJ definition and by a WKT (a `.prj` without a code), as well as by a code.
    let kept_def = Crs::parse("+proj=longlat +datum=NAD27 +no_defs").unwrap().transformer().unwrap();
    let nad27_wkt = r#"GEOGCS["NAD27",DATUM["North_American_Datum_1927",SPHEROID["Clarke 1866",6378206.4,294.978698213898],AUTHORITY["EPSG","6267"]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]]"#;
    let kept_wkt = rustgis_data::raster::wkt::crs_from_wkt(nad27_wkt).unwrap();
    assert!(kept_wkt.custom_def().is_some_and(|d| d.wkt.is_some()), "{kept_wkt:?}");
    let kept_wkt = kept_wkt.transformer().unwrap();
    for t in [&kept_def, &kept_wkt] {
        assert!(close(t.to_lonlat(at).unwrap(), (-100.00041558862637, 39.99999688243282)), "{:?}", t.to_lonlat(at));
    }
    // Put in the folder, it isn't used until RustGIS is told: transformers keep their operation.
    std::fs::create_dir_all(&dir).unwrap();
    for grid in ["us_noaa_conus.tif", "us_noaa_ethpgn.tif"] {
        std::fs::copy(format!("{}/tests/data/grids/{grid}", env!("CARGO_MANIFEST_DIR")), dir.join(grid)).unwrap();
    }
    assert!(close(nad27(Crs::Epsg(4267)), (-100.00041558862637, 39.99999688243282)));

    crs::grids_changed();
    assert!(crs::grid_generation() > generation);
    let after = nad27(Crs::Epsg(4267));
    for t in [&kept, &kept_def, &kept_wkt] {
        assert!(close(t.to_lonlat(at).unwrap(), (-100.00040583667015, 40.00000589472259)), "{:?}", t.to_lonlat(at));
    }
    assert!(close(after, (-100.00040583667015, 40.00000589472259)), "{after:?}");
    // As the PROJ definition RustGIS keeps for it, and with nothing left to say.
    let def = Crs::parse("+proj=longlat +datum=NAD27 +no_defs").unwrap();
    assert!(close(nad27(def), (-100.00040583667015, 40.00000589472259)));
    let _ = texas.transformer().unwrap();
    assert_eq!(crs::grid_note(texas), None);
    assert_eq!(crs::missing_grids(texas), []);
    assert!(crs::grid_note(Crs::Epsg(4267)).is_some());
}
