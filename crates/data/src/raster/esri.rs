//! ESRI codes: Esri's projected coordinate systems, for `map.projection` (opengeos/RustGIS#213).
//!
//! `.prj` files and ArcGIS users name coordinate systems by their ESRI code: South America Albers
//! Equal Area Conic is ESRI:102033, World Robinson ESRI:54030, and the state planes in feet
//! ESRI:102xxx. `crs-definitions` holds EPSG codes only, so this table carries them, the way
//! [`super::epsg_gap`] carries the EPSG codes it lacks.
//!
//! Generated from PROJ 9.7.1's database with pyproj 3.7.2: every projected system in the ESRI
//! authority, deprecated ones included (ArcGIS still writes many of them; the comment says so), as
//! `CRS.from_user_input("ESRI:<code>").to_proj4()` without `+type=crs`. Each definition was then
//! checked by projecting two points of its area of use (its centre, and a quarter of the way in
//! from the south-west corner; 20°E 30°N and 40°W 40°N for world systems) with RustGIS's
//! `Projector` and with pyproj, and kept only when they agree within 1 m. Left out:
//!
//! - systems whose method proj4rs 0.2, against which the table was checked, lacks (`col_urban`,
//!   `omerc`, `cass`, `eqdc`, `poly`, `ortho`, `bonne`, `wink2` and more; 0.3 grew the oblique
//!   Mercator, Cassini, the polyconic, the equidistant conic and the orthographic, whose ESRI
//!   systems are candidates to add), and the NAD27 ones whose grids proj4rs can't load: 167;
//! - six world systems RustGIS computes on a sphere where PROJ uses the ellipsoid or other
//!   parameters, kilometres apart: World Miller (54003), the WGS 84 Equal Earths (54035–54037)
//!   and the Winkel Tripel NGS pair (53042, 54042), whose `lat_1` RustGIS doesn't read. The
//!   presets, and EPSG:8857, stay the way to draw these;
//! - 44 systems whose datum PROJ shifts to WGS 84 by something other than one Helmert step
//!   (Molodensky–Badekas for Korean 1985, a chain of operations for NGO 1948 …), and the 11
//!   planetary ones (Moon, Mars), whose bodies the map can't hold;
//! - the seven PROJ can't write as a PROJ string (Hammer–Aitoff, Fuller, Cube, Tobler …) and the
//!   two two-point oblique Mercators (53025, 54025), for which RustGIS has no WKT form.
//!
//! The three Earth sinusoidal systems (Sphere and World Sinusoidal, 53008 and 54008, and Africa
//! Sinusoidal, 102011) were added afterwards (opengeos/RustGIS#563): RustGIS computes the
//! sinusoidal projection itself (`rustgis_geom::Sinusoidal`), checked against PROJ in its tests.
//!
//! Datum shifts follow `epsg_gap.rs`: where the definition has none and the first grid-free
//! operation to WGS 84 that PROJ lists is a single non-zero Helmert step, it is written as
//! `+towgs84` in the position-vector convention. PROJ itself may pick a regional transformation
//! for a given place instead, metres from that one (up to 10 m for ED50 in Turkey, Bogota 1975 or
//! SAD69; NAD83(HARN) is taken as WGS 84, about 1 m off).

/// The PROJ definition of an ESRI projected coordinate system, or `None`.
pub fn proj4(code: u32) -> Option<&'static str> {
    DEFS.binary_search_by_key(&code, |&(c, _)| c).ok().and_then(|i| DEFS.get(i)).map(|&(_, p)| p)
}

/// (code, PROJ definition), sorted by code. The comment is the ESRI name.
#[rustfmt::skip]
pub(crate) const DEFS: &[(u32, &str)] = &[
    (2181, "+proj=tmerc +lat_0=0 +lon_0=27 +k=0.9996 +x_0=9500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_9 (deprecated)
    (2182, "+proj=tmerc +lat_0=0 +lon_0=30 +k=0.9996 +x_0=10500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_10 (deprecated)
    (2183, "+proj=tmerc +lat_0=0 +lon_0=33 +k=0.9996 +x_0=11500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_11 (deprecated)
    (2184, "+proj=tmerc +lat_0=0 +lon_0=36 +k=0.9996 +x_0=12500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_12 (deprecated)
    (2185, "+proj=tmerc +lat_0=0 +lon_0=39 +k=0.9996 +x_0=13500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_13 (deprecated)
    (2186, "+proj=tmerc +lat_0=0 +lon_0=42 +k=0.9996 +x_0=14500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_14 (deprecated)
    (2187, "+proj=tmerc +lat_0=0 +lon_0=45 +k=0.9996 +x_0=15500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_15 (deprecated)
    (10820, "+proj=aea +lat_0=40 +lon_0=-96 +lat_1=44.75 +lat_2=55.75 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Agriculture_Canada_Albers
    (20062, "+proj=tmerc +lat_0=0 +lon_0=9 +k=1 +x_0=500000 +y_0=0 +ellps=krass +towgs84=24.47,-130.89,-81.56,-0,-0,0.13,-0.22 +units=m +no_defs"), // Pulkovo_1995_GK_Zone_2N (deprecated)
    (20063, "+proj=tmerc +lat_0=0 +lon_0=15 +k=1 +x_0=500000 +y_0=0 +ellps=krass +towgs84=24.47,-130.89,-81.56,-0,-0,0.13,-0.22 +units=m +no_defs"), // Pulkovo_1995_GK_Zone_3N (deprecated)
    (31491, "+proj=tmerc +lat_0=0 +lon_0=3 +k=1 +x_0=1500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_1 (deprecated)
    (31492, "+proj=tmerc +lat_0=0 +lon_0=6 +k=1 +x_0=2500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_2 (deprecated)
    (31493, "+proj=tmerc +lat_0=0 +lon_0=9 +k=1 +x_0=3500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_3 (deprecated)
    (31494, "+proj=tmerc +lat_0=0 +lon_0=12 +k=1 +x_0=4500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_4 (deprecated)
    (31495, "+proj=tmerc +lat_0=0 +lon_0=15 +k=1 +x_0=5500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_5 (deprecated)
    (31917, "+proj=utm +zone=17 +ellps=GRS80 +units=m +no_defs"), // SIRGAS_UTM_Zone_17N (deprecated)
    (31918, "+proj=utm +zone=18 +ellps=GRS80 +units=m +no_defs"), // SIRGAS_UTM_Zone_18N (deprecated)
    (31919, "+proj=utm +zone=19 +ellps=GRS80 +units=m +no_defs"), // SIRGAS_UTM_Zone_19N (deprecated)
    (31920, "+proj=utm +zone=20 +ellps=GRS80 +units=m +no_defs"), // SIRGAS_UTM_Zone_20N (deprecated)
    (31921, "+proj=utm +zone=21 +ellps=GRS80 +units=m +no_defs"), // SIRGAS_UTM_Zone_21N (deprecated)
    (31922, "+proj=utm +zone=22 +ellps=GRS80 +units=m +no_defs"), // SIRGAS_UTM_Zone_22N (deprecated)
    (53001, "+proj=eqc +lat_ts=0 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Plate_Carree
    (53002, "+proj=eqc +lat_ts=60 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Equidistant_Cylindrical
    (53003, "+proj=mill +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Miller_Cylindrical
    (53004, "+proj=merc +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Mercator
    (53008, "+proj=sinu +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Sinusoidal
    (53009, "+proj=moll +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Mollweide
    (53012, "+proj=eck4 +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Eckert_IV
    (53017, "+proj=cea +lat_ts=30 +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Behrmann
    (53026, "+proj=stere +lat_0=0 +lon_0=0 +k=1 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Stereographic
    (53030, "+proj=robin +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Robinson
    (53032, "+proj=aeqd +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Azimuthal_Equidistant
    (53034, "+proj=cea +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +R=6371000 +units=m +no_defs"), // Sphere_Cylindrical_Equal_Area
    (53035, "+proj=eqearth +lon_0=0 +x_0=0 +y_0=0 +R=6371008.7714 +units=m +no_defs"), // Sphere_Equal_Earth_Greenwich
    (53036, "+proj=eqearth +lon_0=-90 +x_0=0 +y_0=0 +R=6371008.7714 +units=m +no_defs"), // Sphere_Equal_Earth_Americas
    (53037, "+proj=eqearth +lon_0=150 +x_0=0 +y_0=0 +R=6371008.7714 +units=m +no_defs"), // Sphere_Equal_Earth_Asia_Pacific
    (53074, "+proj=wag4 +lon_0=0 +x_0=0 +y_0=0 +R=6371007 +units=m +no_defs"), // Sphere_Wagner_IV
    (53075, "+proj=wag5 +lon_0=0 +x_0=0 +y_0=0 +R=6371008.7714 +units=m +no_defs"), // Sphere_Wagner_V
    (53077, "+proj=natearth +lon_0=0 +x_0=0 +y_0=0 +R=6371008.7714 +units=m +no_defs"), // Sphere_Natural_Earth
    (54001, "+proj=eqc +lat_ts=0 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Plate_Carree
    (54002, "+proj=eqc +lat_ts=60 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Equidistant_Cylindrical
    (54004, "+proj=merc +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Mercator
    (54008, "+proj=sinu +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Sinusoidal
    (54009, "+proj=moll +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Mollweide
    (54012, "+proj=eck4 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Eckert_IV
    (54017, "+proj=cea +lat_ts=30 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Behrmann
    (54026, "+proj=stere +lat_0=0 +lon_0=0 +k=1 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Stereographic
    (54030, "+proj=robin +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Robinson
    (54032, "+proj=aeqd +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Azimuthal_Equidistant
    (54034, "+proj=cea +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Cylindrical_Equal_Area
    (54074, "+proj=wag4 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Wagner_IV
    (54075, "+proj=wag5 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Wagner_V
    (54077, "+proj=natearth +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // World_Natural_Earth
    (54092, "+proj=aea +lat_0=45.568977 +lon_0=-84.455955 +lat_1=42.122774 +lat_2=49.01518 +x_0=1000000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Great_Lakes_Basin_Albers
    (54093, "+proj=aea +lat_0=37.5 +lon_0=-96 +lat_1=29.5 +lat_2=45.5 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_USA_Contiguous_Albers_Equal_Area_Conic
    (54094, "+proj=lcc +lat_0=39 +lon_0=-96 +lat_1=33 +lat_2=45 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_USA_Contiguous_Lambert_Conformal_Conic
    (54096, "+proj=aea +lat_0=23 +lon_0=-96 +lat_1=29.5 +lat_2=45.5 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_USA_Contiguous_Albers_Equal_Area_Conic_USGS_version
    (65062, "+proj=lcc +lat_1=-14.2666666666667 +lat_0=-14.2666666666667 +lon_0=-170 +k_0=1 +x_0=152400.30480061 +y_0=95169.3116586233 +ellps=clrk66 +towgs84=-115,118,426,0,0,0,0 +units=us-ft +no_defs"), // American_Samoa_1962_StatePlane_American_Samoa_FIPS_5300
    (65162, "+proj=tmerc +lat_0=0 +lon_0=-78.5 +k=1.0004584 +x_0=500000 +y_0=10000000 +datum=WGS84 +units=m +no_defs"), // WGS_1984_SIRES_TMQ
    (65163, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=37.0833333333333 +lat_2=38.6666666666667 +x_0=1500000 +y_0=1000000 +datum=NAD83 +units=m +no_defs"), // NAD_1983_StatePlane_Kentucky_FIPS_1600 (deprecated)
    (102001, "+proj=aea +lat_0=40 +lon_0=-96 +lat_1=50 +lat_2=70 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // Canada_Albers_Equal_Area_Conic
    (102002, "+proj=lcc +lat_0=40 +lon_0=-96 +lat_1=50 +lat_2=70 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // Canada_Lambert_Conformal_Conic
    (102003, "+proj=aea +lat_0=37.5 +lon_0=-96 +lat_1=29.5 +lat_2=45.5 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // USA_Contiguous_Albers_Equal_Area_Conic
    (102004, "+proj=lcc +lat_0=39 +lon_0=-96 +lat_1=33 +lat_2=45 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // USA_Contiguous_Lambert_Conformal_Conic
    (102006, "+proj=aea +lat_0=50 +lon_0=-154 +lat_1=55 +lat_2=65 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Alaska_Albers (deprecated)
    (102007, "+proj=aea +lat_0=13 +lon_0=-157 +lat_1=8 +lat_2=18 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // Hawaii_Albers_Equal_Area_Conic
    (102008, "+proj=aea +lat_0=40 +lon_0=-96 +lat_1=20 +lat_2=60 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // North_America_Albers_Equal_Area_Conic
    (102009, "+proj=lcc +lat_0=40 +lon_0=-96 +lat_1=20 +lat_2=60 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // North_America_Lambert_Conformal_Conic
    (102011, "+proj=sinu +lon_0=15 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // Africa_Sinusoidal
    (102012, "+proj=lcc +lat_0=0 +lon_0=105 +lat_1=30 +lat_2=62 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // Asia_Lambert_Conformal_Conic
    (102013, "+proj=aea +lat_0=30 +lon_0=10 +lat_1=43 +lat_2=62 +x_0=0 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // Europe_Albers_Equal_Area_Conic
    (102014, "+proj=lcc +lat_0=30 +lon_0=10 +lat_1=43 +lat_2=62 +x_0=0 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // Europe_Lambert_Conformal_Conic
    (102015, "+proj=lcc +lat_0=-32 +lon_0=-60 +lat_1=-5 +lat_2=-42 +x_0=0 +y_0=0 +ellps=aust_SA +towgs84=-57,1,-41,0,0,0,0 +units=m +no_defs"), // South_America_Lambert_Conformal_Conic
    (102016, "+proj=aeqd +lat_0=90 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // North_Pole_Azimuthal_Equidistant
    (102017, "+proj=laea +lat_0=90 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // North_Pole_Lambert_Azimuthal_Equal_Area
    (102018, "+proj=stere +lat_0=90 +lon_0=0 +k=1 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // North_Pole_Stereographic
    (102019, "+proj=aeqd +lat_0=-90 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // South_Pole_Azimuthal_Equidistant
    (102020, "+proj=laea +lat_0=-90 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // South_Pole_Lambert_Azimuthal_Equal_Area
    (102021, "+proj=stere +lat_0=-90 +lon_0=0 +k=1 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // South_Pole_Stereographic
    (102022, "+proj=aea +lat_0=0 +lon_0=25 +lat_1=20 +lat_2=-23 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // Africa_Albers_Equal_Area_Conic
    (102024, "+proj=lcc +lat_0=0 +lon_0=25 +lat_1=20 +lat_2=-23 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // Africa_Lambert_Conformal_Conic
    (102025, "+proj=aea +lat_0=30 +lon_0=95 +lat_1=15 +lat_2=65 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // Asia_North_Albers_Equal_Area_Conic
    (102027, "+proj=lcc +lat_0=30 +lon_0=95 +lat_1=15 +lat_2=65 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // Asia_North_Lambert_Conformal_Conic
    (102028, "+proj=aea +lat_0=-15 +lon_0=125 +lat_1=7 +lat_2=-32 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // Asia_South_Albers_Equal_Area_Conic
    (102030, "+proj=lcc +lat_0=-15 +lon_0=125 +lat_1=7 +lat_2=-32 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // Asia_South_Lambert_Conformal_Conic
    (102033, "+proj=aea +lat_0=-32 +lon_0=-60 +lat_1=-5 +lat_2=-42 +x_0=0 +y_0=0 +ellps=aust_SA +towgs84=-57,1,-41,0,0,0,0 +units=m +no_defs"), // South_America_Albers_Equal_Area_Conic
    (102039, "+proj=aea +lat_0=23 +lon_0=-96 +lat_1=29.5 +lat_2=45.5 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // USA_Contiguous_Albers_Equal_Area_Conic_USGS_version
    (102041, "+proj=lcc +lat_0=47 +lon_0=-120.833333333333 +lat_1=47.5 +lat_2=48.7333333333333 +x_0=500000.07010414 +y_0=0.0731521463042926 +ellps=GRS80 +units=us-ft +no_defs"), // COB_NAD83_2007
    (102042, "+proj=aea +lat_0=35 +lon_0=-82.25 +lat_1=38.25 +lat_2=47.25 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_USFS_R9_Albers
    (102043, "+proj=utm +zone=20 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_20N
    (102044, "+proj=utm +zone=20 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_NSRS2007_UTM_Zone_20N
    (102045, "+proj=utm +zone=20 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_20N
    (102046, "+proj=utm +zone=59 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_59N (deprecated)
    (102047, "+proj=utm +zone=60 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_60N (deprecated)
    (102048, "+proj=utm +zone=1 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_1N (deprecated)
    (102049, "+proj=utm +zone=2 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_2N (deprecated)
    (102050, "+proj=utm +zone=3 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_3N (deprecated)
    (102051, "+proj=utm +zone=4 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_4N (deprecated)
    (102052, "+proj=utm +zone=5 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_5N (deprecated)
    (102053, "+proj=utm +zone=6 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_6N (deprecated)
    (102054, "+proj=utm +zone=7 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_7N (deprecated)
    (102055, "+proj=utm +zone=8 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_8N (deprecated)
    (102056, "+proj=utm +zone=9 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_9N (deprecated)
    (102057, "+proj=utm +zone=10 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_10N (deprecated)
    (102058, "+proj=utm +zone=11 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_11N (deprecated)
    (102059, "+proj=utm +zone=12 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_12N (deprecated)
    (102060, "+proj=tmerc +lat_0=0 +lon_0=15 +k=0.9999 +x_0=500000 +y_0=-5000000 +ellps=bessel +towgs84=476.08,125.947,417.81,4.610862,2.388137,-11.942335,9.896638 +units=m +no_defs"), // D48_Slovenia_TM
    (102063, "+proj=tmerc +lat_0=7.00048027777778 +lon_0=80.7717111111111 +k=1 +x_0=160933.56048 +y_0=160933.56048 +ellps=evrst30 +towgs84=-97,787,86,0,0,0,0 +units=m +no_defs"), // Kandawala_Ceylon_Belt_Meters
    (102064, "+proj=tmerc +lat_0=7.00048027777778 +lon_0=80.7717111111111 +k=1 +x_0=160933.56048 +y_0=160933.56048 +ellps=evrst30 +towgs84=-97,787,86,0,0,0,0 +units=ind-yd +no_defs"), // Kandawala_Ceylon_Belt_Indian_Yards_1937
    (102065, "+proj=krovak +axis=swu +lat_0=49.5 +lon_0=24.8333333333333 +alpha=30.2881397527778 +k=0.9999 +x_0=0 +y_0=0 +ellps=bessel +towgs84=589,76,480,0,0,0,0 +units=m +no_defs"), // S-JTSK_Krovak (deprecated)
    (102067, "+proj=krovak +lat_0=49.5 +lon_0=24.8333333333333 +alpha=30.2881397527778 +k=0.9999 +x_0=0 +y_0=0 +ellps=bessel +towgs84=589,76,480,0,0,0,0 +units=m +no_defs"), // S-JTSK_Krovak_East_North (deprecated)
    (102068, "+proj=stere +lat_0=90 +lat_ts=60 +lon_0=-32 +x_0=400000 +y_0=5500000 +R=6370000 +to_meter=50000 +no_defs"), // EMEP_50_Kilometer_Grid
    (102069, "+proj=stere +lat_0=90 +lat_ts=60 +lon_0=-32 +x_0=450000 +y_0=5550000 +R=6370000 +to_meter=150000 +no_defs"), // EMEP_150_Kilometer_Grid
    (102070, "+proj=tmerc +lat_0=49.5 +lon_0=-2.41666666666667 +k=0.999997 +x_0=47000 +y_0=50000 +datum=WGS84 +units=m +no_defs"), // Guernsey_Grid
    (102071, "+proj=tmerc +lat_0=0 +lon_0=149.009294833333 +k=1.000086 +x_0=200000 +y_0=4510193.4939 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ACT_Grid_AGC_Zone
    (102072, "+proj=tmerc +lat_0=0 +lon_0=141 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_54_2
    (102073, "+proj=tmerc +lat_0=0 +lon_0=143 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_54_3
    (102074, "+proj=tmerc +lat_0=0 +lon_0=145 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_55_1
    (102075, "+proj=tmerc +lat_0=0 +lon_0=147 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_55_2
    (102076, "+proj=tmerc +lat_0=0 +lon_0=149 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_55_3
    (102077, "+proj=tmerc +lat_0=0 +lon_0=151 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_56_1
    (102078, "+proj=tmerc +lat_0=0 +lon_0=153 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_56_2
    (102079, "+proj=tmerc +lat_0=0 +lon_0=155 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_56_3
    (102080, "+proj=tmerc +lat_0=38 +lon_0=127.5 +k=0.9996 +x_0=1000000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // KGD2002_Unified_Coordinate_System (deprecated)
    (102081, "+proj=tmerc +lat_0=38 +lon_0=125 +k=1 +x_0=200000 +y_0=600000 +ellps=GRS80 +units=m +no_defs"), // KGD2002_West_Belt_2010 (deprecated)
    (102082, "+proj=tmerc +lat_0=38 +lon_0=127 +k=1 +x_0=200000 +y_0=600000 +ellps=GRS80 +units=m +no_defs"), // KGD2002_Central_Belt_2010 (deprecated)
    (102083, "+proj=tmerc +lat_0=38 +lon_0=129 +k=1 +x_0=200000 +y_0=600000 +ellps=GRS80 +units=m +no_defs"), // KGD2002_East_Belt_2010 (deprecated)
    (102084, "+proj=tmerc +lat_0=38 +lon_0=131 +k=1 +x_0=200000 +y_0=600000 +ellps=GRS80 +units=m +no_defs"), // KGD2002_East_Sea_Belt_2010 (deprecated)
    (102090, "+proj=tmerc +lat_0=32 +lon_0=-64.75 +k=1 +x_0=550000 +y_0=100000 +ellps=WGS84 +units=m +no_defs"), // Bermuda_2000_National_Grid (deprecated)
    (102091, "+proj=tmerc +lat_0=0 +lon_0=9 +k=0.9996 +x_0=1500000 +y_0=0 +ellps=intl +towgs84=-104.1,-49.1,-9.9,0.971,-2.917,0.714,-11.68 +units=m +no_defs"), // Monte_Mario_Italy_1 (deprecated)
    (102092, "+proj=tmerc +lat_0=0 +lon_0=15 +k=0.9996 +x_0=2520000 +y_0=0 +ellps=intl +towgs84=-104.1,-49.1,-9.9,0.971,-2.917,0.714,-11.68 +units=m +no_defs"), // Monte_Mario_Italy_2 (deprecated)
    (102093, "+proj=tmerc +lat_0=0 +lon_0=15 +k=0.9996 +x_0=2520000 +y_0=0 +ellps=intl +units=m +no_defs"), // Roma_1940_Gauss_Boaga_Est
    (102094, "+proj=tmerc +lat_0=0 +lon_0=9 +k=0.9996 +x_0=1500000 +y_0=0 +ellps=intl +units=m +no_defs"), // Roma_1940_Gauss_Boaga_Ovest
    (102095, "+proj=lcc +lat_1=18 +lat_0=18 +lon_0=-77 +k_0=1 +x_0=750000 +y_0=650000 +ellps=WGS84 +units=m +no_defs"), // JAD_2001_Jamaica_Grid (deprecated)
    (102096, "+proj=aeqd +lat_0=7.35122211111111 +lon_0=134.450444861111 +x_0=50000 +y_0=150000 +ellps=clrk66 +towgs84=-185.583,-230.096,281.361,0,0,0,0 +units=m +no_defs"), // Bab_South_Palau_Azimuthal_Equidistant
    (102097, "+proj=utm +zone=26 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UTM_Zone_26N (deprecated)
    (102098, "+proj=utm +zone=27 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UTM_Zone_27N (deprecated)
    (102099, "+proj=utm +zone=39 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UTM_Zone_39N (deprecated)
    (102100, "+proj=merc +a=6378137 +b=6378137 +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +k=1 +units=m +nadgrids=@null +wktext +no_defs"), // WGS_1984_Web_Mercator_Auxiliary_Sphere (deprecated)
    (102109, "+proj=tmerc +lat_0=0 +lon_0=15 +k=0.9999 +x_0=500000 +y_0=-5000000 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_Slovenia_TM
    (102110, "+proj=lcc +lat_0=46.5 +lon_0=3 +lat_1=44 +lat_2=49 +x_0=700000 +y_0=6600000 +ellps=GRS80 +units=m +no_defs"), // RGF_1993_Lambert_93 (deprecated)
    (102111, "+proj=tmerc +lat_0=-44 +lon_0=-176.5 +k=1 +x_0=350000 +y_0=650000 +ellps=intl +towgs84=174.05,-25.49,112.57,-0,-0,0.554,0.2263 +units=m +no_defs"), // Chatham_Islands_1979_Map_Grid (deprecated)
    (102112, "+proj=tmerc +lat_0=-44 +lon_0=-176.5 +k=1 +x_0=400000 +y_0=800000 +ellps=GRS80 +units=m +no_defs"), // NZGD_2000_Chatham_Island_Circuit (deprecated)
    (102113, "+proj=merc +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +R=6378137 +units=m +no_defs"), // WGS_1984_Web_Mercator (deprecated)
    (102114, "+proj=utm +zone=4 +ellps=clrk66 +towgs84=61,-285,-181,0,0,0,0 +units=m +no_defs"), // Old_Hawaiian_UTM_Zone_4N
    (102115, "+proj=utm +zone=5 +ellps=clrk66 +towgs84=61,-285,-181,0,0,0,0 +units=m +no_defs"), // Old_Hawaiian_UTM_Zone_5N
    (102116, "+proj=utm +zone=2 +south +ellps=clrk66 +towgs84=-115,118,426,0,0,0,0 +units=m +no_defs"), // American_Samoa_1962_UTM_Zone_2S
    (102128, "+proj=utm +zone=1 +datum=NAD83 +units=m +no_defs"), // NAD_1983_UTM_Zone_1N (deprecated)
    (102129, "+proj=utm +zone=2 +datum=NAD83 +units=m +no_defs"), // NAD_1983_UTM_Zone_2N (deprecated)
    (102130, "+proj=utm +zone=59 +datum=NAD83 +units=m +no_defs"), // NAD_1983_UTM_Zone_59N (deprecated)
    (102131, "+proj=utm +zone=60 +datum=NAD83 +units=m +no_defs"), // NAD_1983_UTM_Zone_60N (deprecated)
    (102139, "+proj=utm +zone=35 +ellps=GRS80 +units=m +no_defs"), // EUREF-FIN_TM35FIN (deprecated)
    (102140, "+proj=tmerc +lat_0=22.3121333333333 +lon_0=114.178555555556 +k=1 +x_0=836694.05 +y_0=819069.8 +ellps=intl +towgs84=-162.619,-276.959,-161.764,0.067753,-2.243648,-1.158828,-1.094246 +units=m +no_defs"), // Hong_Kong_1980_Grid (deprecated)
    (102141, "+proj=utm +zone=49 +ellps=intl +towgs84=-162.619,-276.959,-161.764,0.067753,-2.243648,-1.158828,-1.094246 +units=m +no_defs"), // Hong_Kong_1980_UTM_Zone_49N
    (102142, "+proj=utm +zone=50 +ellps=intl +towgs84=-162.619,-276.959,-161.764,0.067753,-2.243648,-1.158828,-1.094246 +units=m +no_defs"), // Hong_Kong_1980_UTM_Zone_50N
    (102143, "+proj=utm +zone=39 +ellps=intl +towgs84=-119.4248,-303.65872,-11.00061,1.164298,0.174458,1.096259,3.657065 +units=m +no_defs"), // QND_1995_UTM_39N
    (102144, "+proj=utm +zone=28 +ellps=clrk80ign +towgs84=31,146,47,0,0,0,0 +units=m +no_defs"), // Merchich_UTM_Zone_28N
    (102145, "+proj=utm +zone=51 +ellps=GRS80 +units=m +no_defs"), // JGD_2000_UTM_Zone_51N (deprecated)
    (102146, "+proj=utm +zone=52 +ellps=GRS80 +units=m +no_defs"), // JGD_2000_UTM_Zone_52N (deprecated)
    (102147, "+proj=utm +zone=53 +ellps=GRS80 +units=m +no_defs"), // JGD_2000_UTM_Zone_53N (deprecated)
    (102148, "+proj=utm +zone=54 +ellps=GRS80 +units=m +no_defs"), // JGD_2000_UTM_Zone_54N (deprecated)
    (102149, "+proj=utm +zone=55 +ellps=GRS80 +units=m +no_defs"), // JGD_2000_UTM_Zone_55N (deprecated)
    (102150, "+proj=utm +zone=56 +ellps=GRS80 +units=m +no_defs"), // JGD_2000_UTM_Zone_56N
    (102151, "+proj=utm +zone=51 +ellps=bessel +towgs84=-146.414,507.337,680.507,0,0,0,0 +units=m +no_defs"), // Tokyo_UTM_Zone_51N (deprecated)
    (102152, "+proj=utm +zone=52 +ellps=bessel +towgs84=-146.414,507.337,680.507,0,0,0,0 +units=m +no_defs"), // Tokyo_UTM_Zone_52N (deprecated)
    (102153, "+proj=utm +zone=53 +ellps=bessel +towgs84=-146.414,507.337,680.507,0,0,0,0 +units=m +no_defs"), // Tokyo_UTM_Zone_53N (deprecated)
    (102154, "+proj=utm +zone=54 +ellps=bessel +towgs84=-146.414,507.337,680.507,0,0,0,0 +units=m +no_defs"), // Tokyo_UTM_Zone_54N (deprecated)
    (102155, "+proj=utm +zone=55 +ellps=bessel +towgs84=-146.414,507.337,680.507,0,0,0,0 +units=m +no_defs"), // Tokyo_UTM_Zone_55N (deprecated)
    (102156, "+proj=utm +zone=56 +ellps=bessel +towgs84=-146.414,507.337,680.507,0,0,0,0 +units=m +no_defs"), // Tokyo_UTM_Zone_56N
    (102157, "+proj=tmerc +lat_0=0 +lon_0=21 +k=0.9999 +x_0=7500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_Kosovo_Grid
    (102158, "+proj=tmerc +lat_0=0 +lon_0=37 +k=0.9998 +x_0=500000 +y_0=-3000000 +ellps=intl +towgs84=-86,-98,-119,0,0,0,0 +units=m +no_defs"), // Jordan_JTM
    (102160, "+proj=tmerc +lat_0=39.6666666666667 +lon_0=-8.13190611111111 +k=1 +x_0=200180.598 +y_0=299913.01 +ellps=intl +towgs84=-239.749,88.181,30.488,0.263,0.082,1.211,2.229 +units=m +no_defs"), // Datum_73_Hayford_Gauss_IGeoE
    (102161, "+proj=tmerc +lat_0=39.6666666666667 +lon_0=-8.13190611111111 +k=1 +x_0=180.598 +y_0=-86.99 +ellps=intl +towgs84=-239.749,88.181,30.488,0.263,0.082,1.211,2.229 +units=m +no_defs"), // Datum_73_Hayford_Gauss_IPCC
    (102162, "+proj=utm +zone=26 +ellps=intl +towgs84=-105.679,166.1,-37.322,0,0,0,0 +units=m +no_defs"), // Graciosa_Base_SW_1948_UTM_Zone_26N
    (102164, "+proj=tmerc +lat_0=39.6666666666667 +lon_0=-8.13190611111111 +k=1 +x_0=200000 +y_0=300000 +ellps=intl +towgs84=-283.088,-70.693,117.445,-1.157,0.059,-0.652,-4.058 +units=m +no_defs"), // Lisboa_Hayford_Gauss_IGeoE
    (102165, "+proj=tmerc +lat_0=39.6666666666667 +lon_0=-8.13190611111111 +k=1 +x_0=0 +y_0=0 +ellps=intl +towgs84=-283.088,-70.693,117.445,-1.157,0.059,-0.652,-4.058 +units=m +no_defs"), // Lisboa_Hayford_Gauss_IPCC
    (102166, "+proj=utm +zone=25 +ellps=intl +towgs84=-422.651,-172.995,84.02,0,0,0,0 +units=m +no_defs"), // Observatorio_Meteorologico_1939_UTM_Zone_25N
    (102167, "+proj=utm +zone=28 +ellps=intl +towgs84=-499,-249,314,0,0,0,0 +units=m +no_defs"), // Porto_Santo_1936_UTM_Zone_28N (deprecated)
    (102168, "+proj=utm +zone=26 +ellps=intl +towgs84=-269.089,186.247,155.667,2.005,3.606,-0.366,0.097 +units=m +no_defs"), // Sao_Braz_UTM_Zone_26N
    (102169, "+proj=utm +zone=28 +ellps=intl +towgs84=-289,-124,60,0,0,0,0 +units=m +no_defs"), // Selvagem_Grande_1938_UTM_Zone_28N (deprecated)
    (102170, "+proj=lcc +lat_0=-37 +lon_0=145 +lat_1=-36 +lat_2=-38 +x_0=2500000 +y_0=4500000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_VICGRID (deprecated)
    (102171, "+proj=lcc +lat_0=-37 +lon_0=145 +lat_1=-36 +lat_2=-38 +x_0=2500000 +y_0=2500000 +ellps=GRS80 +units=m +no_defs"), // GDA_1994_VICGRID94 (deprecated)
    (102172, "+proj=lcc +lat_0=-32 +lon_0=135 +lat_1=-28 +lat_2=-36 +x_0=1000000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // GDA_1994_South_Australia_Lambert (deprecated)
    (102173, "+proj=tmerc +lat_0=0 +lon_0=19 +k=0.9993 +x_0=500000 +y_0=-5300000 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UWPP_1992
    (102174, "+proj=tmerc +lat_0=0 +lon_0=15 +k=0.999923 +x_0=5500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UWPP_2000_PAS_5
    (102175, "+proj=tmerc +lat_0=0 +lon_0=18 +k=0.999923 +x_0=6500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UWPP_2000_PAS_6
    (102176, "+proj=tmerc +lat_0=0 +lon_0=21 +k=0.999923 +x_0=7500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UWPP_2000_PAS_7
    (102177, "+proj=tmerc +lat_0=0 +lon_0=24 +k=0.999923 +x_0=8500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UWPP_2000_PAS_8
    (102184, "+proj=tmerc +lat_0=0 +lon_0=-115 +k=0.9992 +x_0=500000 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_10TM_AEP_Forest (deprecated)
    (102185, "+proj=tmerc +lat_0=0 +lon_0=-115 +k=0.9992 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_10TM_AEP_Resource (deprecated)
    (102186, "+proj=tmerc +lat_0=0 +lon_0=-111 +k=0.9999 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_3TM_111 (deprecated)
    (102187, "+proj=tmerc +lat_0=0 +lon_0=-114 +k=0.9999 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_3TM_114 (deprecated)
    (102188, "+proj=tmerc +lat_0=0 +lon_0=-117 +k=0.9999 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_3TM_117 (deprecated)
    (102189, "+proj=tmerc +lat_0=0 +lon_0=-120 +k=0.9999 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_3TM_120 (deprecated)
    (102190, "+proj=aea +lat_0=45 +lon_0=-126 +lat_1=50 +lat_2=58.5 +x_0=1000000 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_BC_Environment_Albers (deprecated)
    (102191, "+proj=lcc +lat_1=37 +lat_0=37 +lon_0=-5.4 +k_0=0.999625769 +x_0=500000 +y_0=300000 +ellps=clrk80ign +towgs84=31,146,47,0,0,0,0 +units=m +no_defs"), // Nord_Maroc (deprecated)
    (102192, "+proj=lcc +lat_1=33 +lat_0=33 +lon_0=-5.4 +k_0=0.999615596 +x_0=500000 +y_0=300000 +ellps=clrk80ign +towgs84=31,146,47,0,0,0,0 +units=m +no_defs"), // Sud_Maroc (deprecated)
    (102193, "+proj=lcc +lat_1=26.1 +lat_0=26.1 +lon_0=-5.4 +k_0=0.9996 +x_0=1200000 +y_0=400000 +ellps=clrk80ign +towgs84=31,146,47,0,0,0,0 +units=m +no_defs"), // Sahara_Degree (deprecated)
    (102194, "+proj=tmerc +lat_0=0 +lon_0=19 +k=0.9993 +x_0=500000 +y_0=-5300000 +ellps=GRS80 +units=m +no_defs"), // UWPP_1992
    (102195, "+proj=tmerc +lat_0=0 +lon_0=15 +k=0.999923 +x_0=5500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // UWPP_2000_PAS_5
    (102196, "+proj=tmerc +lat_0=0 +lon_0=18 +k=0.999923 +x_0=6500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // UWPP_2000_PAS_6
    (102197, "+proj=tmerc +lat_0=0 +lon_0=21 +k=0.999923 +x_0=7500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // UWPP_2000_PAS_7
    (102198, "+proj=tmerc +lat_0=0 +lon_0=24 +k=0.999923 +x_0=8500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // UWPP_2000_PAS_8
    (102199, "+proj=lcc +lat_0=50.797815 +lon_0=4.35921583333333 +lat_1=49.8333333333333 +lat_2=51.1666666666667 +x_0=649328 +y_0=665262 +ellps=GRS80 +units=m +no_defs"), // Belge_Lambert_2008 (deprecated)
    (102200, "+proj=utm +zone=2 +south +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_UTM_Zone_2S (deprecated)
    (102201, "+proj=tmerc +lat_0=13.5 +lon_0=144.75 +k=1 +x_0=100000 +y_0=200000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Guam_Map_Grid (deprecated)
    (102202, "+proj=utm +zone=4 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_UTM_Zone_4N (deprecated)
    (102203, "+proj=utm +zone=5 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_UTM_Zone_5N (deprecated)
    (102204, "+proj=lcc +lat_0=24 +lon_0=45 +lat_1=21 +lat_2=27 +x_0=1000000 +y_0=3000000 +ellps=intl +towgs84=-143,-236,7,0,0,0,0 +units=m +no_defs"), // Ain_el_Abd_1970_Aramco_Lambert_2
    (102205, "+proj=utm +zone=11 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_UTM_Zone_11N (deprecated)
    (102206, "+proj=utm +zone=12 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_UTM_Zone_12N (deprecated)
    (102207, "+proj=utm +zone=13 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_UTM_Zone_13N (deprecated)
    (102208, "+proj=tmerc +lat_0=43.8333333333333 +lon_0=-67.875 +k=0.99998 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Maine_2000_East_Zone (deprecated)
    (102209, "+proj=tmerc +lat_0=43.5 +lon_0=-69.125 +k=0.99998 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Maine_2000_Central_Zone (deprecated)
    (102210, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.375 +k=0.99998 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Maine_2000_West_Zone (deprecated)
    (102211, "+proj=utm +zone=18 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_UTM_Zone_18N (deprecated)
    (102212, "+proj=lcc +lat_0=41 +lon_0=-107.5 +lat_1=41 +lat_2=45 +x_0=500000 +y_0=200000 +datum=NAD83 +units=m +no_defs"), // NAD_1983_WyLAM (deprecated)
    (102213, "+proj=utm +zone=58 +datum=NAD83 +units=m +no_defs"), // NAD_1983_UTM_Zone_58N
    (102214, "+proj=lcc +lat_0=15.8333333333333 +lon_0=-24 +lat_1=15 +lat_2=16.6666666666667 +x_0=161587.83 +y_0=128511.202 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Cape_Verde_Grid (deprecated)
    (102215, "+proj=lcc +lat_0=0 +lon_0=-95 +lat_1=49 +lat_2=77 +x_0=0 +y_0=-8000000 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Canada_Atlas_LCC
    (102216, "+proj=tmerc +lat_0=0 +lon_0=115.816666666667 +k=0.99999906 +x_0=50000 +y_0=3800000 +ellps=GRS80 +units=m +no_defs"), // GDA_1994_Perth_Coastal_Grid_1994
    (102217, "+proj=tmerc +lat_0=0 +lon_0=-90 +k=0.9996 +x_0=520000 +y_0=-4480000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_NSRS2007_Wisconsin_TM_US_Ft
    (102218, "+proj=aea +lat_0=34 +lon_0=-120 +lat_1=43 +lat_2=48 +x_0=600000 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_USFS_R6_Albers (deprecated)
    (102219, "+proj=tmerc +lat_0=0 +lon_0=-90 +k=0.9996 +x_0=520000 +y_0=-4480000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_Wisconsin_TM_US_Ft
    (102220, "+proj=tmerc +lat_0=0 +lon_0=-90 +k=0.9996 +x_0=520000 +y_0=-4480000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Wisconsin_TM_US_Ft
    (102221, "+proj=lcc +lat_0=10.4666666666667 +lon_0=-84.3333333333333 +lat_1=9.93333333333333 +lat_2=11 +x_0=500000 +y_0=271820.522 +ellps=clrk66 +towgs84=205,96,-98,0,0,0,0 +units=m +no_defs"), // Ocotepeque_1935_Costa_Rica_Lambert_Norte
    (102222, "+proj=lcc +lat_0=9 +lon_0=-83.6666666666667 +lat_1=8.46666666666667 +lat_2=9.53333333333333 +x_0=500000 +y_0=327987.436 +ellps=clrk66 +towgs84=205,96,-98,0,0,0,0 +units=m +no_defs"), // Ocotepeque_1935_Costa_Rica_Lambert_Sur
    (102223, "+proj=tmerc +lat_0=0 +lon_0=-84 +k=0.9996 +x_0=500000 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Costa_Rica_TM_90
    (102224, "+proj=utm +zone=46 +ellps=GRS80 +units=m +no_defs"), // MONREF_1997_UTM_Zone_46N
    (102225, "+proj=utm +zone=47 +ellps=GRS80 +units=m +no_defs"), // MONREF_1997_UTM_Zone_47N
    (102226, "+proj=utm +zone=48 +ellps=GRS80 +units=m +no_defs"), // MONREF_1997_UTM_Zone_48N
    (102227, "+proj=utm +zone=49 +ellps=GRS80 +units=m +no_defs"), // MONREF_1997_UTM_Zone_49N
    (102228, "+proj=utm +zone=50 +ellps=GRS80 +units=m +no_defs"), // MONREF_1997_UTM_Zone_50N
    (102229, "+proj=tmerc +lat_0=30.5 +lon_0=-85.8333333333333 +k=0.99996 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Alabama_East_FIPS_0101 (deprecated)
    (102230, "+proj=tmerc +lat_0=30 +lon_0=-87.5 +k=0.999933333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Alabama_West_FIPS_0102 (deprecated)
    (102231, "+proj=tmerc +lat_0=4.59904722222222 +lon_0=-80.0809166666667 +k=1 +x_0=1000000 +y_0=1000000 +ellps=intl +towgs84=307,304,-318,0,0,0,0 +units=m +no_defs"), // Colombia_West_West_Zone
    (102232, "+proj=tmerc +lat_0=4.68333333333333 +lon_0=-74.15 +k=1.00039978753252 +x_0=92334.879 +y_0=109320.965 +ellps=intl +towgs84=307,304,-318,0,0,0,0 +units=m +no_defs"), // Bogota_Ciudad_Bogota
    (102233, "+proj=tmerc +lat_0=4.68048611111111 +lon_0=-74.1465916666667 +k=1.00039980326544 +x_0=92334.879 +y_0=109320.965 +ellps=GRS80 +units=m +no_defs"), // MAGNA_Ciudad_Bogota
    (102234, "+proj=utm +zone=14 +ellps=GRS80 +towgs84=-0.991,1.9072,0.5129,0.02578990752,0.00965009896,0.01165994323,0 +units=m +no_defs"), // NAD_1983_CSRS_UTM_Zone_14N (deprecated)
    (102235, "+proj=utm +zone=15 +ellps=GRS80 +towgs84=-0.991,1.9072,0.5129,0.02578990752,0.00965009896,0.01165994323,0 +units=m +no_defs"), // NAD_1983_CSRS_UTM_Zone_15N (deprecated)
    (102236, "+proj=utm +zone=16 +ellps=GRS80 +towgs84=-0.991,1.9072,0.5129,0.02578990752,0.00965009896,0.01165994323,0 +units=m +no_defs"), // NAD_1983_CSRS_UTM_Zone_16N (deprecated)
    (102237, "+proj=aeqd +lat_0=6.96507569444444 +lon_0=158.209299222222 +x_0=80122.82 +y_0=80747.24 +ellps=clrk66 +towgs84=-89.121,-348.182,260.871,0,0,0,0 +units=m +no_defs"), // Pohnpei_Az_Eq_1971
    (102238, "+proj=aeqd +lat_0=15.1675572222222 +lon_0=145.711286944444 +x_0=50000 +y_0=50000 +ellps=clrk66 +towgs84=-100,-248,259,0,0,0,0 +units=m +no_defs"), // Saipan_Az_Eq_1969
    (102239, "+proj=aeqd +lat_0=13.4724663527778 +lon_0=144.748750705556 +x_0=50000 +y_0=50000 +ellps=clrk66 +towgs84=-100,-248,259,0,0,0,0 +units=m +no_defs"), // Guam_Geodetic_Triangulation_Network_1963
    (102240, "+proj=tmerc +lat_0=13.5 +lon_0=144.75 +k=1 +x_0=100000 +y_0=200000 +ellps=GRS80 +units=m +no_defs"), // Guam_Geodetic_Network_1993
    (102241, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-122 +lat_1=40 +lat_2=41.6666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_California_I_FIPS_0401 (deprecated)
    (102242, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-122 +lat_1=38.3333333333333 +lat_2=39.8333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_California_II_FIPS_0402 (deprecated)
    (102243, "+proj=lcc +lat_0=36.5 +lon_0=-120.5 +lat_1=37.0666666666667 +lat_2=38.4333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_California_III_FIPS_0403 (deprecated)
    (102244, "+proj=lcc +lat_0=35.3333333333333 +lon_0=-119 +lat_1=36 +lat_2=37.25 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_California_IV_FIPS_0404 (deprecated)
    (102245, "+proj=lcc +lat_0=33.5 +lon_0=-118 +lat_1=34.0333333333333 +lat_2=35.4666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_California_V_FIPS_0405 (deprecated)
    (102246, "+proj=lcc +lat_0=32.1666666666667 +lon_0=-116.25 +lat_1=32.7833333333333 +lat_2=33.8833333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_California_VI_FIPS_0406 (deprecated)
    (102247, "+proj=aea +lat_0=50 +lon_0=-154 +lat_1=55 +lat_2=65 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_Alaska_Albers
    (102248, "+proj=tmerc +lat_0=31 +lon_0=-110.166666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Arizona_East_FIPS_0201 (deprecated)
    (102249, "+proj=tmerc +lat_0=31 +lon_0=-111.916666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Arizona_Central_FIPS_0202 (deprecated)
    (102250, "+proj=tmerc +lat_0=31 +lon_0=-113.75 +k=0.999933333333333 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Arizona_West_FIPS_0203 (deprecated)
    (102251, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-92 +lat_1=34.9333333333333 +lat_2=36.2333333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Arkansas_North_FIPS_0301 (deprecated)
    (102252, "+proj=lcc +lat_0=32.6666666666667 +lon_0=-92 +lat_1=33.3 +lat_2=34.7666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Arkansas_South_FIPS_0302 (deprecated)
    (102253, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-105.5 +lat_1=39.7166666666667 +lat_2=40.7833333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Colorado_North_FIPS_0501 (deprecated)
    (102254, "+proj=lcc +lat_0=37.8333333333333 +lon_0=-105.5 +lat_1=38.45 +lat_2=39.75 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Colorado_Central_FIPS_0502 (deprecated)
    (102255, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-105.5 +lat_1=37.2333333333333 +lat_2=38.4333333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Colorado_South_FIPS_0503 (deprecated)
    (102256, "+proj=lcc +lat_0=40.8333333333333 +lon_0=-72.75 +lat_1=41.2 +lat_2=41.8666666666667 +x_0=304800.6096 +y_0=152400.3048 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Connecticut_FIPS_0600 (deprecated)
    (102257, "+proj=tmerc +lat_0=38 +lon_0=-75.4166666666667 +k=0.999995 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Delaware_FIPS_0700 (deprecated)
    (102258, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-81 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Florida_East_FIPS_0901 (deprecated)
    (102259, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-82 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Florida_West_FIPS_0902 (deprecated)
    (102260, "+proj=lcc +lat_0=29 +lon_0=-84.5 +lat_1=29.5833333333333 +lat_2=30.75 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Florida_North_FIPS_0903 (deprecated)
    (102261, "+proj=tmerc +lat_0=18.8333333333333 +lon_0=-155.5 +k=0.999966666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_1_FIPS_5101 (deprecated)
    (102262, "+proj=tmerc +lat_0=20.3333333333333 +lon_0=-156.666666666667 +k=0.999966666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_2_FIPS_5102 (deprecated)
    (102263, "+proj=tmerc +lat_0=21.1666666666667 +lon_0=-158 +k=0.99999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_3_FIPS_5103 (deprecated)
    (102264, "+proj=tmerc +lat_0=21.8333333333333 +lon_0=-159.5 +k=0.99999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_4_FIPS_5104 (deprecated)
    (102265, "+proj=tmerc +lat_0=21.6666666666667 +lon_0=-160.166666666667 +k=1 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_5_FIPS_5105 (deprecated)
    (102266, "+proj=tmerc +lat_0=30 +lon_0=-82.1666666666667 +k=0.9999 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Georgia_East_FIPS_1001 (deprecated)
    (102267, "+proj=tmerc +lat_0=30 +lon_0=-84.1666666666667 +k=0.9999 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Georgia_West_FIPS_1002 (deprecated)
    (102268, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-112.166666666667 +k=0.999947368421053 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Idaho_East_FIPS_1101 (deprecated)
    (102269, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-114 +k=0.999947368421053 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Idaho_Central_FIPS_1102 (deprecated)
    (102270, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-115.75 +k=0.999933333333333 +x_0=800000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Idaho_West_FIPS_1103 (deprecated)
    (102271, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-88.3333333333333 +k=0.999975 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Illinois_East_FIPS_1201 (deprecated)
    (102272, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-90.1666666666667 +k=0.999941176470588 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Illinois_West_FIPS_1202 (deprecated)
    (102273, "+proj=tmerc +lat_0=37.5 +lon_0=-85.6666666666667 +k=0.999966666666667 +x_0=100000 +y_0=250000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Indiana_East_FIPS_1301 (deprecated)
    (102274, "+proj=tmerc +lat_0=37.5 +lon_0=-87.0833333333333 +k=0.999966666666667 +x_0=900000 +y_0=250000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Indiana_West_FIPS_1302 (deprecated)
    (102275, "+proj=lcc +lat_0=41.5 +lon_0=-93.5 +lat_1=42.0666666666667 +lat_2=43.2666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Iowa_North_FIPS_1401 (deprecated)
    (102276, "+proj=lcc +lat_0=40 +lon_0=-93.5 +lat_1=40.6166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Iowa_South_FIPS_1402 (deprecated)
    (102277, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-98 +lat_1=38.7166666666667 +lat_2=39.7833333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Kansas_North_FIPS_1501 (deprecated)
    (102278, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-98.5 +lat_1=37.2666666666667 +lat_2=38.5666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Kansas_South_FIPS_1502 (deprecated)
    (102279, "+proj=lcc +lat_0=37.5 +lon_0=-84.25 +lat_1=37.9666666666667 +lat_2=38.9666666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Kentucky_North_FIPS_1601 (deprecated)
    (102280, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=36.7333333333333 +lat_2=37.9333333333333 +x_0=500000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Kentucky_South_FIPS_1602 (deprecated)
    (102281, "+proj=lcc +lat_0=30.5 +lon_0=-92.5 +lat_1=31.1666666666667 +lat_2=32.6666666666667 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Louisiana_North_FIPS_1701 (deprecated)
    (102282, "+proj=lcc +lat_0=28.5 +lon_0=-91.3333333333333 +lat_1=29.3 +lat_2=30.7 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Louisiana_South_FIPS_1702 (deprecated)
    (102283, "+proj=tmerc +lat_0=43.6666666666667 +lon_0=-68.5 +k=0.9999 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Maine_East_FIPS_1801 (deprecated)
    (102284, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.1666666666667 +k=0.999966666666667 +x_0=900000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Maine_West_FIPS_1802 (deprecated)
    (102285, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-77 +lat_1=38.3 +lat_2=39.45 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Maryland_FIPS_1900 (deprecated)
    (102286, "+proj=lcc +lat_0=41 +lon_0=-71.5 +lat_1=41.7166666666667 +lat_2=42.6833333333333 +x_0=200000 +y_0=750000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Massachusetts_Mainland_FIPS_2001 (deprecated)
    (102287, "+proj=lcc +lat_0=41 +lon_0=-70.5 +lat_1=41.2833333333333 +lat_2=41.4833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Massachusetts_Island_FIPS_2002 (deprecated)
    (102288, "+proj=lcc +lat_0=44.7833333333333 +lon_0=-87 +lat_1=45.4833333333333 +lat_2=47.0833333333333 +x_0=8000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Michigan_North_FIPS_2111 (deprecated)
    (102289, "+proj=lcc +lat_0=43.3166666666667 +lon_0=-84.3666666666667 +lat_1=44.1833333333333 +lat_2=45.7 +x_0=6000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Michigan_Central_FIPS_2112 (deprecated)
    (102290, "+proj=lcc +lat_0=41.5 +lon_0=-84.3666666666667 +lat_1=42.1 +lat_2=43.6666666666667 +x_0=4000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Michigan_South_FIPS_2113 (deprecated)
    (102291, "+proj=lcc +lat_0=46.5 +lon_0=-93.1 +lat_1=47.0333333333333 +lat_2=48.6333333333333 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Minnesota_North_FIPS_2201 (deprecated)
    (102292, "+proj=lcc +lat_0=45 +lon_0=-94.25 +lat_1=45.6166666666667 +lat_2=47.05 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Minnesota_Central_FIPS_2202 (deprecated)
    (102293, "+proj=lcc +lat_0=43 +lon_0=-94 +lat_1=43.7833333333333 +lat_2=45.2166666666667 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Minnesota_South_FIPS_2203 (deprecated)
    (102294, "+proj=tmerc +lat_0=29.5 +lon_0=-88.8333333333333 +k=0.99995 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Mississippi_East_FIPS_2301 (deprecated)
    (102295, "+proj=tmerc +lat_0=29.5 +lon_0=-90.3333333333333 +k=0.99995 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Mississippi_West_FIPS_2302 (deprecated)
    (102296, "+proj=tmerc +lat_0=35.8333333333333 +lon_0=-90.5 +k=0.999933333333333 +x_0=250000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Missouri_East_FIPS_2401 (deprecated)
    (102297, "+proj=tmerc +lat_0=35.8333333333333 +lon_0=-92.5 +k=0.999933333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Missouri_Central_FIPS_2402 (deprecated)
    (102298, "+proj=tmerc +lat_0=36.1666666666667 +lon_0=-94.5 +k=0.999941176470588 +x_0=850000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Missouri_West_FIPS_2403 (deprecated)
    (102300, "+proj=lcc +lat_0=44.25 +lon_0=-109.5 +lat_1=45 +lat_2=49 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Montana_FIPS_2500 (deprecated)
    (102304, "+proj=lcc +lat_0=39.8333333333333 +lon_0=-100 +lat_1=40 +lat_2=43 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Nebraska_FIPS_2600 (deprecated)
    (102305, "+proj=tmerc +lat_0=0 +lon_0=-84 +k=0.9999 +x_0=500000 +y_0=0 +ellps=WGS84 +towgs84=-0.16959,0.35312,0.51846,0.03385,-0.16325,0.03446,0.03693 +units=m +no_defs"), // CRTM05 (deprecated)
    (102306, "+proj=tmerc +lat_0=0 +lon_0=84 +k=0.9999 +x_0=500000 +y_0=0 +ellps=evrst30 +towgs84=293.17,726.18,245.36,0,0,0,0 +units=m +no_defs"), // Nepal_Nagarkot_TM
    (102307, "+proj=tmerc +lat_0=34.75 +lon_0=-115.583333333333 +k=0.9999 +x_0=200000 +y_0=8000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Nevada_East_FIPS_2701 (deprecated)
    (102308, "+proj=tmerc +lat_0=34.75 +lon_0=-116.666666666667 +k=0.9999 +x_0=500000 +y_0=6000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Nevada_Central_FIPS_2702 (deprecated)
    (102309, "+proj=tmerc +lat_0=34.75 +lon_0=-118.583333333333 +k=0.9999 +x_0=800000 +y_0=4000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Nevada_West_FIPS_2703 (deprecated)
    (102310, "+proj=tmerc +lat_0=42.5 +lon_0=-71.6666666666667 +k=0.999966666666667 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_Hampshire_FIPS_2800 (deprecated)
    (102311, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_Jersey_FIPS_2900 (deprecated)
    (102312, "+proj=tmerc +lat_0=31 +lon_0=-104.333333333333 +k=0.999909090909091 +x_0=165000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_Mexico_East_FIPS_3001 (deprecated)
    (102313, "+proj=tmerc +lat_0=31 +lon_0=-106.25 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_Mexico_Central_FIPS_3002 (deprecated)
    (102314, "+proj=tmerc +lat_0=31 +lon_0=-107.833333333333 +k=0.999916666666667 +x_0=830000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_Mexico_West_FIPS_3003 (deprecated)
    (102315, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_York_East_FIPS_3101 (deprecated)
    (102316, "+proj=tmerc +lat_0=40 +lon_0=-76.5833333333333 +k=0.9999375 +x_0=250000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_York_Central_FIPS_3102 (deprecated)
    (102317, "+proj=tmerc +lat_0=40 +lon_0=-78.5833333333333 +k=0.9999375 +x_0=350000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_York_West_FIPS_3103 (deprecated)
    (102318, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-74 +lat_1=40.6666666666667 +lat_2=41.0333333333333 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_New_York_Long_Island_FIPS_3104 (deprecated)
    (102319, "+proj=tmerc +lat_0=0 +lon_0=33 +k=0.99995 +x_0=200000 +y_0=-3500000 +ellps=WGS84 +towgs84=8.846,-4.394,-1.122,-0.00237,-0.146528,0.130428,0.783926 +units=m +no_defs"), // CGRS_1993_LTM
    (102320, "+proj=lcc +lat_0=47 +lon_0=-100.5 +lat_1=47.4333333333333 +lat_2=48.7333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_North_Dakota_North_FIPS_3301 (deprecated)
    (102321, "+proj=lcc +lat_0=45.6666666666667 +lon_0=-100.5 +lat_1=46.1833333333333 +lat_2=47.4833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_North_Dakota_South_FIPS_3302 (deprecated)
    (102322, "+proj=lcc +lat_0=39.6666666666667 +lon_0=-82.5 +lat_1=40.4333333333333 +lat_2=41.7 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Ohio_North_FIPS_3401 (deprecated)
    (102323, "+proj=lcc +lat_0=38 +lon_0=-82.5 +lat_1=38.7333333333333 +lat_2=40.0333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Ohio_South_FIPS_3402 (deprecated)
    (102324, "+proj=lcc +lat_0=35 +lon_0=-98 +lat_1=35.5666666666667 +lat_2=36.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Oklahoma_North_FIPS_3501 (deprecated)
    (102325, "+proj=lcc +lat_0=33.3333333333333 +lon_0=-98 +lat_1=33.9333333333333 +lat_2=35.2333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Oklahoma_South_FIPS_3502 (deprecated)
    (102326, "+proj=lcc +lat_0=43.6666666666667 +lon_0=-120.5 +lat_1=44.3333333333333 +lat_2=46 +x_0=2500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Oregon_North_FIPS_3601 (deprecated)
    (102327, "+proj=lcc +lat_0=41.6666666666667 +lon_0=-120.5 +lat_1=42.3333333333333 +lat_2=44 +x_0=1500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Oregon_South_FIPS_3602 (deprecated)
    (102328, "+proj=tmerc +lat_0=0 +lon_0=9 +k=0.9996 +x_0=2500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UTM_Zone_32N_7stellen
    (102329, "+proj=tmerc +lat_0=0 +lon_0=9 +k=0.9996 +x_0=32500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UTM_Zone_32N_8stellen
    (102330, "+proj=tmerc +lat_0=41.0833333333333 +lon_0=-71.5 +k=0.99999375 +x_0=100000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Rhode_Island_FIPS_3800 (deprecated)
    (102331, "+proj=utm +zone=25 +ellps=GRS80 +units=m +no_defs"), // PTRA08_UTM_Zone_25N (deprecated)
    (102332, "+proj=utm +zone=26 +ellps=GRS80 +units=m +no_defs"), // PTRA08_UTM_Zone_26N (deprecated)
    (102333, "+proj=utm +zone=28 +ellps=GRS80 +units=m +no_defs"), // PTRA08_UTM_Zone_28N (deprecated)
    (102334, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-100 +lat_1=44.4166666666667 +lat_2=45.6833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_South_Dakota_North_FIPS_4001 (deprecated)
    (102335, "+proj=lcc +lat_0=42.3333333333333 +lon_0=-100.333333333333 +lat_1=42.8333333333333 +lat_2=44.4 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_South_Dakota_South_FIPS_4002 (deprecated)
    (102336, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-86 +lat_1=35.25 +lat_2=36.4166666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Tennessee_FIPS_4100 (deprecated)
    (102337, "+proj=lcc +lat_0=34 +lon_0=-101.5 +lat_1=34.65 +lat_2=36.1833333333333 +x_0=200000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Texas_North_FIPS_4201 (deprecated)
    (102338, "+proj=lcc +lat_0=31.6666666666667 +lon_0=-98.5 +lat_1=32.1333333333333 +lat_2=33.9666666666667 +x_0=600000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Texas_North_Central_FIPS_4202 (deprecated)
    (102339, "+proj=lcc +lat_0=29.6666666666667 +lon_0=-100.333333333333 +lat_1=30.1166666666667 +lat_2=31.8833333333333 +x_0=700000 +y_0=3000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Texas_Central_FIPS_4203 (deprecated)
    (102340, "+proj=lcc +lat_0=27.8333333333333 +lon_0=-99 +lat_1=28.3833333333333 +lat_2=30.2833333333333 +x_0=600000 +y_0=4000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Texas_South_Central_FIPS_4204 (deprecated)
    (102341, "+proj=lcc +lat_0=25.6666666666667 +lon_0=-98.5 +lat_1=26.1666666666667 +lat_2=27.8333333333333 +x_0=300000 +y_0=5000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Texas_South_FIPS_4205 (deprecated)
    (102342, "+proj=lcc +lat_0=40.3333333333333 +lon_0=-111.5 +lat_1=40.7166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Utah_North_FIPS_4301 (deprecated)
    (102343, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-111.5 +lat_1=39.0166666666667 +lat_2=40.65 +x_0=500000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Utah_Central_FIPS_4302 (deprecated)
    (102344, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-111.5 +lat_1=37.2166666666667 +lat_2=38.35 +x_0=500000 +y_0=3000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Utah_South_FIPS_4303 (deprecated)
    (102345, "+proj=tmerc +lat_0=42.5 +lon_0=-72.5 +k=0.999964285714286 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Vermont_FIPS_4400 (deprecated)
    (102346, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-78.5 +lat_1=38.0333333333333 +lat_2=39.2 +x_0=3500000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Virginia_North_FIPS_4501 (deprecated)
    (102347, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-78.5 +lat_1=36.7666666666667 +lat_2=37.9666666666667 +x_0=3500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Virginia_South_FIPS_4502 (deprecated)
    (102348, "+proj=lcc +lat_0=47 +lon_0=-120.833333333333 +lat_1=47.5 +lat_2=48.7333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Washington_North_FIPS_4601 (deprecated)
    (102349, "+proj=lcc +lat_0=45.3333333333333 +lon_0=-120.5 +lat_1=45.8333333333333 +lat_2=47.3333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Washington_South_FIPS_4602 (deprecated)
    (102350, "+proj=lcc +lat_0=38.5 +lon_0=-79.5 +lat_1=39 +lat_2=40.25 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_West_Virginia_North_FIPS_4701 (deprecated)
    (102351, "+proj=lcc +lat_0=37 +lon_0=-81 +lat_1=37.4833333333333 +lat_2=38.8833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_West_Virginia_South_FIPS_4702 (deprecated)
    (102352, "+proj=lcc +lat_0=45.1666666666667 +lon_0=-90 +lat_1=45.5666666666667 +lat_2=46.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Wisconsin_North_FIPS_4801 (deprecated)
    (102353, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-90 +lat_1=44.25 +lat_2=45.5 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Wisconsin_Central_FIPS_4802 (deprecated)
    (102354, "+proj=lcc +lat_0=42 +lon_0=-90 +lat_1=42.7333333333333 +lat_2=44.0666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Wisconsin_South_FIPS_4803 (deprecated)
    (102355, "+proj=tmerc +lat_0=40.5 +lon_0=-105.166666666667 +k=0.9999375 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Wyoming_East_FIPS_4901 (deprecated)
    (102356, "+proj=tmerc +lat_0=40.5 +lon_0=-107.333333333333 +k=0.9999375 +x_0=400000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Wyoming_East_Central_FIPS_4902 (deprecated)
    (102357, "+proj=tmerc +lat_0=40.5 +lon_0=-108.75 +k=0.9999375 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Wyoming_West_Central_FIPS_4903 (deprecated)
    (102358, "+proj=tmerc +lat_0=40.5 +lon_0=-110.083333333333 +k=0.9999375 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Wyoming_West_FIPS_4904 (deprecated)
    (102359, "+proj=tmerc +lat_0=0 +lon_0=15 +k=0.9996 +x_0=3500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UTM_Zone_33N_7stellen
    (102360, "+proj=tmerc +lat_0=0 +lon_0=15 +k=0.9996 +x_0=33500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UTM_Zone_33N_8stellen
    (102361, "+proj=lcc +lat_0=17.8333333333333 +lon_0=-66.4333333333333 +lat_1=18.0333333333333 +lat_2=18.4333333333333 +x_0=200000 +y_0=200000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Puerto_Rico_Virgin_Islands_FIPS_5200 (deprecated)
    (102362, "+proj=tmerc +lat_0=0 +lon_0=9 +k=0.9996 +x_0=32500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_UTM_Zone_N32 (deprecated)
    (102363, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=37.0833333333333 +lat_2=38.6666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_StatePlane_Kentucky_FIPS_1600 (deprecated)
    (102364, "+proj=utm +zone=59 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_59N
    (102365, "+proj=utm +zone=60 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_60N
    (102367, "+proj=tmerc +lat_0=54 +lon_0=-142 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_2_FIPS_5002
    (102368, "+proj=tmerc +lat_0=54 +lon_0=-146 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_3_FIPS_5003
    (102369, "+proj=tmerc +lat_0=54 +lon_0=-150 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_4_FIPS_5004
    (102370, "+proj=tmerc +lat_0=54 +lon_0=-154 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_5_FIPS_5005
    (102371, "+proj=tmerc +lat_0=54 +lon_0=-158 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_6_FIPS_5006
    (102372, "+proj=tmerc +lat_0=54 +lon_0=-162 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_7_FIPS_5007
    (102373, "+proj=tmerc +lat_0=54 +lon_0=-166 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_8_FIPS_5008
    (102374, "+proj=tmerc +lat_0=54 +lon_0=-170 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_9_FIPS_5009
    (102375, "+proj=lcc +lat_0=51 +lon_0=-176 +lat_1=51.8333333333333 +lat_2=53.8333333333333 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alaska_10_FIPS_5010
    (102376, "+proj=lcc +lat_0=43.6666666666667 +lon_0=-120.5 +lat_1=44.3333333333333 +lat_2=46 +x_0=2500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Oregon_North_FIPS_3601 (deprecated)
    (102377, "+proj=lcc +lat_0=41.6666666666667 +lon_0=-120.5 +lat_1=42.3333333333333 +lat_2=44 +x_0=1500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Oregon_South_FIPS_3602 (deprecated)
    (102378, "+proj=lcc +lat_0=43.6666666666667 +lon_0=-120.5 +lat_1=44.3333333333333 +lat_2=46 +x_0=2500000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Oregon_North_FIPS_3601_Ft_Intl (deprecated)
    (102379, "+proj=lcc +lat_0=41.6666666666667 +lon_0=-120.5 +lat_1=42.3333333333333 +lat_2=44 +x_0=1500000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Oregon_South_FIPS_3602_Ft_Intl (deprecated)
    (102380, "+proj=lcc +lat_0=41.75 +lon_0=-120.5 +lat_1=43 +lat_2=45.5 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_Oregon_Statewide_Lambert (deprecated)
    (102381, "+proj=lcc +lat_0=41.75 +lon_0=-120.5 +lat_1=43 +lat_2=45.5 +x_0=400000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_Oregon_Statewide_Lambert_Ft_Intl (deprecated)
    (102382, "+proj=utm +zone=13 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_13N (deprecated)
    (102383, "+proj=utm +zone=14 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_14N (deprecated)
    (102384, "+proj=utm +zone=15 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_15N (deprecated)
    (102385, "+proj=utm +zone=16 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_16N (deprecated)
    (102386, "+proj=utm +zone=17 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_17N (deprecated)
    (102387, "+proj=utm +zone=18 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_18N (deprecated)
    (102388, "+proj=utm +zone=19 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_UTM_Zone_19N (deprecated)
    (102392, "+proj=tmerc +lat_0=54 +lon_0=-150 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_4_FIPS_5004_Feet
    (102393, "+proj=tmerc +lat_0=54 +lon_0=-154 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_5_FIPS_5005_Feet
    (102394, "+proj=tmerc +lat_0=54 +lon_0=-158 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_6_FIPS_5006_Feet
    (102395, "+proj=tmerc +lat_0=54 +lon_0=-162 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_7_FIPS_5007_Feet
    (102396, "+proj=tmerc +lat_0=54 +lon_0=-166 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_8_FIPS_5008_Feet
    (102397, "+proj=tmerc +lat_0=54 +lon_0=-170 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_9_FIPS_5009_Feet
    (102398, "+proj=lcc +lat_0=51 +lon_0=-176 +lat_1=51.8333333333333 +lat_2=53.8333333333333 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_10_FIPS_5010_Feet
    (102399, "+proj=aea +lat_0=42.5 +lon_0=-106 +lat_1=43.5 +lat_2=48 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Albers_BLM_MT_ND_SD
    (102401, "+proj=utm +zone=1 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_1N
    (102402, "+proj=utm +zone=2 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_2N
    (102403, "+proj=utm +zone=3 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_3N
    (102404, "+proj=utm +zone=4 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_4N
    (102405, "+proj=utm +zone=5 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_5N
    (102406, "+proj=utm +zone=6 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_6N
    (102407, "+proj=utm +zone=7 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_7N
    (102408, "+proj=utm +zone=8 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_8N
    (102409, "+proj=utm +zone=9 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_9N
    (102410, "+proj=utm +zone=10 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_10N
    (102411, "+proj=utm +zone=11 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_11N
    (102412, "+proj=utm +zone=12 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_12N
    (102413, "+proj=utm +zone=13 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_13N
    (102414, "+proj=utm +zone=14 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_14N
    (102415, "+proj=utm +zone=15 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_15N
    (102416, "+proj=utm +zone=16 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_16N
    (102417, "+proj=utm +zone=17 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_17N
    (102418, "+proj=utm +zone=18 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_18N
    (102419, "+proj=utm +zone=19 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_UTM_Zone_19N
    (102420, "+proj=lcc +lat_0=65 +lon_0=-19 +lat_1=64.25 +lat_2=65.75 +x_0=1700000 +y_0=300000 +ellps=GRS80 +units=m +no_defs"), // ISN_2004_Lambert_2004 (deprecated)
    (102421, "+proj=eqc +lat_ts=22.94791772 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_01
    (102422, "+proj=eqc +lat_ts=41.12682127 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_02
    (102423, "+proj=eqc +lat_ts=52.28859923 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_03
    (102424, "+proj=eqc +lat_ts=60.32378942 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_04
    (102425, "+proj=eqc +lat_ts=66.09421768 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_05
    (102426, "+proj=eqc +lat_ts=70.10896259 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_06
    (102427, "+proj=eqc +lat_ts=74.13230145 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_07
    (102428, "+proj=eqc +lat_ts=78.1728375 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_08
    (102429, "+proj=aeqd +lat_0=90 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_09
    (102430, "+proj=eqc +lat_ts=-22.94791772 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_10
    (102431, "+proj=eqc +lat_ts=-41.12682127 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_11
    (102432, "+proj=eqc +lat_ts=-52.28859923 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_12
    (102433, "+proj=eqc +lat_ts=-60.32378942 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_13
    (102434, "+proj=eqc +lat_ts=-66.09421768 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_14
    (102435, "+proj=eqc +lat_ts=-70.10896259 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_15
    (102436, "+proj=eqc +lat_ts=-74.13230145 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_16
    (102437, "+proj=eqc +lat_ts=-78.1728375 +lat_0=0 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_17
    (102438, "+proj=aeqd +lat_0=-90 +lon_0=0 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_ARC_System_Zone_18
    (102439, "+proj=lcc +lat_0=-27 +lon_0=132 +lat_1=-18 +lat_2=-36 +x_0=0 +y_0=0 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Australian_Centre_for_Remote_Sensing_Lambert (deprecated)
    (102440, "+proj=tmerc +lat_0=0 +lon_0=24 +k=0.9996 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // LKS_1992_Latvia_TM_0
    (102441, "+proj=tmerc +lat_0=0 +lon_0=121 +k=0.9999 +x_0=250000 +y_0=0 +ellps=aust_SA +units=m +no_defs"), // TWD_1967_TM_Taiwan (deprecated)
    (102442, "+proj=tmerc +lat_0=0 +lon_0=119 +k=0.9999 +x_0=250000 +y_0=0 +ellps=aust_SA +units=m +no_defs"), // TWD_1967_TM_Penghu (deprecated)
    (102443, "+proj=tmerc +lat_0=0 +lon_0=121 +k=0.9999 +x_0=250000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // TWD_1997_TM_Taiwan (deprecated)
    (102444, "+proj=tmerc +lat_0=0 +lon_0=119 +k=0.9999 +x_0=250000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // TWD_1997_TM_Penghu (deprecated)
    (102446, "+proj=tmerc +lat_0=54 +lon_0=-142 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_2_FIPS_5002_Feet
    (102447, "+proj=tmerc +lat_0=54 +lon_0=-146 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Alaska_3_FIPS_5003_Feet
    (102448, "+proj=tmerc +lat_0=22.2123972222222 +lon_0=113.536469444444 +k=1 +x_0=20000 +y_0=20000 +ellps=GRS80 +units=m +no_defs"), // Macao_2008_Macao_Grid
    (102449, "+proj=utm +zone=55 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_MA11_UTM_Zone_55N (deprecated)
    (102453, "+proj=utm +zone=50 +ellps=clrk66 +towgs84=-133,-77,-51,0,0,0,0 +units=m +no_defs"), // Luzon_1911_UTM_Zone_50N
    (102454, "+proj=utm +zone=51 +ellps=clrk66 +towgs84=-133,-77,-51,0,0,0,0 +units=m +no_defs"), // Luzon_1911_UTM_Zone_51N
    (102455, "+proj=utm +zone=52 +ellps=clrk66 +towgs84=-133,-77,-51,0,0,0,0 +units=m +no_defs"), // Luzon_1911_UTM_Zone_52N
    (102456, "+proj=utm +zone=50 +ellps=clrk66 +towgs84=-127.62,-67.24,-47.04,-3.068,4.903,1.578,-1.06 +units=m +no_defs"), // PRS_1992_UTM_Zone_50N
    (102457, "+proj=utm +zone=51 +ellps=clrk66 +towgs84=-127.62,-67.24,-47.04,-3.068,4.903,1.578,-1.06 +units=m +no_defs"), // PRS_1992_UTM_Zone_51N
    (102458, "+proj=utm +zone=52 +ellps=clrk66 +towgs84=-127.62,-67.24,-47.04,-3.068,4.903,1.578,-1.06 +units=m +no_defs"), // PRS_1992_UTM_Zone_52N
    (102459, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-115.75 +k=1.00011328 +x_0=800143.9664 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_Idaho-Ada_County
    (102461, "+proj=tmerc +lat_0=18.8333333333333 +lon_0=-155.5 +k=0.999966666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_1_FIPS_5101_Feet
    (102462, "+proj=tmerc +lat_0=20.3333333333333 +lon_0=-156.666666666667 +k=0.999966666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_2_FIPS_5102_Feet
    (102463, "+proj=tmerc +lat_0=21.1666666666667 +lon_0=-158 +k=0.99999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_3_FIPS_5103_Feet (deprecated)
    (102464, "+proj=tmerc +lat_0=21.8333333333333 +lon_0=-159.5 +k=0.99999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_4_FIPS_5104_Feet
    (102465, "+proj=tmerc +lat_0=21.6666666666667 +lon_0=-160.166666666667 +k=1 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_StatePlane_Hawaii_5_FIPS_5105_Feet
    (102466, "+proj=lcc +lat_0=46.5 +lon_0=-93.1 +lat_1=47.0333333333333 +lat_2=48.6333333333333 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_StatePlane_Minnesota_North_FIPS_2201_Feet (deprecated)
    (102467, "+proj=lcc +lat_0=45 +lon_0=-94.25 +lat_1=45.6166666666667 +lat_2=47.05 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_StatePlane_Minnesota_Central_FIPS_2202_Feet (deprecated)
    (102468, "+proj=lcc +lat_0=43 +lon_0=-94 +lat_1=43.7833333333333 +lat_2=45.2166666666667 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_StatePlane_Minnesota_South_FIPS_2203_Feet (deprecated)
    (102469, "+proj=tmerc +lat_0=32.5 +lon_0=-89.75 +k=0.9998335 +x_0=500000 +y_0=1300000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Mississippi_TM (deprecated)
    (102480, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=15 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo15
    (102481, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=17 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo17
    (102482, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=19 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo19
    (102483, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=21 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo21
    (102484, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=23 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo23
    (102485, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=25 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo25
    (102486, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=27 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo27
    (102487, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=29 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo29
    (102488, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=31 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo31
    (102489, "+proj=tmerc +axis=wsu +lat_0=0 +lon_0=33 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo33
    (102491, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=2.7 +k_0=0.999625544 +x_0=500000 +y_0=300000 +ellps=clrk80ign +towgs84=-73,-247,227,0,0,0,0 +units=m +no_defs"), // Nord_Algerie_Ancienne_Degree
    (102492, "+proj=lcc +lat_1=33.3 +lat_0=33.3 +lon_0=2.7 +k_0=0.999625769 +x_0=500000 +y_0=300000 +ellps=clrk80ign +towgs84=-73,-247,227,0,0,0,0 +units=m +no_defs"), // Sud_Algerie_Ancienne_Degree
    (102493, "+proj=utm +zone=4 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PA11_UTM_Zone_4N (deprecated)
    (102494, "+proj=utm +zone=5 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PA11_UTM_Zone_5N (deprecated)
    (102495, "+proj=tmerc +lat_0=13.5 +lon_0=144.75 +k=1 +x_0=100000 +y_0=200000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_MA11_Guam_Map_Grid (deprecated)
    (102496, "+proj=utm +zone=2 +south +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PA11_UTM_Zone_2S (deprecated)
    (102497, "+proj=geos +lon_0=-75 +h=35786023 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // GOES-16_East_ABI_Fixed_Grid_ITRF2000_incorrect_GCS (deprecated)
    (102498, "+proj=geos +lon_0=-75 +h=35786023 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // GOES-16_East_ABI_Fixed_Grid_ITRF2008
    (102499, "+proj=lcc +lat_0=90 +lon_0=4.36748666666667 +lat_1=49.8333339 +lat_2=51.1666672333333 +x_0=150000.01256 +y_0=5400088.4378 +ellps=intl +towgs84=-106.8686,52.2978,-103.7239,0.3366,-0.457,1.8422,-1.2747 +units=m +no_defs"), // Belge_Lambert_1972_bad_FE_FN
    (102500, "+proj=tmerc +lat_0=44.5 +lon_0=-117.833333333333 +k=1.00016 +x_0=40000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Baker_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102501, "+proj=lcc +lat_1=43.6666666666667 +lat_0=43.6666666666667 +lon_0=-119.75 +k_0=1.0002 +x_0=120000 +y_0=60000 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Bend-Burns_NAD_1983_CORS96_LCC_Feet_Intl (deprecated)
    (102502, "+proj=tmerc +lat_0=41.75 +lon_0=-121.75 +k=1.0002 +x_0=80000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Bend-Klamath_Falls_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102503, "+proj=lcc +lat_1=44.6666666666667 +lat_0=44.6666666666667 +lon_0=-121.25 +k_0=1.00012 +x_0=80000 +y_0=130000 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Bend-Redmond-Prineville_NAD_1983_CORS96_LCC_Feet_Intl (deprecated)
    (102504, "+proj=tmerc +lat_0=42.5 +lon_0=-123.333333333333 +k=1.00007 +x_0=40000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Canyonville-Grants_Pass_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102505, "+proj=lcc +lat_1=45.6666666666667 +lat_0=45.6666666666667 +lon_0=-120.5 +k_0=1.000008 +x_0=150000 +y_0=30000 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Columbia_River_East_NAD_1983_CORS96_LCC_Feet_Intl (deprecated)
    (102507, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-123.333333333333 +k=1.000023 +x_0=50000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Cottage_Grove-Canyonville_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102508, "+proj=tmerc +lat_0=44.5 +lon_0=-121 +k=1.00011 +x_0=80000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Dufur-Madras_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102509, "+proj=tmerc +lat_0=43.75 +lon_0=-123.166666666667 +k=1.000015 +x_0=50000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Eugene_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102510, "+proj=tmerc +lat_0=41.75 +lon_0=-123.333333333333 +k=1.000043 +x_0=50000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Grants_Pass-Ashland_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102511, "+proj=tmerc +lat_0=45 +lon_0=-122.333333333333 +k=1.00005 +x_0=10000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Gresham-Warm_Springs_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102512, "+proj=tmerc +lat_0=45 +lon_0=-118 +k=1.00013 +x_0=40000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_La_Grande_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102513, "+proj=tmerc +lat_0=43.25 +lon_0=-117 +k=1.0001 +x_0=80000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Ontario_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102515, "+proj=tmerc +lat_0=45.0833333333333 +lon_0=-118.333333333333 +k=1.000175 +x_0=30000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Pendleton-La_Grande_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102516, "+proj=tmerc +lat_0=45.25 +lon_0=-119.166666666667 +k=1.000045 +x_0=60000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Pendleton_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102517, "+proj=lcc +lat_1=45.5 +lat_0=45.5 +lon_0=-122.75 +k_0=1.000002 +x_0=100000 +y_0=50000 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Portland_NAD_1983_CORS96_LCC_Feet_Intl (deprecated)
    (102518, "+proj=tmerc +lat_0=44.3333333333333 +lon_0=-123.083333333333 +k=1.00001 +x_0=50000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Salem_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102519, "+proj=tmerc +lat_0=44.0833333333333 +lon_0=-122.5 +k=1.000155 +x_0=0 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // OCRS_Santiam_Pass_NAD_1983_CORS96_TM_Feet_Intl (deprecated)
    (102520, "+proj=tmerc +lat_0=18.8333333333333 +lon_0=-155.5 +k=0.999966666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_1_FIPS_5101 (deprecated)
    (102521, "+proj=tmerc +lat_0=20.3333333333333 +lon_0=-156.666666666667 +k=0.999966666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_2_FIPS_5102 (deprecated)
    (102522, "+proj=tmerc +lat_0=21.1666666666667 +lon_0=-158 +k=0.99999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_3_FIPS_5103 (deprecated)
    (102523, "+proj=tmerc +lat_0=21.8333333333333 +lon_0=-159.5 +k=0.99999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_4_FIPS_5104 (deprecated)
    (102524, "+proj=tmerc +lat_0=21.6666666666667 +lon_0=-160.166666666667 +k=1 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_5_FIPS_5105 (deprecated)
    (102525, "+proj=tmerc +lat_0=18.8333333333333 +lon_0=-155.5 +k=0.999966666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_1_FIPS_5101_Feet
    (102526, "+proj=tmerc +lat_0=20.3333333333333 +lon_0=-156.666666666667 +k=0.999966666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_2_FIPS_5102_Feet
    (102527, "+proj=tmerc +lat_0=21.1666666666667 +lon_0=-158 +k=0.99999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_3_FIPS_5103_Feet (deprecated)
    (102528, "+proj=tmerc +lat_0=21.8333333333333 +lon_0=-159.5 +k=0.99999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_4_FIPS_5104_Feet
    (102529, "+proj=tmerc +lat_0=21.6666666666667 +lon_0=-160.166666666667 +k=1 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_PA11_StatePlane_Hawaii_5_FIPS_5105_Feet
    (102530, "+proj=tmerc +lat_0=44.5 +lon_0=-117.833333333333 +k=1.00016 +x_0=40000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Baker_NAD_1983_CORS96_TM_Meters (deprecated)
    (102531, "+proj=lcc +lat_1=43.6666666666667 +lat_0=43.6666666666667 +lon_0=-119.75 +k_0=1.0002 +x_0=120000 +y_0=60000 +ellps=GRS80 +units=m +no_defs"), // OCRS_Bend-Burns_NAD_1983_CORS96_LCC_Meters (deprecated)
    (102532, "+proj=tmerc +lat_0=41.75 +lon_0=-121.75 +k=1.0002 +x_0=80000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Bend-Klamath_Falls_NAD_1983_CORS96_TM_Meters (deprecated)
    (102533, "+proj=lcc +lat_1=44.6666666666667 +lat_0=44.6666666666667 +lon_0=-121.25 +k_0=1.00012 +x_0=80000 +y_0=130000 +ellps=GRS80 +units=m +no_defs"), // OCRS_Bend-Redmond-Prineville_NAD_1983_CORS96_LCC_Meters (deprecated)
    (102534, "+proj=tmerc +lat_0=42.5 +lon_0=-123.333333333333 +k=1.00007 +x_0=40000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Canyonville-Grants_Pass_NAD_1983_CORS96_TM_Meters (deprecated)
    (102535, "+proj=lcc +lat_1=45.6666666666667 +lat_0=45.6666666666667 +lon_0=-120.5 +k_0=1.000008 +x_0=150000 +y_0=30000 +ellps=GRS80 +units=m +no_defs"), // OCRS_Columbia_River_East_NAD_1983_CORS96_LCC_Meters (deprecated)
    (102537, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-123.333333333333 +k=1.000023 +x_0=50000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Cottage_Grove-Canyonville_NAD_1983_CORS96_TM_Meters (deprecated)
    (102538, "+proj=tmerc +lat_0=44.5 +lon_0=-121 +k=1.00011 +x_0=80000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Dufur-Madras_NAD_1983_CORS96_TM_Meters (deprecated)
    (102539, "+proj=tmerc +lat_0=43.75 +lon_0=-123.166666666667 +k=1.000015 +x_0=50000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Eugene_NAD_1983_CORS96_TM_Meters (deprecated)
    (102540, "+proj=tmerc +lat_0=41.75 +lon_0=-123.333333333333 +k=1.000043 +x_0=50000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Grants_Pass-Ashland_NAD_1983_CORS96_TM_Meters (deprecated)
    (102541, "+proj=tmerc +lat_0=45 +lon_0=-122.333333333333 +k=1.00005 +x_0=10000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Gresham-Warm_Springs_NAD_1983_CORS96_TM_Meters (deprecated)
    (102542, "+proj=tmerc +lat_0=45 +lon_0=-118 +k=1.00013 +x_0=40000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_La_Grande_NAD_1983_CORS96_TM_Meters (deprecated)
    (102543, "+proj=tmerc +lat_0=43.25 +lon_0=-117 +k=1.0001 +x_0=80000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Ontario_NAD_1983_CORS96_TM_Meters (deprecated)
    (102545, "+proj=tmerc +lat_0=45.0833333333333 +lon_0=-118.333333333333 +k=1.000175 +x_0=30000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Pendleton-La_Grande_NAD_1983_CORS96_TM_Meters (deprecated)
    (102546, "+proj=tmerc +lat_0=45.25 +lon_0=-119.166666666667 +k=1.000045 +x_0=60000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Pendleton_NAD_1983_CORS96_TM_Meters (deprecated)
    (102547, "+proj=lcc +lat_1=45.5 +lat_0=45.5 +lon_0=-122.75 +k_0=1.000002 +x_0=100000 +y_0=50000 +ellps=GRS80 +units=m +no_defs"), // OCRS_Portland_NAD_1983_CORS96_LCC_Meters (deprecated)
    (102548, "+proj=tmerc +lat_0=44.3333333333333 +lon_0=-123.083333333333 +k=1.00001 +x_0=50000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Salem_NAD_1983_CORS96_TM_Meters (deprecated)
    (102549, "+proj=tmerc +lat_0=44.0833333333333 +lon_0=-122.5 +k=1.000155 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // OCRS_Santiam_Pass_NAD_1983_CORS96_TM_Meters (deprecated)
    (102550, "+proj=tmerc +lat_0=0 +lon_0=27 +k=0.9996 +x_0=9500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_9
    (102551, "+proj=tmerc +lat_0=0 +lon_0=30 +k=0.9996 +x_0=10500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_10
    (102552, "+proj=tmerc +lat_0=0 +lon_0=33 +k=0.9996 +x_0=11500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_11
    (102553, "+proj=tmerc +lat_0=0 +lon_0=36 +k=0.9996 +x_0=12500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_12
    (102554, "+proj=tmerc +lat_0=0 +lon_0=39 +k=0.9996 +x_0=13500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_13
    (102555, "+proj=tmerc +lat_0=0 +lon_0=42 +k=0.9996 +x_0=14500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_14
    (102556, "+proj=tmerc +lat_0=0 +lon_0=45 +k=0.9996 +x_0=15500000 +y_0=0 +ellps=intl +towgs84=-87,-98,-121,0,0,0,0 +units=m +no_defs"), // ED_1950_Turkey_15
    (102557, "+proj=tmerc +lat_0=0 +lon_0=68.5166666666667 +k=1 +x_0=1300000 +y_0=14743.5 +ellps=GRS80 +units=m +no_defs"), // Kyrg-06_TM_Zone_1 (deprecated)
    (102558, "+proj=tmerc +lat_0=0 +lon_0=71.5166666666667 +k=1 +x_0=2300000 +y_0=14743.5 +ellps=GRS80 +units=m +no_defs"), // Kyrg-06_TM_Zone_2 (deprecated)
    (102559, "+proj=tmerc +lat_0=0 +lon_0=74.5166666666667 +k=1 +x_0=3300000 +y_0=14743.5 +ellps=GRS80 +units=m +no_defs"), // Kyrg-06_TM_Zone_3 (deprecated)
    (102560, "+proj=tmerc +lat_0=0 +lon_0=77.5166666666667 +k=1 +x_0=4300000 +y_0=14743.5 +ellps=GRS80 +units=m +no_defs"), // Kyrg-06_TM_Zone_4 (deprecated)
    (102561, "+proj=tmerc +lat_0=0 +lon_0=80.5166666666667 +k=1 +x_0=5300000 +y_0=14743.5 +ellps=GRS80 +units=m +no_defs"), // Kyrg-06_TM_Zone_5 (deprecated)
    (102562, "+proj=tmerc +lat_0=0 +lon_0=19 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo19_(E-N)
    (102563, "+proj=tmerc +lat_0=0 +lon_0=21 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo21_(E-N)
    (102564, "+proj=tmerc +lat_0=0 +lon_0=23 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo23_(E-N)
    (102565, "+proj=tmerc +lat_0=0 +lon_0=25 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo25_(E-N)
    (102566, "+proj=tmerc +lat_0=0 +lon_0=27 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo27_(E-N)
    (102567, "+proj=tmerc +lat_0=0 +lon_0=29 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo29_(E-N)
    (102568, "+proj=tmerc +lat_0=0 +lon_0=31 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo31_(E-N)
    (102569, "+proj=tmerc +lat_0=0 +lon_0=132 +k=1 +x_0=500000 +y_0=0 +ellps=krass +units=m +no_defs"), // New_Beijing_3_Degree_Gauss_Kruger_CM_132E
    (102570, "+proj=utm +zone=20 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_20N
    (102571, "+proj=utm +zone=21 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_21N
    (102572, "+proj=utm +zone=22 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_22N
    (102573, "+proj=utm +zone=23 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_23N
    (102574, "+proj=utm +zone=24 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_24N
    (102575, "+proj=utm +zone=25 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_25N
    (102576, "+proj=utm +zone=26 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_26N
    (102577, "+proj=utm +zone=27 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_27N
    (102578, "+proj=utm +zone=28 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_28N
    (102579, "+proj=utm +zone=29 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_29N
    (102580, "+proj=utm +zone=30 +datum=WGS84 +units=m +no_defs"), // WGS_1984_Complex_UTM_Zone_30N
    (102581, "+proj=lcc +lat_1=49.5 +lat_0=49.5 +lon_0=2.33722916666667 +k_0=0.999877341 +x_0=600000 +y_0=1200000 +ellps=clrk80ign +towgs84=-168,-60,320,0,0,0,0 +units=m +no_defs"), // NTF_France_I_degrees
    (102582, "+proj=lcc +lat_1=46.8 +lat_0=46.8 +lon_0=2.33722916666667 +k_0=0.99987742 +x_0=600000 +y_0=2200000 +ellps=clrk80ign +towgs84=-168,-60,320,0,0,0,0 +units=m +no_defs"), // NTF_France_II_degrees
    (102583, "+proj=lcc +lat_1=44.1 +lat_0=44.1 +lon_0=2.33722916666667 +k_0=0.999877499 +x_0=600000 +y_0=3200000 +ellps=clrk80ign +towgs84=-168,-60,320,0,0,0,0 +units=m +no_defs"), // NTF_France_III_degrees
    (102584, "+proj=lcc +lat_1=42.165 +lat_0=42.165 +lon_0=2.33722916666667 +k_0=0.99994471 +x_0=234.358 +y_0=4185861.369 +ellps=clrk80ign +towgs84=-168,-60,320,0,0,0,0 +units=m +no_defs"), // NTF_France_IV_degrees
    (102585, "+proj=lcc +lat_1=49.5 +lat_0=49.5 +lon_0=2.33722916666667 +k_0=0.999877341 +x_0=600000 +y_0=1200000 +ellps=clrk80ign +towgs84=-168,-60,320,0,0,0,0 +units=m +no_defs"), // NTF_Lambert_Zone_I
    (102586, "+proj=lcc +lat_1=46.8 +lat_0=46.8 +lon_0=2.33722916666667 +k_0=0.99987742 +x_0=600000 +y_0=2200000 +ellps=clrk80ign +towgs84=-168,-60,320,0,0,0,0 +units=m +no_defs"), // NTF_Lambert_Zone_II
    (102587, "+proj=lcc +lat_1=44.1 +lat_0=44.1 +lon_0=2.33722916666667 +k_0=0.999877499 +x_0=600000 +y_0=3200000 +ellps=clrk80ign +towgs84=-168,-60,320,0,0,0,0 +units=m +no_defs"), // NTF_Lambert_Zone_III
    (102588, "+proj=lcc +lat_1=42.165 +lat_0=42.165 +lon_0=2.33722916666667 +k_0=0.99994471 +x_0=234.358 +y_0=4185861.369 +ellps=clrk80ign +towgs84=-168,-60,320,0,0,0,0 +units=m +no_defs"), // NTF_Lambert_Zone_IV
    (102589, "+proj=aea +lat_0=25 +lon_0=-86 +lat_1=31 +lat_2=41 +x_0=0 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // Panhandle_Energy_Albers
    (102591, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=2.7 +k_0=0.999625544 +x_0=500135 +y_0=300090 +a=6378249.145 +rf=293.465 +units=m +no_defs"), // Nord_Algerie_Degree (deprecated)
    (102592, "+proj=lcc +lat_1=33.3 +lat_0=33.3 +lon_0=2.7 +k_0=0.999625769 +x_0=500135 +y_0=300090 +a=6378249.145 +rf=293.465 +units=m +no_defs"), // Sud_Algerie_Degree (deprecated)
    (102593, "+proj=utm +zone=51 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_UTM_Zone_51N (deprecated)
    (102594, "+proj=utm +zone=52 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_UTM_Zone_52N (deprecated)
    (102595, "+proj=utm +zone=53 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_UTM_Zone_53N (deprecated)
    (102596, "+proj=utm +zone=54 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_UTM_Zone_54N (deprecated)
    (102597, "+proj=utm +zone=55 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_UTM_Zone_55N (deprecated)
    (102598, "+proj=utm +zone=56 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_UTM_Zone_56N
    (102599, "+proj=aea +lat_0=0 +lon_0=-120 +lat_1=34 +lat_2=40.5 +x_0=0 +y_0=-1219202.43840488 +datum=WGS84 +units=us-ft +no_defs"), // WGS_1984_California_Teale_Albers_FtUS
    (102600, "+proj=aea +lat_0=0 +lon_0=-120 +lat_1=34 +lat_2=40.5 +x_0=0 +y_0=-1219202.43840488 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_California_Teale_Albers_FtUS
    (102601, "+proj=aea +lat_0=18 +lon_0=-100 +lat_1=27.5 +lat_2=35 +x_0=1500000 +y_0=6000000 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Texas_Centric_Mapping_System_Albers (deprecated)
    (102602, "+proj=lcc +lat_0=18 +lon_0=-100 +lat_1=27.5 +lat_2=35 +x_0=1500000 +y_0=5000000 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Texas_Centric_Mapping_System_Lambert (deprecated)
    (102603, "+proj=lcc +lat_0=31.1666666666667 +lon_0=-100 +lat_1=27.4166666666667 +lat_2=34.9166666666667 +x_0=1000000 +y_0=1000000 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Texas_Statewide_Mapping_System (deprecated)
    (102604, "+proj=lcc +lat_0=0 +lon_0=-83.5 +lat_1=31.4166666666667 +lat_2=34.2833333333333 +x_0=0 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_Georgia_Statewide_Lambert
    (102605, "+proj=tmerc +lat_0=42 +lon_0=-114 +k=0.9996 +x_0=2500000 +y_0=1200000 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Idaho_TM (deprecated)
    (102606, "+proj=tmerc +lat_0=43.8333333333333 +lon_0=-67.875 +k=0.99998 +x_0=700000 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Maine_2000_East_Zone (deprecated)
    (102607, "+proj=tmerc +lat_0=43.5 +lon_0=-69.125 +k=0.99998 +x_0=500000 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Maine_2000_Central_Zone (deprecated)
    (102608, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.375 +k=0.99998 +x_0=300000 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Maine_2000_West_Zone (deprecated)
    (102609, "+proj=tmerc +lat_0=32.5 +lon_0=-89.75 +k=0.9998335 +x_0=500000 +y_0=1300000 +datum=NAD83 +units=m +no_defs"), // NAD_1983_Mississippi_TM (deprecated)
    (102610, "+proj=tmerc +lat_0=33 +lon_0=129.5 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_1 (deprecated)
    (102611, "+proj=tmerc +lat_0=33 +lon_0=131 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_2 (deprecated)
    (102612, "+proj=tmerc +lat_0=36 +lon_0=132.166666666667 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_3 (deprecated)
    (102613, "+proj=tmerc +lat_0=33 +lon_0=133.5 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_4 (deprecated)
    (102614, "+proj=tmerc +lat_0=36 +lon_0=134.333333333333 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_5 (deprecated)
    (102615, "+proj=tmerc +lat_0=36 +lon_0=136 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_6 (deprecated)
    (102616, "+proj=tmerc +lat_0=36 +lon_0=137.166666666667 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_7 (deprecated)
    (102617, "+proj=tmerc +lat_0=36 +lon_0=138.5 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_8 (deprecated)
    (102618, "+proj=tmerc +lat_0=36 +lon_0=139.833333333333 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_9 (deprecated)
    (102619, "+proj=tmerc +lat_0=40 +lon_0=140.833333333333 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_10 (deprecated)
    (102620, "+proj=tmerc +lat_0=44 +lon_0=140.25 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_11 (deprecated)
    (102621, "+proj=tmerc +lat_0=44 +lon_0=142.25 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_12 (deprecated)
    (102622, "+proj=tmerc +lat_0=44 +lon_0=144.25 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_13 (deprecated)
    (102623, "+proj=tmerc +lat_0=26 +lon_0=142 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_14 (deprecated)
    (102624, "+proj=tmerc +lat_0=26 +lon_0=127.5 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_15 (deprecated)
    (102625, "+proj=tmerc +lat_0=26 +lon_0=124 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_16 (deprecated)
    (102626, "+proj=tmerc +lat_0=26 +lon_0=131 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_17 (deprecated)
    (102627, "+proj=tmerc +lat_0=20 +lon_0=136 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_18 (deprecated)
    (102628, "+proj=tmerc +lat_0=26 +lon_0=154 +k=0.9999 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // JGD_2011_Japan_Zone_19 (deprecated)
    (102629, "+proj=tmerc +lat_0=30.5 +lon_0=-85.8333333333333 +k=0.99996 +x_0=200000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alabama_East_FIPS_0101_Feet
    (102630, "+proj=tmerc +lat_0=30 +lon_0=-87.5 +k=0.999933333333333 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alabama_West_FIPS_0102_Feet
    (102632, "+proj=tmerc +lat_0=54 +lon_0=-142 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_2_FIPS_5002_Feet
    (102633, "+proj=tmerc +lat_0=54 +lon_0=-146 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_3_FIPS_5003_Feet
    (102634, "+proj=tmerc +lat_0=54 +lon_0=-150 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_4_FIPS_5004_Feet
    (102635, "+proj=tmerc +lat_0=54 +lon_0=-154 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_5_FIPS_5005_Feet
    (102636, "+proj=tmerc +lat_0=54 +lon_0=-158 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_6_FIPS_5006_Feet
    (102637, "+proj=tmerc +lat_0=54 +lon_0=-162 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_7_FIPS_5007_Feet
    (102638, "+proj=tmerc +lat_0=54 +lon_0=-166 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_8_FIPS_5008_Feet
    (102639, "+proj=tmerc +lat_0=54 +lon_0=-170 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_9_FIPS_5009_Feet
    (102640, "+proj=lcc +lat_0=51 +lon_0=-176 +lat_1=51.8333333333333 +lat_2=53.8333333333333 +x_0=1000000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Alaska_10_FIPS_5010_Feet
    (102641, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-122 +lat_1=40 +lat_2=41.6666666666667 +x_0=2000000 +y_0=500000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_California_I_FIPS_0401_Feet (deprecated)
    (102642, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-122 +lat_1=38.3333333333333 +lat_2=39.8333333333333 +x_0=2000000 +y_0=500000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_California_II_FIPS_0402_Feet (deprecated)
    (102643, "+proj=lcc +lat_0=36.5 +lon_0=-120.5 +lat_1=37.0666666666667 +lat_2=38.4333333333333 +x_0=2000000 +y_0=500000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_California_III_FIPS_0403_Feet (deprecated)
    (102644, "+proj=lcc +lat_0=35.3333333333333 +lon_0=-119 +lat_1=36 +lat_2=37.25 +x_0=2000000 +y_0=500000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_California_IV_FIPS_0404_Feet (deprecated)
    (102645, "+proj=lcc +lat_0=33.5 +lon_0=-118 +lat_1=34.0333333333333 +lat_2=35.4666666666667 +x_0=2000000 +y_0=500000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_California_V_FIPS_0405_Feet (deprecated)
    (102646, "+proj=lcc +lat_0=32.1666666666667 +lon_0=-116.25 +lat_1=32.7833333333333 +lat_2=33.8833333333333 +x_0=2000000 +y_0=500000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_California_VI_FIPS_0406_Feet (deprecated)
    (102647, "+proj=lcc +lat_0=17.8333333333333 +lon_0=-66.4333333333333 +lat_1=18.0333333333333 +lat_2=18.4333333333333 +x_0=200000 +y_0=200000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_NSRS2007_StatePlane_Puerto_Rico_Virgin_Isls_FIPS_5200 (deprecated)
    (102648, "+proj=tmerc +lat_0=31 +lon_0=-110.166666666667 +k=0.9999 +x_0=213360 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Arizona_East_FIPS_0201_Feet
    (102649, "+proj=tmerc +lat_0=31 +lon_0=-111.916666666667 +k=0.9999 +x_0=213360 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Arizona_Central_FIPS_0202_Feet
    (102650, "+proj=tmerc +lat_0=31 +lon_0=-113.75 +k=0.999933333333333 +x_0=213360 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Arizona_West_FIPS_0203_Feet
    (102651, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-92 +lat_1=34.9333333333333 +lat_2=36.2333333333333 +x_0=400000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Arkansas_North_FIPS_0301_Feet (deprecated)
    (102652, "+proj=lcc +lat_0=32.6666666666667 +lon_0=-92 +lat_1=33.3 +lat_2=34.7666666666667 +x_0=400000 +y_0=400000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Arkansas_South_FIPS_0302_Feet (deprecated)
    (102653, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-105.5 +lat_1=39.7166666666667 +lat_2=40.7833333333333 +x_0=914401.8289 +y_0=304800.6096 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Colorado_North_FIPS_0501_Feet (deprecated)
    (102654, "+proj=lcc +lat_0=37.8333333333333 +lon_0=-105.5 +lat_1=38.45 +lat_2=39.75 +x_0=914401.8289 +y_0=304800.6096 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Colorado_Central_FIPS_0502_Feet (deprecated)
    (102655, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-105.5 +lat_1=37.2333333333333 +lat_2=38.4333333333333 +x_0=914401.8289 +y_0=304800.6096 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Colorado_South_FIPS_0503_Feet (deprecated)
    (102656, "+proj=lcc +lat_0=40.8333333333333 +lon_0=-72.75 +lat_1=41.2 +lat_2=41.8666666666667 +x_0=304800.6096 +y_0=152400.3048 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Connecticut_FIPS_0600_Feet (deprecated)
    (102657, "+proj=tmerc +lat_0=38 +lon_0=-75.4166666666667 +k=0.999995 +x_0=200000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Delaware_FIPS_0700_Feet (deprecated)
    (102658, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-81 +k=0.999941176470588 +x_0=200000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Florida_East_FIPS_0901_Feet (deprecated)
    (102659, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-82 +k=0.999941176470588 +x_0=200000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Florida_West_FIPS_0902_Feet (deprecated)
    (102660, "+proj=lcc +lat_0=29 +lon_0=-84.5 +lat_1=29.5833333333333 +lat_2=30.75 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Florida_North_FIPS_0903_Feet (deprecated)
    (102661, "+proj=tmerc +lat_0=18.8333333333333 +lon_0=-155.5 +k=0.999966666666667 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Hawaii_1_FIPS_5101_Feet
    (102662, "+proj=tmerc +lat_0=20.3333333333333 +lon_0=-156.666666666667 +k=0.999966666666667 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Hawaii_2_FIPS_5102_Feet
    (102663, "+proj=tmerc +lat_0=21.1666666666667 +lon_0=-158 +k=0.99999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Hawaii_3_FIPS_5103_Feet (deprecated)
    (102664, "+proj=tmerc +lat_0=21.8333333333333 +lon_0=-159.5 +k=0.99999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Hawaii_4_FIPS_5104_Feet
    (102665, "+proj=tmerc +lat_0=21.6666666666667 +lon_0=-160.166666666667 +k=1 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Hawaii_5_FIPS_5105_Feet
    (102666, "+proj=tmerc +lat_0=30 +lon_0=-82.1666666666667 +k=0.9999 +x_0=200000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Georgia_East_FIPS_1001_Feet (deprecated)
    (102667, "+proj=tmerc +lat_0=30 +lon_0=-84.1666666666667 +k=0.9999 +x_0=700000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Georgia_West_FIPS_1002_Feet (deprecated)
    (102668, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-112.166666666667 +k=0.999947368421053 +x_0=200000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Idaho_East_FIPS_1101_Feet (deprecated)
    (102669, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-114 +k=0.999947368421053 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Idaho_Central_FIPS_1102_Feet (deprecated)
    (102670, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-115.75 +k=0.999933333333333 +x_0=800000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Idaho_West_FIPS_1103_Feet (deprecated)
    (102671, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-88.3333333333333 +k=0.999975 +x_0=300000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Illinois_East_FIPS_1201_Feet (deprecated)
    (102672, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-90.1666666666667 +k=0.999941176470588 +x_0=700000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Illinois_West_FIPS_1202_Feet (deprecated)
    (102673, "+proj=tmerc +lat_0=37.5 +lon_0=-85.6666666666667 +k=0.999966666666667 +x_0=100000 +y_0=250000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Indiana_East_FIPS_1301_Feet (deprecated)
    (102674, "+proj=tmerc +lat_0=37.5 +lon_0=-87.0833333333333 +k=0.999966666666667 +x_0=900000 +y_0=250000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Indiana_West_FIPS_1302_Feet (deprecated)
    (102675, "+proj=lcc +lat_0=41.5 +lon_0=-93.5 +lat_1=42.0666666666667 +lat_2=43.2666666666667 +x_0=1500000 +y_0=1000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Iowa_North_FIPS_1401_Feet (deprecated)
    (102676, "+proj=lcc +lat_0=40 +lon_0=-93.5 +lat_1=40.6166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Iowa_South_FIPS_1402_Feet (deprecated)
    (102677, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-98 +lat_1=38.7166666666667 +lat_2=39.7833333333333 +x_0=400000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Kansas_North_FIPS_1501_Feet (deprecated)
    (102678, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-98.5 +lat_1=37.2666666666667 +lat_2=38.5666666666667 +x_0=400000 +y_0=400000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Kansas_South_FIPS_1502_Feet (deprecated)
    (102679, "+proj=lcc +lat_0=37.5 +lon_0=-84.25 +lat_1=37.9666666666667 +lat_2=38.9666666666667 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Kentucky_North_FIPS_1601_Feet (deprecated)
    (102680, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=36.7333333333333 +lat_2=37.9333333333333 +x_0=500000 +y_0=500000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Kentucky_South_FIPS_1602_Feet (deprecated)
    (102681, "+proj=lcc +lat_0=30.5 +lon_0=-92.5 +lat_1=31.1666666666667 +lat_2=32.6666666666667 +x_0=1000000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Louisiana_North_FIPS_1701_Feet (deprecated)
    (102682, "+proj=lcc +lat_0=28.5 +lon_0=-91.3333333333333 +lat_1=29.3 +lat_2=30.7 +x_0=1000000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Louisiana_South_FIPS_1702_Feet (deprecated)
    (102683, "+proj=tmerc +lat_0=43.6666666666667 +lon_0=-68.5 +k=0.9999 +x_0=300000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Maine_East_FIPS_1801_Feet (deprecated)
    (102684, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.1666666666667 +k=0.999966666666667 +x_0=900000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Maine_West_FIPS_1802_Feet (deprecated)
    (102685, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-77 +lat_1=38.3 +lat_2=39.45 +x_0=400000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Maryland_FIPS_1900_Feet (deprecated)
    (102686, "+proj=lcc +lat_0=41 +lon_0=-71.5 +lat_1=41.7166666666667 +lat_2=42.6833333333333 +x_0=200000 +y_0=750000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Massachusetts_Mainland_FIPS_2001_Feet (deprecated)
    (102687, "+proj=lcc +lat_0=41 +lon_0=-70.5 +lat_1=41.2833333333333 +lat_2=41.4833333333333 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Massachusetts_Island_FIPS_2002_Feet (deprecated)
    (102688, "+proj=lcc +lat_0=44.7833333333333 +lon_0=-87 +lat_1=45.4833333333333 +lat_2=47.0833333333333 +x_0=8000000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Michigan_North_FIPS_2111_Feet
    (102689, "+proj=lcc +lat_0=43.3166666666667 +lon_0=-84.3666666666667 +lat_1=44.1833333333333 +lat_2=45.7 +x_0=6000000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Michigan_Central_FIPS_2112_Feet
    (102690, "+proj=lcc +lat_0=41.5 +lon_0=-84.3666666666667 +lat_1=42.1 +lat_2=43.6666666666667 +x_0=4000000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Michigan_South_FIPS_2113_Feet
    (102691, "+proj=lcc +lat_0=46.5 +lon_0=-93.1 +lat_1=47.0333333333333 +lat_2=48.6333333333333 +x_0=800000 +y_0=100000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Minnesota_North_FIPS_2201_Feet (deprecated)
    (102692, "+proj=lcc +lat_0=45 +lon_0=-94.25 +lat_1=45.6166666666667 +lat_2=47.05 +x_0=800000 +y_0=100000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Minnesota_Central_FIPS_2202_Feet (deprecated)
    (102693, "+proj=lcc +lat_0=43 +lon_0=-94 +lat_1=43.7833333333333 +lat_2=45.2166666666667 +x_0=800000 +y_0=100000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Minnesota_South_FIPS_2203_Feet (deprecated)
    (102694, "+proj=tmerc +lat_0=29.5 +lon_0=-88.8333333333333 +k=0.99995 +x_0=300000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Mississippi_East_FIPS_2301_Feet (deprecated)
    (102695, "+proj=tmerc +lat_0=29.5 +lon_0=-90.3333333333333 +k=0.99995 +x_0=700000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Mississippi_West_FIPS_2302_Feet (deprecated)
    (102696, "+proj=tmerc +lat_0=35.8333333333333 +lon_0=-90.5 +k=0.999933333333333 +x_0=250000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Missouri_East_FIPS_2401_Feet
    (102697, "+proj=tmerc +lat_0=35.8333333333333 +lon_0=-92.5 +k=0.999933333333333 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Missouri_Central_FIPS_2402_Feet
    (102698, "+proj=tmerc +lat_0=36.1666666666667 +lon_0=-94.5 +k=0.999941176470588 +x_0=850000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Missouri_West_FIPS_2403_Feet
    (102700, "+proj=lcc +lat_0=44.25 +lon_0=-109.5 +lat_1=45 +lat_2=49 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Montana_FIPS_2500_Feet
    (102701, "+proj=utm +zone=4 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PACP00_UTM_Zone_4N
    (102702, "+proj=utm +zone=5 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PACP00_UTM_Zone_5N
    (102703, "+proj=utm +zone=2 +south +ellps=GRS80 +units=m +no_defs"), // NAD_1983_PACP00_UTM_Zone_2S
    (102704, "+proj=lcc +lat_0=39.8333333333333 +lon_0=-100 +lat_1=40 +lat_2=43 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Nebraska_FIPS_2600_Feet (deprecated)
    (102705, "+proj=tmerc +lat_0=40.25 +lon_0=-96.6880555555556 +k=1.000054615 +x_0=50000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_Nebraska_Lancaster_County_FtUS
    (102707, "+proj=tmerc +lat_0=34.75 +lon_0=-115.583333333333 +k=0.9999 +x_0=200000 +y_0=8000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Nevada_East_FIPS_2701_Feet (deprecated)
    (102708, "+proj=tmerc +lat_0=34.75 +lon_0=-116.666666666667 +k=0.9999 +x_0=500000 +y_0=6000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Nevada_Central_FIPS_2702_Feet (deprecated)
    (102709, "+proj=tmerc +lat_0=34.75 +lon_0=-118.583333333333 +k=0.9999 +x_0=800000 +y_0=4000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Nevada_West_FIPS_2703_Feet (deprecated)
    (102710, "+proj=tmerc +lat_0=42.5 +lon_0=-71.6666666666667 +k=0.999966666666667 +x_0=300000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_Hampshire_FIPS_2800_Feet (deprecated)
    (102711, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_Jersey_FIPS_2900_Feet (deprecated)
    (102712, "+proj=tmerc +lat_0=31 +lon_0=-104.333333333333 +k=0.999909090909091 +x_0=165000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_Mexico_East_FIPS_3001_Feet (deprecated)
    (102713, "+proj=tmerc +lat_0=31 +lon_0=-106.25 +k=0.9999 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_Mexico_Central_FIPS_3002_Feet (deprecated)
    (102714, "+proj=tmerc +lat_0=31 +lon_0=-107.833333333333 +k=0.999916666666667 +x_0=830000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_Mexico_West_FIPS_3003_Feet (deprecated)
    (102715, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_York_East_FIPS_3101_Feet (deprecated)
    (102716, "+proj=tmerc +lat_0=40 +lon_0=-76.5833333333333 +k=0.9999375 +x_0=250000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_York_Central_FIPS_3102_Feet (deprecated)
    (102717, "+proj=tmerc +lat_0=40 +lon_0=-78.5833333333333 +k=0.9999375 +x_0=350000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_York_West_FIPS_3103_Feet (deprecated)
    (102718, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-74 +lat_1=40.6666666666667 +lat_2=41.0333333333333 +x_0=300000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_New_York_Long_Island_FIPS_3104_Feet (deprecated)
    (102719, "+proj=lcc +lat_0=33.75 +lon_0=-79 +lat_1=34.3333333333333 +lat_2=36.1666666666667 +x_0=609601.22 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_North_Carolina_FIPS_3200_Feet (deprecated)
    (102720, "+proj=lcc +lat_0=47 +lon_0=-100.5 +lat_1=47.4333333333333 +lat_2=48.7333333333333 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_North_Dakota_North_FIPS_3301_Feet
    (102721, "+proj=lcc +lat_0=45.6666666666667 +lon_0=-100.5 +lat_1=46.1833333333333 +lat_2=47.4833333333333 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_North_Dakota_South_FIPS_3302_Feet
    (102722, "+proj=lcc +lat_0=39.6666666666667 +lon_0=-82.5 +lat_1=40.4333333333333 +lat_2=41.7 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Ohio_North_FIPS_3401_Feet (deprecated)
    (102723, "+proj=lcc +lat_0=38 +lon_0=-82.5 +lat_1=38.7333333333333 +lat_2=40.0333333333333 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Ohio_South_FIPS_3402_Feet (deprecated)
    (102724, "+proj=lcc +lat_0=35 +lon_0=-98 +lat_1=35.5666666666667 +lat_2=36.7666666666667 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Oklahoma_North_FIPS_3501_Feet (deprecated)
    (102725, "+proj=lcc +lat_0=33.3333333333333 +lon_0=-98 +lat_1=33.9333333333333 +lat_2=35.2333333333333 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Oklahoma_South_FIPS_3502_Feet (deprecated)
    (102726, "+proj=lcc +lat_0=43.6666666666667 +lon_0=-120.5 +lat_1=44.3333333333333 +lat_2=46 +x_0=2500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Oregon_North_FIPS_3601_Feet
    (102727, "+proj=lcc +lat_0=41.6666666666667 +lon_0=-120.5 +lat_1=42.3333333333333 +lat_2=44 +x_0=1500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Oregon_South_FIPS_3602_Feet
    (102728, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-77.75 +lat_1=40.8833333333333 +lat_2=41.95 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Pennsylvania_North_FIPS_3701_Feet (deprecated)
    (102729, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-77.75 +lat_1=39.9333333333333 +lat_2=40.9666666666667 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Pennsylvania_South_FIPS_3702_Feet (deprecated)
    (102730, "+proj=tmerc +lat_0=41.0833333333333 +lon_0=-71.5 +k=0.99999375 +x_0=100000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Rhode_Island_FIPS_3800_Feet (deprecated)
    (102731, "+proj=utm +zone=17 +ellps=GRS80 +units=m +no_defs"), // PANAMA_ITRF08_UTM_17N
    (102732, "+proj=utm +zone=18 +ellps=GRS80 +units=m +no_defs"), // PANAMA_ITRF08_UTM_18N
    (102733, "+proj=lcc +lat_0=31.8333333333333 +lon_0=-81 +lat_1=32.5 +lat_2=34.8333333333333 +x_0=609600 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_South_Carolina_FIPS_3900_Feet
    (102734, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-100 +lat_1=44.4166666666667 +lat_2=45.6833333333333 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_South_Dakota_North_FIPS_4001_Feet (deprecated)
    (102735, "+proj=lcc +lat_0=42.3333333333333 +lon_0=-100.333333333333 +lat_1=42.8333333333333 +lat_2=44.4 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_South_Dakota_South_FIPS_4002_Feet (deprecated)
    (102736, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-86 +lat_1=35.25 +lat_2=36.4166666666667 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Tennessee_FIPS_4100_Feet (deprecated)
    (102737, "+proj=lcc +lat_0=34 +lon_0=-101.5 +lat_1=34.65 +lat_2=36.1833333333333 +x_0=200000 +y_0=1000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Texas_North_FIPS_4201_Feet (deprecated)
    (102738, "+proj=lcc +lat_0=31.6666666666667 +lon_0=-98.5 +lat_1=32.1333333333333 +lat_2=33.9666666666667 +x_0=600000 +y_0=2000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Texas_North_Central_FIPS_4202_Feet (deprecated)
    (102739, "+proj=lcc +lat_0=29.6666666666667 +lon_0=-100.333333333333 +lat_1=30.1166666666667 +lat_2=31.8833333333333 +x_0=700000 +y_0=3000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Texas_Central_FIPS_4203_Feet (deprecated)
    (102740, "+proj=lcc +lat_0=27.8333333333333 +lon_0=-99 +lat_1=28.3833333333333 +lat_2=30.2833333333333 +x_0=600000 +y_0=4000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Texas_South_Central_FIPS_4204_Feet (deprecated)
    (102741, "+proj=lcc +lat_0=25.6666666666667 +lon_0=-98.5 +lat_1=26.1666666666667 +lat_2=27.8333333333333 +x_0=300000 +y_0=5000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Texas_South_FIPS_4205_Feet (deprecated)
    (102742, "+proj=lcc +lat_0=40.3333333333333 +lon_0=-111.5 +lat_1=40.7166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=1000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Utah_North_FIPS_4301_Feet (deprecated)
    (102743, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-111.5 +lat_1=39.0166666666667 +lat_2=40.65 +x_0=500000 +y_0=2000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Utah_Central_FIPS_4302_Feet (deprecated)
    (102744, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-111.5 +lat_1=37.2166666666667 +lat_2=38.35 +x_0=500000 +y_0=3000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Utah_South_FIPS_4303_Feet (deprecated)
    (102745, "+proj=tmerc +lat_0=42.5 +lon_0=-72.5 +k=0.999964285714286 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Vermont_FIPS_4400_Feet (deprecated)
    (102746, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-78.5 +lat_1=38.0333333333333 +lat_2=39.2 +x_0=3500000 +y_0=2000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Virginia_North_FIPS_4501_Feet (deprecated)
    (102747, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-78.5 +lat_1=36.7666666666667 +lat_2=37.9666666666667 +x_0=3500000 +y_0=1000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Virginia_South_FIPS_4502_Feet (deprecated)
    (102748, "+proj=lcc +lat_0=47 +lon_0=-120.833333333333 +lat_1=47.5 +lat_2=48.7333333333333 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Washington_North_FIPS_4601_Feet (deprecated)
    (102749, "+proj=lcc +lat_0=45.3333333333333 +lon_0=-120.5 +lat_1=45.8333333333333 +lat_2=47.3333333333333 +x_0=500000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Washington_South_FIPS_4602_Feet (deprecated)
    (102750, "+proj=lcc +lat_0=38.5 +lon_0=-79.5 +lat_1=39 +lat_2=40.25 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_West_Virginia_North_FIPS_4701_Feet (deprecated)
    (102751, "+proj=lcc +lat_0=37 +lon_0=-81 +lat_1=37.4833333333333 +lat_2=38.8833333333333 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_West_Virginia_South_FIPS_4702_Feet (deprecated)
    (102752, "+proj=lcc +lat_0=45.1666666666667 +lon_0=-90 +lat_1=45.5666666666667 +lat_2=46.7666666666667 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Wisconsin_North_FIPS_4801_Feet (deprecated)
    (102753, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-90 +lat_1=44.25 +lat_2=45.5 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Wisconsin_Central_FIPS_4802_Feet (deprecated)
    (102754, "+proj=lcc +lat_0=42 +lon_0=-90 +lat_1=42.7333333333333 +lat_2=44.0666666666667 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Wisconsin_South_FIPS_4803_Feet (deprecated)
    (102755, "+proj=tmerc +lat_0=40.5 +lon_0=-105.166666666667 +k=0.9999375 +x_0=200000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Wyoming_East_FIPS_4901_Feet (deprecated)
    (102756, "+proj=tmerc +lat_0=40.5 +lon_0=-107.333333333333 +k=0.9999375 +x_0=400000 +y_0=100000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Wyoming_East_Central_FIPS_4902_Feet (deprecated)
    (102757, "+proj=tmerc +lat_0=40.5 +lon_0=-108.75 +k=0.9999375 +x_0=600000 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Wyoming_West_Central_FIPS_4903_Feet (deprecated)
    (102758, "+proj=tmerc +lat_0=40.5 +lon_0=-110.083333333333 +k=0.9999375 +x_0=800000 +y_0=100000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Wyoming_West_FIPS_4904_Feet (deprecated)
    (102759, "+proj=aea +lat_0=0 +lon_0=-120 +lat_1=34 +lat_2=40.5 +x_0=0 +y_0=-1219200 +datum=WGS84 +units=ft +no_defs"), // WGS_1984_California_Teale_Albers_Ft_Intl
    (102760, "+proj=aea +lat_0=0 +lon_0=-120 +lat_1=34 +lat_2=40.5 +x_0=0 +y_0=-1219200 +datum=NAD83 +units=ft +no_defs"), // NAD_1983_California_Teale_Albers_Ft_Intl
    (102761, "+proj=lcc +lat_0=17.8333333333333 +lon_0=-66.4333333333333 +lat_1=18.0333333333333 +lat_2=18.4333333333333 +x_0=200000 +y_0=200000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Puerto_Rico_Virgin_Islands_FIPS_5200_Feet
    (102763, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=37.0833333333333 +lat_2=38.6666666666667 +x_0=1500000 +y_0=1000000 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_StatePlane_Kentucky_FIPS_1600_Feet (deprecated)
    (102764, "+proj=tmerc +lat_0=0 +lon_0=21 +k=1 +x_0=7500000 +y_0=0 +ellps=krass +towgs84=26,-121,-78,0,0,0,0 +units=m +no_defs"), // Pulkovo_1942_Adj_1983_3_Degree_GK_Zone_7 (deprecated)
    (102765, "+proj=tmerc +lat_0=0 +lon_0=24 +k=1 +x_0=8500000 +y_0=0 +ellps=krass +towgs84=26,-121,-78,0,0,0,0 +units=m +no_defs"), // Pulkovo_1942_Adj_1983_3_Degree_GK_Zone_8 (deprecated)
    (102799, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99926 +x_0=261910.5587 +y_0=70975.76209 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A1H1 (deprecated)
    (102800, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999314 +x_0=252927.2844 +y_0=70979.59363 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A2H1 (deprecated)
    (102801, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999365 +x_0=243942.3084 +y_0=70983.21269 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A3H1 (deprecated)
    (102802, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99939 +x_0=243948.4072 +y_0=70984.98734 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A3H2 (deprecated)
    (102803, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999415 +x_0=234956.1813 +y_0=70986.76115 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A4H1 (deprecated)
    (102804, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999465 +x_0=225969.1556 +y_0=70990.30995 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A5H1 (deprecated)
    (102805, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99949 +x_0=225974.8051 +y_0=70992.08478 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A5H2 (deprecated)
    (102806, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999516 +x_0=215981.5338 +y_0=70993.93011 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A6H1 (deprecated)
    (102807, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999541 +x_0=215986.9336 +y_0=70995.70502 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A6H2 (deprecated)
    (102808, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999564 +x_0=205992.2754 +y_0=70997.33764 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A7H1 (deprecated)
    (102809, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999589 +x_0=205997.4254 +y_0=70999.11264 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A7H2 (deprecated)
    (102810, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999613 +x_0=196002.254 +y_0=71000.81651 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A8H1 (deprecated)
    (102811, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999638 +x_0=196007.1543 +y_0=71002.5916 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A8H2 (deprecated)
    (102812, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999662 +x_0=185011.1931 +y_0=71004.29572 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A9H1 (deprecated)
    (102813, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999687 +x_0=185015.8185 +y_0=71006.07089 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A9H2 (deprecated)
    (102814, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999713 +x_0=173019.2914 +y_0=71007.91729 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A10H1 (deprecated)
    (102815, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999738 +x_0=173023.6171 +y_0=71009.69256 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A10H2 (deprecated)
    (102816, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999767 +x_0=159026.3186 +y_0=71011.75231 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A11H1 (deprecated)
    (102817, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999792 +x_0=159030.2944 +y_0=71013.52767 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A11H2 (deprecated)
    (102818, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999817 +x_0=159034.2704 +y_0=71015.30312 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A11H3 (deprecated)
    (102819, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999817 +x_0=144031.0383 +y_0=71015.30362 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A12H1 (deprecated)
    (102820, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999842 +x_0=144034.6392 +y_0=71017.07907 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A12H2 (deprecated)
    (102821, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999867 +x_0=144038.2403 +y_0=71018.8546 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A12H3 (deprecated)
    (102822, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999865 +x_0=128033.7365 +y_0=71018.71321 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A13H1 (deprecated)
    (102823, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99989 +x_0=128036.9375 +y_0=71020.48874 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A13H2 (deprecated)
    (102824, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999914 +x_0=111034.6979 +y_0=71022.19417 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A14H1 (deprecated)
    (102825, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999939 +x_0=111037.4739 +y_0=71023.96979 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A14H2 (deprecated)
    (102826, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999967 +x_0=88032.17537 +y_0=71025.95967 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A15H1 (deprecated)
    (102827, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999992 +x_0=88034.37626 +y_0=71027.73539 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A15H2 (deprecated)
    (102828, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000012 +x_0=54022.17583 +y_0=71029.15712 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A16H1 (deprecated)
    (102829, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000037 +x_0=54023.52644 +y_0=71030.93291 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A16H2 (deprecated)
    (102830, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999981 +x_0=-24009.11135 +y_0=71026.9544 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A17H1 (deprecated)
    (102831, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000006 +x_0=-24009.7116 +y_0=71028.73014 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A17H2 (deprecated)
    (102832, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999928 +x_0=-58018.94296 +y_0=71023.18879 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A18H1 (deprecated)
    (102833, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999953 +x_0=-58020.39349 +y_0=71024.96444 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A18H2 (deprecated)
    (102834, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999874 +x_0=-88023.98625 +y_0=71019.35254 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A19H1 (deprecated)
    (102835, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999899 +x_0=-88026.18693 +y_0=71021.12809 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A19H2 (deprecated)
    (102836, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999826 +x_0=-105023.5775 +y_0=71015.94289 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A20H1 (deprecated)
    (102837, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999851 +x_0=-105026.2032 +y_0=71017.71836 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A20H2 (deprecated)
    (102838, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999771 +x_0=-122020.6823 +y_0=71012.0364 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A21H1 (deprecated)
    (102839, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999796 +x_0=-122023.7329 +y_0=71013.81177 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A21H2 (deprecated)
    (102840, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999708 +x_0=-139014.8049 +y_0=71007.56222 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A22H1 (deprecated)
    (102841, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999733 +x_0=-139018.2804 +y_0=71009.33748 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A22H2 (deprecated)
    (102842, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999656 +x_0=-156008.5024 +y_0=71003.86967 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A23H1 (deprecated)
    (102843, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999681 +x_0=-156012.4027 +y_0=71005.64484 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A23H2 (deprecated)
    (102844, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999613 +x_0=-165001.8975 +y_0=71000.81651 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A24H1 (deprecated)
    (102845, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999638 +x_0=-165006.0227 +y_0=71002.5916 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A24H2 (deprecated)
    (102846, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999565 +x_0=-175993.5763 +y_0=70997.40864 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A25H1 (deprecated)
    (102847, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99959 +x_0=-175997.9763 +y_0=70999.18364 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A25H2 (deprecated)
    (102848, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999517 +x_0=-185984.2846 +y_0=70994.00109 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A26H1 (deprecated)
    (102849, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999542 +x_0=-185988.9343 +y_0=70995.77601 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A26H2 (deprecated)
    (102850, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999467 +x_0=-195973.6419 +y_0=70990.45191 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A27H1 (deprecated)
    (102851, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999492 +x_0=-195978.5414 +y_0=70992.22674 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A27H2 (deprecated)
    (102852, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999416 +x_0=-205961.7946 +y_0=70986.83212 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A28H1 (deprecated)
    (102853, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999441 +x_0=-205966.9438 +y_0=70988.60686 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A28H2 (deprecated)
    (102854, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999366 +x_0=-214949.3801 +y_0=70983.28366 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A29H1 (deprecated)
    (102855, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999314 +x_0=-223935.6193 +y_0=70979.59363 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_A30H1 (deprecated)
    (102856, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999967 +x_0=88032.17537 +y_0=111040.5848 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B15H1 (deprecated)
    (102857, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999992 +x_0=88034.37626 +y_0=111043.361 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B15H2 (deprecated)
    (102858, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000017 +x_0=88036.57726 +y_0=111046.1372 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B15H3 (deprecated)
    (102859, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000012 +x_0=54022.17583 +y_0=111045.5837 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B16H1 (deprecated)
    (102860, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000037 +x_0=54023.52644 +y_0=111048.3599 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B16H2 (deprecated)
    (102861, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000062 +x_0=54024.87711 +y_0=111051.1363 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B16H3 (deprecated)
    (102862, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000087 +x_0=54026.22785 +y_0=111053.9128 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B16H4 (deprecated)
    (102863, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999981 +x_0=-24009.11135 +y_0=111042.14 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B17H1 (deprecated)
    (102864, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000006 +x_0=-24009.7116 +y_0=111044.9161 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B17H2 (deprecated)
    (102865, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999928 +x_0=-58018.94296 +y_0=111036.2529 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B18H1 (deprecated)
    (102866, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999953 +x_0=-58020.39349 +y_0=111039.0289 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B18H2 (deprecated)
    (102867, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999874 +x_0=-88023.98625 +y_0=111030.2554 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B19H1 (deprecated)
    (102868, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999826 +x_0=-105023.5775 +y_0=111024.9248 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B20H1 (deprecated)
    (102869, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999771 +x_0=-122020.6823 +y_0=111018.8175 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B21H1 (deprecated)
    (102870, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999709 +x_0=-139014.9439 +y_0=111011.9337 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B22H1 (deprecated)
    (102871, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999656 +x_0=-156008.5024 +y_0=111006.0498 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B23H1 (deprecated)
    (102872, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999614 +x_0=-165002.0625 +y_0=111001.3875 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B24H1 (deprecated)
    (102873, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999565 +x_0=-175993.5763 +y_0=110995.9487 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B25H1 (deprecated)
    (102874, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999517 +x_0=-185984.2846 +y_0=110990.6214 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B26H1 (deprecated)
    (102875, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999467 +x_0=-195973.6419 +y_0=110985.0727 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B27H1 (deprecated)
    (102876, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999416 +x_0=-205961.7946 +y_0=110979.4136 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B28H1 (deprecated)
    (102877, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999367 +x_0=-214949.595 +y_0=110973.9769 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B29H1 (deprecated)
    (102878, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999315 +x_0=-223935.8432 +y_0=110968.208 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B30H1 (deprecated)
    (102879, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999261 +x_0=-232920.6915 +y_0=110962.2179 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B31H1 (deprecated)
    (102880, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999206 +x_0=-241904.3281 +y_0=110956.1174 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_B32H1 (deprecated)
    (102881, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999866 +x_0=128033.8646 +y_0=126033.3354 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C13H1 (deprecated)
    (102882, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999914 +x_0=111034.6979 +y_0=126039.3868 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C14H1 (deprecated)
    (102883, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999939 +x_0=111037.4739 +y_0=126042.5379 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C14H2 (deprecated)
    (102884, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999964 +x_0=111040.25 +y_0=126045.6892 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C14H3 (deprecated)
    (102885, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999989 +x_0=111043.0263 +y_0=126048.8406 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C14H4 (deprecated)
    (102886, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999967 +x_0=88032.17537 +y_0=126046.0693 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C15H1 (deprecated)
    (102887, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999992 +x_0=88034.37626 +y_0=126049.2206 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C15H2 (deprecated)
    (102888, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000017 +x_0=88036.57726 +y_0=126052.372 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C15H3 (deprecated)
    (102889, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000042 +x_0=88038.77836 +y_0=126055.5236 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C15H4 (deprecated)
    (102890, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000067 +x_0=88040.97958 +y_0=126058.6753 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C15H5 (deprecated)
    (102891, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000012 +x_0=54022.17583 +y_0=126051.7436 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C16H1 (deprecated)
    (102892, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000037 +x_0=54023.52644 +y_0=126054.895 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C16H2 (deprecated)
    (102893, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000062 +x_0=54024.87711 +y_0=126058.0466 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C16H3 (deprecated)
    (102894, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000087 +x_0=54026.22785 +y_0=126061.1983 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C16H4 (deprecated)
    (102895, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999981 +x_0=-24009.11135 +y_0=126047.8346 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C17H1 (deprecated)
    (102896, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000006 +x_0=-24009.7116 +y_0=126050.9859 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C17H2 (deprecated)
    (102897, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999928 +x_0=-58018.94296 +y_0=126041.1519 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C18H1 (deprecated)
    (102898, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999953 +x_0=-58020.39349 +y_0=126044.3031 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C18H2 (deprecated)
    (102899, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999874 +x_0=-88023.98625 +y_0=126034.3439 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C19H1 (deprecated)
    (102900, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999899 +x_0=-88026.18693 +y_0=126037.4949 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_Highways_England_C19H2 (deprecated)
    (102901, "+proj=tmerc +lat_0=41.25 +lon_0=-88.5 +k=1.00003 +x_0=1150012.7000254 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Aurora_(US_Feet) (deprecated)
    (102902, "+proj=lcc +lat_1=42.2 +lat_0=42.2 +lon_0=-89.95 +k_0=1.000029 +x_0=549860.2997206 +y_0=230124.460248921 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Freeport_(US_Feet) (deprecated)
    (102903, "+proj=tmerc +lat_0=41.25 +lon_0=-87.8 +k=1.000023 +x_0=1449936.499873 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Chicago_(US_Feet) (deprecated)
    (102904, "+proj=tmerc +lat_0=41.25 +lon_0=-89.25 +k=1.000029 +x_0=860147.320294641 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Rockford_(US_Feet) (deprecated)
    (102905, "+proj=lcc +lat_1=41.55 +lat_0=41.55 +lon_0=-90.6 +k_0=1.000024 +x_0=1749860.2997206 +y_0=230124.460248921 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Moline_(US_Feet) (deprecated)
    (102906, "+proj=lcc +lat_1=41.3 +lat_0=41.3 +lon_0=-89.05 +k_0=1.000023 +x_0=2360071.12014224 +y_0=230124.460248921 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Ottawa_(US_Feet) (deprecated)
    (102907, "+proj=lcc +lat_1=41.55 +lat_0=41.55 +lon_0=-90.05 +k_0=1.00002 +x_0=2050088.9001778 +y_0=230124.460248921 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Sterling_(US_Feet) (deprecated)
    (102908, "+proj=tmerc +lat_0=40.55 +lon_0=-88 +k=1.000022 +x_0=2649936.499873 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Joliet_(US_Feet) (deprecated)
    (102909, "+proj=tmerc +lat_0=37.45 +lon_0=-89.3 +k=1.000025 +x_0=840030.48006096 +y_0=130149.860299721 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Eureka_(US_Feet) (deprecated)
    (102910, "+proj=lcc +lat_1=40.9 +lat_0=40.9 +lon_0=-88.55 +k_0=1.000025 +x_0=1449936.499873 +y_0=530048.26009652 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Pontiac_(US_Feet) (deprecated)
    (102911, "+proj=tmerc +lat_0=40.2 +lon_0=-90.85 +k=1.000024 +x_0=2949860.2997206 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Monmouth_(US_Feet) (deprecated)
    (102912, "+proj=lcc +lat_1=40.75 +lat_0=40.75 +lon_0=-87.95 +k_0=1.000024 +x_0=1749860.2997206 +y_0=530048.26009652 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Watseka_(US_Feet) (deprecated)
    (102913, "+proj=tmerc +lat_0=37.4 +lon_0=-89.65 +k=1.000023 +x_0=420015.24003048 +y_0=189585.979171958 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Peoria_(US_Feet) (deprecated)
    (102914, "+proj=lcc +lat_1=40.5 +lat_0=40.5 +lon_0=-88.85 +k_0=1.000031 +x_0=1150012.7000254 +y_0=530048.26009652 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Bloomington_(US_Feet) (deprecated)
    (102915, "+proj=tmerc +lat_0=37.25 +lon_0=-90.1 +k=1.000023 +x_0=70104.1402082804 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Galesburg_(US_Feet) (deprecated)
    (102916, "+proj=lcc +lat_1=40.15 +lat_0=40.15 +lon_0=-88 +k_0=1.000026 +x_0=99974.5999491999 +y_0=860147.320294641 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Champaign_(US_Feet) (deprecated)
    (102917, "+proj=tmerc +lat_0=36.3 +lon_0=-88.8 +k=1.000024 +x_0=2949860.2997206 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Decatur_(US_Feet) (deprecated)
    (102918, "+proj=tmerc +lat_0=36.75 +lon_0=-91.25 +k=1.000023 +x_0=2050088.9001778 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Quincy_(US_Feet) (deprecated)
    (102919, "+proj=lcc +lat_1=40.15 +lat_0=40.15 +lon_0=-89.8 +k_0=1.000018 +x_0=2670053.34010668 +y_0=530048.26009652 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Lincoln_(US_Feet) (deprecated)
    (102920, "+proj=tmerc +lat_0=36.8 +lon_0=-90.6 +k=1.000024 +x_0=2350012.7000254 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Macomb_(US_Feet) (deprecated)
    (102921, "+proj=lcc +lat_1=39.75 +lat_0=39.75 +lon_0=-89.65 +k_0=1.000022 +x_0=709880.61976124 +y_0=879959.35991872 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Springfield_(US_Feet) (deprecated)
    (102922, "+proj=lcc +lat_1=39.65 +lat_0=39.65 +lon_0=-90.6 +k_0=1.000023 +x_0=380086.36017272 +y_0=860147.320294641 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Jacksonville_(US_Feet) (deprecated)
    (102923, "+proj=tmerc +lat_0=33.15 +lon_0=-89.4 +k=1.000023 +x_0=2050088.9001778 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Taylorville_(US_Feet) (deprecated)
    (102924, "+proj=lcc +lat_1=39.65 +lat_0=39.65 +lon_0=-88 +k_0=1.000024 +x_0=1150012.7000254 +y_0=840030.48006096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Charleston_(US_Feet) (deprecated)
    (102925, "+proj=lcc +lat_1=39.3 +lat_0=39.3 +lon_0=-90.15 +k_0=1.00002 +x_0=1749860.2997206 +y_0=840030.48006096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Carlinville_(US_Feet) (deprecated)
    (102926, "+proj=lcc +lat_1=39.1 +lat_0=39.1 +lon_0=-88 +k_0=1.000017 +x_0=2649936.499873 +y_0=840030.48006096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Robinson_(US_Feet) (deprecated)
    (102927, "+proj=tmerc +lat_0=32.85 +lon_0=-90.5 +k=1.000019 +x_0=1449936.499873 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Jerseyville_(US_Feet) (deprecated)
    (102928, "+proj=lcc +lat_1=38.95 +lat_0=38.95 +lon_0=-89 +k_0=1.000019 +x_0=2350012.7000254 +y_0=840030.48006096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Effingham_(US_Feet) (deprecated)
    (102929, "+proj=tmerc +lat_0=32.35 +lon_0=-90 +k=1.000016 +x_0=2949860.2997206 +y_0=149961.8999238 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Belleville_(US_Feet) (deprecated)
    (102930, "+proj=lcc +lat_1=38.55 +lat_0=38.55 +lon_0=-88.15 +k_0=1.000013 +x_0=380086.36017272 +y_0=1150012.7000254 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Olney_(US_Feet) (deprecated)
    (102931, "+proj=lcc +lat_1=38.45 +lat_0=38.45 +lon_0=-89.15 +k_0=1.000015 +x_0=99974.5999491999 +y_0=1150012.7000254 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Mount_Vernon_(US_Feet) (deprecated)
    (102932, "+proj=lcc +lat_1=37.9 +lat_0=37.9 +lon_0=-88.95 +k_0=1.000012 +x_0=729997.45999492 +y_0=1150012.7000254 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Carbondale_(US_Feet) (deprecated)
    (102933, "+proj=lcc +lat_1=37.2 +lat_0=37.2 +lon_0=-88.9 +k_0=1.00001 +x_0=1110083.82016764 +y_0=1170129.54025908 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_ICS_Metropolis_(US_Feet) (deprecated)
    (102951, "+proj=lcc +lat_0=26 +lon_0=90 +lat_1=23.15 +lat_2=28.8 +x_0=2743183.6991 +y_0=914395.233 +datum=WGS84 +units=m +no_defs"), // LCC_Bangladesh
    (102952, "+proj=lcc +lat_0=26 +lon_0=90 +lat_1=23.15 +lat_2=28.8 +x_0=2743183.6991 +y_0=914395.233 +ellps=evrst30 +towgs84=283.729,735.942,261.143,0,0,0,0 +units=m +no_defs"), // LCC_Gulshan
    (102953, "+proj=tmerc +lat_0=0 +lon_0=90 +k=0.9996 +x_0=500000 +y_0=-2000000 +ellps=evrst30 +towgs84=283.729,735.942,261.143,0,0,0,0 +units=m +no_defs"), // UTM_Bangladesh_(BTM)
    (102954, "+proj=tmerc +lat_0=0 +lon_0=90 +k=0.9996 +x_0=500000 +y_0=0 +datum=WGS84 +units=m +no_defs"), // BUTM2010
    (102955, "+proj=tmerc +lat_0=0 +lon_0=90 +k=0.9996 +x_0=500000 +y_0=0 +ellps=evrst30 +towgs84=283.729,735.942,261.143,0,0,0,0 +units=m +no_defs"), // UTM_Gulshan
    (102956, "+proj=utm +zone=53 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(MA11)_UTM_Zone_53N
    (102957, "+proj=utm +zone=56 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(PA11)_UTM_Zone_56N
    (102958, "+proj=utm +zone=57 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(PA11)_UTM_Zone_57N
    (102959, "+proj=utm +zone=58 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(PA11)_UTM_Zone_58N
    (102960, "+proj=utm +zone=59 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(PA11)_UTM_Zone_59N
    (102961, "+proj=tmerc +lat_0=0 +lon_0=159 +k=0.99994 +x_0=300000 +y_0=5000000 +ellps=aust_SA +towgs84=-117.808,-51.536,137.784,0.303,0.446,0.234,-0.29 +units=m +no_defs"), // AGD_1966_ISG_57_2
    (102962, "+proj=aea +lat_0=0 +lon_0=-120 +lat_1=34 +lat_2=40.5 +x_0=0 +y_0=-4000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_California_Teale_Albers (deprecated)
    (102963, "+proj=tmerc +lat_0=32.5 +lon_0=-89.75 +k=0.9998335 +x_0=500000 +y_0=1300000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Mississippi_TM (deprecated)
    (102965, "+proj=aea +lat_0=23 +lon_0=-96 +lat_1=29.5 +lat_2=45.5 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Contiguous_USA_Albers (deprecated)
    (102966, "+proj=aea +lat_0=50 +lon_0=-154 +lat_1=55 +lat_2=65 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Alaska_Albers (deprecated)
    (102967, "+proj=aea +lat_0=24 +lon_0=-84 +lat_1=24 +lat_2=31.5 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Florida_GDL_Albers (deprecated)
    (102969, "+proj=lcc +lat_0=41.75 +lon_0=-120.5 +lat_1=43 +lat_2=45.5 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Oregon_Statewide_Lambert (deprecated)
    (102970, "+proj=lcc +lat_0=41.75 +lon_0=-120.5 +lat_1=43 +lat_2=45.5 +x_0=400000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_Oregon_Statewide_Lambert_Ft_Intl (deprecated)
    (102971, "+proj=aea +lat_0=18 +lon_0=-100 +lat_1=27.5 +lat_2=35 +x_0=1500000 +y_0=6000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Texas_Centric_Mapping_System_Albers (deprecated)
    (102972, "+proj=lcc +lat_0=18 +lon_0=-100 +lat_1=27.5 +lat_2=35 +x_0=1500000 +y_0=5000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Texas_Centric_Mapping_System_Lambert (deprecated)
    (102973, "+proj=tmerc +lat_0=0 +lon_0=-90 +k=0.9996 +x_0=520000 +y_0=-4480000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Wisconsin_TM (deprecated)
    (102974, "+proj=tmerc +lat_0=0 +lon_0=-90 +k=0.9996 +x_0=520000 +y_0=-4480000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_Wisconsin_TM_US_Ft
    (102975, "+proj=tmerc +lat_0=30.5 +lon_0=-85.8333333333333 +k=0.99996 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alabama_East_FIPS_0101 (deprecated)
    (102976, "+proj=tmerc +lat_0=30 +lon_0=-87.5 +k=0.999933333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alabama_West_FIPS_0102 (deprecated)
    (102978, "+proj=tmerc +lat_0=54 +lon_0=-142 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_2_FIPS_5002 (deprecated)
    (102979, "+proj=tmerc +lat_0=54 +lon_0=-146 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_3_FIPS_5003 (deprecated)
    (102980, "+proj=tmerc +lat_0=54 +lon_0=-150 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_4_FIPS_5004 (deprecated)
    (102981, "+proj=tmerc +lat_0=54 +lon_0=-154 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_5_FIPS_5005 (deprecated)
    (102982, "+proj=tmerc +lat_0=54 +lon_0=-158 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_6_FIPS_5006 (deprecated)
    (102983, "+proj=tmerc +lat_0=54 +lon_0=-162 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_7_FIPS_5007 (deprecated)
    (102984, "+proj=tmerc +lat_0=54 +lon_0=-166 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_8_FIPS_5008 (deprecated)
    (102985, "+proj=tmerc +lat_0=54 +lon_0=-170 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_9_FIPS_5009 (deprecated)
    (102986, "+proj=lcc +lat_0=51 +lon_0=-176 +lat_1=51.8333333333333 +lat_2=53.8333333333333 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Alaska_10_FIPS_5010 (deprecated)
    (102987, "+proj=tmerc +lat_0=31 +lon_0=-110.166666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Arizona_East_FIPS_0201 (deprecated)
    (102988, "+proj=tmerc +lat_0=31 +lon_0=-111.916666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Arizona_Central_FIPS_0202 (deprecated)
    (102989, "+proj=tmerc +lat_0=31 +lon_0=-113.75 +k=0.999933333333333 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Arizona_West_FIPS_0203 (deprecated)
    (102990, "+proj=tmerc +lat_0=31 +lon_0=-110.166666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Arizona_East_FIPS_0201_Ft_Intl (deprecated)
    (102991, "+proj=tmerc +lat_0=31 +lon_0=-111.916666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Arizona_Central_FIPS_0202_Ft_Intl (deprecated)
    (102992, "+proj=tmerc +lat_0=31 +lon_0=-113.75 +k=0.999933333333333 +x_0=213360 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Arizona_West_FIPS_0203_Ft_Intl (deprecated)
    (102993, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-92 +lat_1=34.9333333333333 +lat_2=36.2333333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Arkansas_North_FIPS_0301 (deprecated)
    (102994, "+proj=lcc +lat_0=32.6666666666667 +lon_0=-92 +lat_1=33.3 +lat_2=34.7666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Arkansas_South_FIPS_0302 (deprecated)
    (102995, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-92 +lat_1=34.9333333333333 +lat_2=36.2333333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Arkansas_North_FIPS_0301_Ft_US (deprecated)
    (102996, "+proj=lcc +lat_0=32.6666666666667 +lon_0=-92 +lat_1=33.3 +lat_2=34.7666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Arkansas_South_FIPS_0302_Ft_US (deprecated)
    (102997, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-122 +lat_1=40 +lat_2=41.6666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_California_I_FIPS_0401 (deprecated)
    (102998, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-122 +lat_1=38.3333333333333 +lat_2=39.8333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_California_II_FIPS_0402 (deprecated)
    (102999, "+proj=lcc +lat_0=36.5 +lon_0=-120.5 +lat_1=37.0666666666667 +lat_2=38.4333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_California_III_FIPS_0403 (deprecated)
    (103000, "+proj=lcc +lat_0=35.3333333333333 +lon_0=-119 +lat_1=36 +lat_2=37.25 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_California_IV_FIPS_0404 (deprecated)
    (103001, "+proj=lcc +lat_0=33.5 +lon_0=-118 +lat_1=34.0333333333333 +lat_2=35.4666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_California_V_FIPS_0405 (deprecated)
    (103002, "+proj=lcc +lat_0=32.1666666666667 +lon_0=-116.25 +lat_1=32.7833333333333 +lat_2=33.8833333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_California_VI_FIPS_0406 (deprecated)
    (103003, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-122 +lat_1=40 +lat_2=41.6666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_California_I_FIPS_0401_Ft_US (deprecated)
    (103004, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-122 +lat_1=38.3333333333333 +lat_2=39.8333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_California_II_FIPS_0402_Ft_US (deprecated)
    (103005, "+proj=lcc +lat_0=36.5 +lon_0=-120.5 +lat_1=37.0666666666667 +lat_2=38.4333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_California_III_FIPS_0403_Ft_US (deprecated)
    (103006, "+proj=lcc +lat_0=35.3333333333333 +lon_0=-119 +lat_1=36 +lat_2=37.25 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_California_IV_FIPS_0404_Ft_US (deprecated)
    (103007, "+proj=lcc +lat_0=33.5 +lon_0=-118 +lat_1=34.0333333333333 +lat_2=35.4666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_California_V_FIPS_0405_Ft_US (deprecated)
    (103008, "+proj=lcc +lat_0=32.1666666666667 +lon_0=-116.25 +lat_1=32.7833333333333 +lat_2=33.8833333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_California_VI_FIPS_0406_Ft_US (deprecated)
    (103009, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-105.5 +lat_1=39.7166666666667 +lat_2=40.7833333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Colorado_North_FIPS_0501 (deprecated)
    (103010, "+proj=lcc +lat_0=37.8333333333333 +lon_0=-105.5 +lat_1=38.45 +lat_2=39.75 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Colorado_Central_FIPS_0502 (deprecated)
    (103011, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-105.5 +lat_1=37.2333333333333 +lat_2=38.4333333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Colorado_South_FIPS_0503 (deprecated)
    (103012, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-105.5 +lat_1=39.7166666666667 +lat_2=40.7833333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Colorado_North_FIPS_0501_Ft_US (deprecated)
    (103013, "+proj=lcc +lat_0=37.8333333333333 +lon_0=-105.5 +lat_1=38.45 +lat_2=39.75 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Colorado_Central_FIPS_0502_Ft_US (deprecated)
    (103014, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-105.5 +lat_1=37.2333333333333 +lat_2=38.4333333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Colorado_South_FIPS_0503_Ft_US (deprecated)
    (103015, "+proj=lcc +lat_0=40.8333333333333 +lon_0=-72.75 +lat_1=41.2 +lat_2=41.8666666666667 +x_0=304800.6096 +y_0=152400.3048 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Connecticut_FIPS_0600 (deprecated)
    (103016, "+proj=lcc +lat_0=40.8333333333333 +lon_0=-72.75 +lat_1=41.2 +lat_2=41.8666666666667 +x_0=304800.6096 +y_0=152400.3048 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Connecticut_FIPS_0600_Ft_US (deprecated)
    (103017, "+proj=tmerc +lat_0=38 +lon_0=-75.4166666666667 +k=0.999995 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Delaware_FIPS_0700 (deprecated)
    (103018, "+proj=tmerc +lat_0=38 +lon_0=-75.4166666666667 +k=0.999995 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Delaware_FIPS_0700_Ft_US (deprecated)
    (103019, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-81 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Florida_East_FIPS_0901 (deprecated)
    (103020, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-82 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Florida_West_FIPS_0902 (deprecated)
    (103021, "+proj=lcc +lat_0=29 +lon_0=-84.5 +lat_1=29.5833333333333 +lat_2=30.75 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Florida_North_FIPS_0903 (deprecated)
    (103022, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-81 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Florida_East_FIPS_0901_Ft_US (deprecated)
    (103023, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-82 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Florida_West_FIPS_0902_Ft_US (deprecated)
    (103024, "+proj=lcc +lat_0=29 +lon_0=-84.5 +lat_1=29.5833333333333 +lat_2=30.75 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Florida_North_FIPS_0903_Ft_US (deprecated)
    (103025, "+proj=tmerc +lat_0=30 +lon_0=-82.1666666666667 +k=0.9999 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Georgia_East_FIPS_1001 (deprecated)
    (103026, "+proj=tmerc +lat_0=30 +lon_0=-84.1666666666667 +k=0.9999 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Georgia_West_FIPS_1002 (deprecated)
    (103027, "+proj=tmerc +lat_0=30 +lon_0=-82.1666666666667 +k=0.9999 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Georgia_East_FIPS_1001_Ft_US (deprecated)
    (103028, "+proj=tmerc +lat_0=30 +lon_0=-84.1666666666667 +k=0.9999 +x_0=700000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Georgia_West_FIPS_1002_Ft_US (deprecated)
    (103029, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-112.166666666667 +k=0.999947368421053 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Idaho_East_FIPS_1101 (deprecated)
    (103030, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-114 +k=0.999947368421053 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Idaho_Central_FIPS_1102 (deprecated)
    (103031, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-115.75 +k=0.999933333333333 +x_0=800000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Idaho_West_FIPS_1103 (deprecated)
    (103032, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-112.166666666667 +k=0.999947368421053 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Idaho_East_FIPS_1101_Ft_US (deprecated)
    (103033, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-114 +k=0.999947368421053 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Idaho_Central_FIPS_1102_Ft_US (deprecated)
    (103034, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-115.75 +k=0.999933333333333 +x_0=800000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Idaho_West_FIPS_1103_Ft_US (deprecated)
    (103035, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-88.3333333333333 +k=0.999975 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Illinois_East_FIPS_1201 (deprecated)
    (103036, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-90.1666666666667 +k=0.999941176470588 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Illinois_West_FIPS_1202 (deprecated)
    (103037, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-88.3333333333333 +k=0.999975 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Illinois_East_FIPS_1201_Ft_US (deprecated)
    (103038, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-90.1666666666667 +k=0.999941176470588 +x_0=700000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Illinois_West_FIPS_1202_Ft_US (deprecated)
    (103039, "+proj=tmerc +lat_0=37.5 +lon_0=-85.6666666666667 +k=0.999966666666667 +x_0=100000 +y_0=250000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Indiana_East_FIPS_1301 (deprecated)
    (103040, "+proj=tmerc +lat_0=37.5 +lon_0=-87.0833333333333 +k=0.999966666666667 +x_0=900000 +y_0=250000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Indiana_West_FIPS_1302 (deprecated)
    (103041, "+proj=tmerc +lat_0=37.5 +lon_0=-85.6666666666667 +k=0.999966666666667 +x_0=100000 +y_0=250000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Indiana_East_FIPS_1301_Ft_US (deprecated)
    (103042, "+proj=tmerc +lat_0=37.5 +lon_0=-87.0833333333333 +k=0.999966666666667 +x_0=900000 +y_0=250000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Indiana_West_FIPS_1302_Ft_US (deprecated)
    (103043, "+proj=lcc +lat_0=41.5 +lon_0=-93.5 +lat_1=42.0666666666667 +lat_2=43.2666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Iowa_North_FIPS_1401 (deprecated)
    (103044, "+proj=lcc +lat_0=40 +lon_0=-93.5 +lat_1=40.6166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Iowa_South_FIPS_1402 (deprecated)
    (103045, "+proj=lcc +lat_0=41.5 +lon_0=-93.5 +lat_1=42.0666666666667 +lat_2=43.2666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Iowa_North_FIPS_1401_Ft_US (deprecated)
    (103046, "+proj=lcc +lat_0=40 +lon_0=-93.5 +lat_1=40.6166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Iowa_South_FIPS_1402_Ft_US (deprecated)
    (103047, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-98 +lat_1=38.7166666666667 +lat_2=39.7833333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Kansas_North_FIPS_1501 (deprecated)
    (103048, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-98.5 +lat_1=37.2666666666667 +lat_2=38.5666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Kansas_South_FIPS_1502 (deprecated)
    (103049, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-98 +lat_1=38.7166666666667 +lat_2=39.7833333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Kansas_North_FIPS_1501_Ft_US (deprecated)
    (103050, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-98.5 +lat_1=37.2666666666667 +lat_2=38.5666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Kansas_South_FIPS_1502_Ft_US (deprecated)
    (103051, "+proj=lcc +lat_0=37.5 +lon_0=-84.25 +lat_1=37.9666666666667 +lat_2=38.9666666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Kentucky_North_FIPS_1601 (deprecated)
    (103052, "+proj=lcc +lat_0=37.5 +lon_0=-84.25 +lat_1=37.9666666666667 +lat_2=38.9666666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Kentucky_North_FIPS_1601_Ft_US (deprecated)
    (103053, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=37.0833333333333 +lat_2=38.6666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Kentucky_FIPS_1600 (deprecated)
    (103054, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=37.0833333333333 +lat_2=38.6666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Kentucky_FIPS_1600_Ft_US (deprecated)
    (103055, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=36.7333333333333 +lat_2=37.9333333333333 +x_0=500000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Kentucky_South_FIPS_1602 (deprecated)
    (103056, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=36.7333333333333 +lat_2=37.9333333333333 +x_0=500000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Kentucky_South_FIPS_1602_Ft_US (deprecated)
    (103057, "+proj=lcc +lat_0=30.5 +lon_0=-92.5 +lat_1=31.1666666666667 +lat_2=32.6666666666667 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Louisiana_North_FIPS_1701 (deprecated)
    (103058, "+proj=lcc +lat_0=28.5 +lon_0=-91.3333333333333 +lat_1=29.3 +lat_2=30.7 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Louisiana_South_FIPS_1702 (deprecated)
    (103059, "+proj=lcc +lat_0=30.5 +lon_0=-92.5 +lat_1=31.1666666666667 +lat_2=32.6666666666667 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Louisiana_North_FIPS_1701_Ft_US (deprecated)
    (103060, "+proj=lcc +lat_0=28.5 +lon_0=-91.3333333333333 +lat_1=29.3 +lat_2=30.7 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Louisiana_South_FIPS_1702_Ft_US (deprecated)
    (103061, "+proj=tmerc +lat_0=43.6666666666667 +lon_0=-68.5 +k=0.9999 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Maine_East_FIPS_1801 (deprecated)
    (103062, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.1666666666667 +k=0.999966666666667 +x_0=900000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Maine_West_FIPS_1802 (deprecated)
    (103063, "+proj=tmerc +lat_0=43.6666666666667 +lon_0=-68.5 +k=0.9999 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Maine_East_FIPS_1801_Ft_US (deprecated)
    (103064, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.1666666666667 +k=0.999966666666667 +x_0=900000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Maine_West_FIPS_1802_Ft_US (deprecated)
    (103065, "+proj=tmerc +lat_0=43.8333333333333 +lon_0=-67.875 +k=0.99998 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Maine_2000_East_Zone (deprecated)
    (103066, "+proj=tmerc +lat_0=43.5 +lon_0=-69.125 +k=0.99998 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Maine_2000_Central_Zone (deprecated)
    (103067, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.375 +k=0.99998 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_Maine_2000_West_Zone (deprecated)
    (103068, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-77 +lat_1=38.3 +lat_2=39.45 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Maryland_FIPS_1900 (deprecated)
    (103069, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-77 +lat_1=38.3 +lat_2=39.45 +x_0=400000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Maryland_FIPS_1900_Ft_US (deprecated)
    (103070, "+proj=lcc +lat_0=41 +lon_0=-71.5 +lat_1=41.7166666666667 +lat_2=42.6833333333333 +x_0=200000 +y_0=750000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Massachusetts_Mainland_FIPS_2001 (deprecated)
    (103071, "+proj=lcc +lat_0=41 +lon_0=-70.5 +lat_1=41.2833333333333 +lat_2=41.4833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Massachusetts_Island_FIPS_2002 (deprecated)
    (103072, "+proj=lcc +lat_0=41 +lon_0=-71.5 +lat_1=41.7166666666667 +lat_2=42.6833333333333 +x_0=200000 +y_0=750000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Massachusetts_Mnld_FIPS_2001_FtUS (deprecated)
    (103073, "+proj=lcc +lat_0=41 +lon_0=-70.5 +lat_1=41.2833333333333 +lat_2=41.4833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Massachusetts_Isl_FIPS_2002_FtUS (deprecated)
    (103074, "+proj=lcc +lat_0=44.7833333333333 +lon_0=-87 +lat_1=45.4833333333333 +lat_2=47.0833333333333 +x_0=8000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Michigan_North_FIPS_2111 (deprecated)
    (103075, "+proj=lcc +lat_0=43.3166666666667 +lon_0=-84.3666666666667 +lat_1=44.1833333333333 +lat_2=45.7 +x_0=6000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Michigan_Central_FIPS_2112 (deprecated)
    (103076, "+proj=lcc +lat_0=41.5 +lon_0=-84.3666666666667 +lat_1=42.1 +lat_2=43.6666666666667 +x_0=4000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Michigan_South_FIPS_2113 (deprecated)
    (103077, "+proj=lcc +lat_0=44.7833333333333 +lon_0=-87 +lat_1=45.4833333333333 +lat_2=47.0833333333333 +x_0=8000000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Michigan_North_FIPS_2111_Ft_Intl (deprecated)
    (103078, "+proj=lcc +lat_0=43.3166666666667 +lon_0=-84.3666666666667 +lat_1=44.1833333333333 +lat_2=45.7 +x_0=6000000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Michigan_Central_FIPS_2112_Ft_Intl (deprecated)
    (103079, "+proj=lcc +lat_0=41.5 +lon_0=-84.3666666666667 +lat_1=42.1 +lat_2=43.6666666666667 +x_0=4000000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Michigan_South_FIPS_2113_Ft_Intl (deprecated)
    (103080, "+proj=lcc +lat_0=46.5 +lon_0=-93.1 +lat_1=47.0333333333333 +lat_2=48.6333333333333 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Minnesota_North_FIPS_2201 (deprecated)
    (103081, "+proj=lcc +lat_0=45 +lon_0=-94.25 +lat_1=45.6166666666667 +lat_2=47.05 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Minnesota_Central_FIPS_2202 (deprecated)
    (103082, "+proj=lcc +lat_0=43 +lon_0=-94 +lat_1=43.7833333333333 +lat_2=45.2166666666667 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Minnesota_South_FIPS_2203 (deprecated)
    (103083, "+proj=lcc +lat_0=46.5 +lon_0=-93.1 +lat_1=47.0333333333333 +lat_2=48.6333333333333 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Minnesota_North_FIPS_2201_Ft_US (deprecated)
    (103084, "+proj=lcc +lat_0=45 +lon_0=-94.25 +lat_1=45.6166666666667 +lat_2=47.05 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Minnesota_Central_FIPS_2202_Ft_US (deprecated)
    (103085, "+proj=lcc +lat_0=43 +lon_0=-94 +lat_1=43.7833333333333 +lat_2=45.2166666666667 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Minnesota_South_FIPS_2203_Ft_US (deprecated)
    (103086, "+proj=tmerc +lat_0=29.5 +lon_0=-88.8333333333333 +k=0.99995 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Mississippi_East_FIPS_2301 (deprecated)
    (103087, "+proj=tmerc +lat_0=29.5 +lon_0=-90.3333333333333 +k=0.99995 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Mississippi_West_FIPS_2302 (deprecated)
    (103088, "+proj=tmerc +lat_0=29.5 +lon_0=-88.8333333333333 +k=0.99995 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Mississippi_East_FIPS_2301_Ft_US (deprecated)
    (103089, "+proj=tmerc +lat_0=29.5 +lon_0=-90.3333333333333 +k=0.99995 +x_0=700000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Mississippi_West_FIPS_2302_Ft_US (deprecated)
    (103090, "+proj=tmerc +lat_0=35.8333333333333 +lon_0=-90.5 +k=0.999933333333333 +x_0=250000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Missouri_East_FIPS_2401 (deprecated)
    (103091, "+proj=tmerc +lat_0=35.8333333333333 +lon_0=-92.5 +k=0.999933333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Missouri_Central_FIPS_2402 (deprecated)
    (103092, "+proj=tmerc +lat_0=36.1666666666667 +lon_0=-94.5 +k=0.999941176470588 +x_0=850000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Missouri_West_FIPS_2403 (deprecated)
    (103093, "+proj=lcc +lat_0=44.25 +lon_0=-109.5 +lat_1=45 +lat_2=49 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Montana_FIPS_2500 (deprecated)
    (103094, "+proj=lcc +lat_0=44.25 +lon_0=-109.5 +lat_1=45 +lat_2=49 +x_0=600000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Montana_FIPS_2500_Ft_Intl (deprecated)
    (103095, "+proj=lcc +lat_0=39.8333333333333 +lon_0=-100 +lat_1=40 +lat_2=43 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Nebraska_FIPS_2600 (deprecated)
    (103096, "+proj=lcc +lat_0=39.8333333333333 +lon_0=-100 +lat_1=40 +lat_2=43 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Nebraska_FIPS_2600_Ft_US (deprecated)
    (103097, "+proj=tmerc +lat_0=34.75 +lon_0=-115.583333333333 +k=0.9999 +x_0=200000 +y_0=8000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Nevada_East_FIPS_2701 (deprecated)
    (103098, "+proj=tmerc +lat_0=34.75 +lon_0=-116.666666666667 +k=0.9999 +x_0=500000 +y_0=6000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Nevada_Central_FIPS_2702 (deprecated)
    (103099, "+proj=tmerc +lat_0=34.75 +lon_0=-118.583333333333 +k=0.9999 +x_0=800000 +y_0=4000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Nevada_West_FIPS_2703 (deprecated)
    (103100, "+proj=tmerc +lat_0=34.75 +lon_0=-115.583333333333 +k=0.9999 +x_0=200000 +y_0=8000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Nevada_East_FIPS_2701_Ft_US (deprecated)
    (103101, "+proj=tmerc +lat_0=34.75 +lon_0=-116.666666666667 +k=0.9999 +x_0=500000 +y_0=6000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Nevada_Central_FIPS_2702_Ft_US (deprecated)
    (103102, "+proj=tmerc +lat_0=34.75 +lon_0=-118.583333333333 +k=0.9999 +x_0=800000 +y_0=4000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Nevada_West_FIPS_2703_Ft_US (deprecated)
    (103103, "+proj=tmerc +lat_0=42.5 +lon_0=-71.6666666666667 +k=0.999966666666667 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_Hampshire_FIPS_2800 (deprecated)
    (103104, "+proj=tmerc +lat_0=42.5 +lon_0=-71.6666666666667 +k=0.999966666666667 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_Hampshire_FIPS_2800_Ft_US (deprecated)
    (103105, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_Jersey_FIPS_2900 (deprecated)
    (103106, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_Jersey_FIPS_2900_Ft_US (deprecated)
    (103107, "+proj=tmerc +lat_0=31 +lon_0=-104.333333333333 +k=0.999909090909091 +x_0=165000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_Mexico_East_FIPS_3001 (deprecated)
    (103108, "+proj=tmerc +lat_0=31 +lon_0=-106.25 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_Mexico_Central_FIPS_3002 (deprecated)
    (103109, "+proj=tmerc +lat_0=31 +lon_0=-107.833333333333 +k=0.999916666666667 +x_0=830000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_Mexico_West_FIPS_3003 (deprecated)
    (103110, "+proj=tmerc +lat_0=31 +lon_0=-104.333333333333 +k=0.999909090909091 +x_0=165000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_Mexico_East_FIPS_3001_Ft_US (deprecated)
    (103111, "+proj=tmerc +lat_0=31 +lon_0=-106.25 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_Mexico_Central_FIPS_3002_Ft_US (deprecated)
    (103112, "+proj=tmerc +lat_0=31 +lon_0=-107.833333333333 +k=0.999916666666667 +x_0=830000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_Mexico_West_FIPS_3003_Ft_US (deprecated)
    (103113, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_York_East_FIPS_3101 (deprecated)
    (103114, "+proj=tmerc +lat_0=40 +lon_0=-76.5833333333333 +k=0.9999375 +x_0=250000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_York_Central_FIPS_3102 (deprecated)
    (103115, "+proj=tmerc +lat_0=40 +lon_0=-78.5833333333333 +k=0.9999375 +x_0=350000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_York_West_FIPS_3103 (deprecated)
    (103116, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-74 +lat_1=40.6666666666667 +lat_2=41.0333333333333 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_New_York_Long_Island_FIPS_3104 (deprecated)
    (103117, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_York_East_FIPS_3101_Ft_US (deprecated)
    (103118, "+proj=tmerc +lat_0=40 +lon_0=-76.5833333333333 +k=0.9999375 +x_0=250000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_York_Central_FIPS_3102_Ft_US (deprecated)
    (103119, "+proj=tmerc +lat_0=40 +lon_0=-78.5833333333333 +k=0.9999375 +x_0=350000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_York_West_FIPS_3103_Ft_US (deprecated)
    (103120, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-74 +lat_1=40.6666666666667 +lat_2=41.0333333333333 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_New_York_Long_Isl_FIPS_3104_Ft_US (deprecated)
    (103121, "+proj=lcc +lat_0=33.75 +lon_0=-79 +lat_1=34.3333333333333 +lat_2=36.1666666666667 +x_0=609601.219202438 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_North_Carolina_FIPS_3200 (deprecated)
    (103122, "+proj=lcc +lat_0=33.75 +lon_0=-79 +lat_1=34.3333333333333 +lat_2=36.1666666666667 +x_0=609601.219202438 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_North_Carolina_FIPS_3200_Ft_US (deprecated)
    (103123, "+proj=lcc +lat_0=47 +lon_0=-100.5 +lat_1=47.4333333333333 +lat_2=48.7333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_North_Dakota_North_FIPS_3301 (deprecated)
    (103124, "+proj=lcc +lat_0=45.6666666666667 +lon_0=-100.5 +lat_1=46.1833333333333 +lat_2=47.4833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_North_Dakota_South_FIPS_3302 (deprecated)
    (103125, "+proj=lcc +lat_0=47 +lon_0=-100.5 +lat_1=47.4333333333333 +lat_2=48.7333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_North_Dakota_North_FIPS_3301_FtI (deprecated)
    (103126, "+proj=lcc +lat_0=45.6666666666667 +lon_0=-100.5 +lat_1=46.1833333333333 +lat_2=47.4833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_North_Dakota_South_FIPS_3302_FtI (deprecated)
    (103127, "+proj=lcc +lat_0=39.6666666666667 +lon_0=-82.5 +lat_1=40.4333333333333 +lat_2=41.7 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Ohio_North_FIPS_3401 (deprecated)
    (103128, "+proj=lcc +lat_0=38 +lon_0=-82.5 +lat_1=38.7333333333333 +lat_2=40.0333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Ohio_South_FIPS_3402 (deprecated)
    (103129, "+proj=lcc +lat_0=39.6666666666667 +lon_0=-82.5 +lat_1=40.4333333333333 +lat_2=41.7 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Ohio_North_FIPS_3401_Ft_US (deprecated)
    (103130, "+proj=lcc +lat_0=38 +lon_0=-82.5 +lat_1=38.7333333333333 +lat_2=40.0333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Ohio_South_FIPS_3402_Ft_US (deprecated)
    (103131, "+proj=lcc +lat_0=35 +lon_0=-98 +lat_1=35.5666666666667 +lat_2=36.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Oklahoma_North_FIPS_3501 (deprecated)
    (103132, "+proj=lcc +lat_0=33.3333333333333 +lon_0=-98 +lat_1=33.9333333333333 +lat_2=35.2333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Oklahoma_South_FIPS_3502 (deprecated)
    (103133, "+proj=lcc +lat_0=35 +lon_0=-98 +lat_1=35.5666666666667 +lat_2=36.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Oklahoma_North_FIPS_3501_Ft_US (deprecated)
    (103134, "+proj=lcc +lat_0=33.3333333333333 +lon_0=-98 +lat_1=33.9333333333333 +lat_2=35.2333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Oklahoma_South_FIPS_3502_Ft_US (deprecated)
    (103135, "+proj=lcc +lat_0=43.6666666666667 +lon_0=-120.5 +lat_1=44.3333333333333 +lat_2=46 +x_0=2500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Oregon_North_FIPS_3601 (deprecated)
    (103136, "+proj=lcc +lat_0=41.6666666666667 +lon_0=-120.5 +lat_1=42.3333333333333 +lat_2=44 +x_0=1500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Oregon_South_FIPS_3602 (deprecated)
    (103137, "+proj=lcc +lat_0=43.6666666666667 +lon_0=-120.5 +lat_1=44.3333333333333 +lat_2=46 +x_0=2500000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Oregon_North_FIPS_3601_Ft_Intl (deprecated)
    (103138, "+proj=lcc +lat_0=41.6666666666667 +lon_0=-120.5 +lat_1=42.3333333333333 +lat_2=44 +x_0=1500000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Oregon_South_FIPS_3602_Ft_Intl (deprecated)
    (103139, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-77.75 +lat_1=40.8833333333333 +lat_2=41.95 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Pennsylvania_North_FIPS_3701 (deprecated)
    (103140, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-77.75 +lat_1=40.8833333333333 +lat_2=41.95 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Pennsylvania_North_FIPS_3701_Ft_US (deprecated)
    (103141, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-77.75 +lat_1=39.9333333333333 +lat_2=40.9666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Pennsylvania_South_FIPS_3702 (deprecated)
    (103142, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-77.75 +lat_1=39.9333333333333 +lat_2=40.9666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Pennsylvania_South_FIPS_3702_Ft_US (deprecated)
    (103143, "+proj=tmerc +lat_0=41.0833333333333 +lon_0=-71.5 +k=0.99999375 +x_0=100000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Rhode_Island_FIPS_3800 (deprecated)
    (103144, "+proj=tmerc +lat_0=41.0833333333333 +lon_0=-71.5 +k=0.99999375 +x_0=100000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Rhode_Island_FIPS_3800_Ft_US (deprecated)
    (103145, "+proj=lcc +lat_0=31.8333333333333 +lon_0=-81 +lat_1=32.5 +lat_2=34.8333333333333 +x_0=609600 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_South_Carolina_FIPS_3900 (deprecated)
    (103146, "+proj=lcc +lat_0=31.8333333333333 +lon_0=-81 +lat_1=32.5 +lat_2=34.8333333333333 +x_0=609600 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_South_Carolina_FIPS_3900_Ft_Intl (deprecated)
    (103147, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-100 +lat_1=44.4166666666667 +lat_2=45.6833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_South_Dakota_North_FIPS_4001 (deprecated)
    (103148, "+proj=lcc +lat_0=42.3333333333333 +lon_0=-100.333333333333 +lat_1=42.8333333333333 +lat_2=44.4 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_South_Dakota_South_FIPS_4002 (deprecated)
    (103149, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-100 +lat_1=44.4166666666667 +lat_2=45.6833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_South_Dakota_North_FIPS_4001_Ft_US (deprecated)
    (103150, "+proj=lcc +lat_0=42.3333333333333 +lon_0=-100.333333333333 +lat_1=42.8333333333333 +lat_2=44.4 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_South_Dakota_South_FIPS_4002_Ft_US (deprecated)
    (103151, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-86 +lat_1=35.25 +lat_2=36.4166666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Tennessee_FIPS_4100 (deprecated)
    (103152, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-86 +lat_1=35.25 +lat_2=36.4166666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Tennessee_FIPS_4100_Ft_US (deprecated)
    (103153, "+proj=lcc +lat_0=34 +lon_0=-101.5 +lat_1=34.65 +lat_2=36.1833333333333 +x_0=200000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Texas_North_FIPS_4201 (deprecated)
    (103154, "+proj=lcc +lat_0=31.6666666666667 +lon_0=-98.5 +lat_1=32.1333333333333 +lat_2=33.9666666666667 +x_0=600000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Texas_North_Central_FIPS_4202 (deprecated)
    (103155, "+proj=lcc +lat_0=29.6666666666667 +lon_0=-100.333333333333 +lat_1=30.1166666666667 +lat_2=31.8833333333333 +x_0=700000 +y_0=3000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Texas_Central_FIPS_4203 (deprecated)
    (103156, "+proj=lcc +lat_0=27.8333333333333 +lon_0=-99 +lat_1=28.3833333333333 +lat_2=30.2833333333333 +x_0=600000 +y_0=4000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Texas_South_Central_FIPS_4204 (deprecated)
    (103157, "+proj=lcc +lat_0=25.6666666666667 +lon_0=-98.5 +lat_1=26.1666666666667 +lat_2=27.8333333333333 +x_0=300000 +y_0=5000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Texas_South_FIPS_4205 (deprecated)
    (103158, "+proj=lcc +lat_0=34 +lon_0=-101.5 +lat_1=34.65 +lat_2=36.1833333333333 +x_0=200000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Texas_North_FIPS_4201_Ft_US (deprecated)
    (103159, "+proj=lcc +lat_0=31.6666666666667 +lon_0=-98.5 +lat_1=32.1333333333333 +lat_2=33.9666666666667 +x_0=600000 +y_0=2000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Texas_North_Central_FIPS_4202_FtUS (deprecated)
    (103160, "+proj=lcc +lat_0=29.6666666666667 +lon_0=-100.333333333333 +lat_1=30.1166666666667 +lat_2=31.8833333333333 +x_0=700000 +y_0=3000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Texas_Central_FIPS_4203_Ft_US (deprecated)
    (103161, "+proj=lcc +lat_0=27.8333333333333 +lon_0=-99 +lat_1=28.3833333333333 +lat_2=30.2833333333333 +x_0=600000 +y_0=4000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Texas_South_Central_FIPS_4204_FtUS (deprecated)
    (103162, "+proj=lcc +lat_0=25.6666666666667 +lon_0=-98.5 +lat_1=26.1666666666667 +lat_2=27.8333333333333 +x_0=300000 +y_0=5000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Texas_South_FIPS_4205_Ft_US (deprecated)
    (103163, "+proj=lcc +lat_0=40.3333333333333 +lon_0=-111.5 +lat_1=40.7166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Utah_North_FIPS_4301 (deprecated)
    (103164, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-111.5 +lat_1=39.0166666666667 +lat_2=40.65 +x_0=500000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Utah_Central_FIPS_4302 (deprecated)
    (103165, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-111.5 +lat_1=37.2166666666667 +lat_2=38.35 +x_0=500000 +y_0=3000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Utah_South_FIPS_4303 (deprecated)
    (103166, "+proj=lcc +lat_0=40.3333333333333 +lon_0=-111.5 +lat_1=40.7166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=1000000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Utah_North_FIPS_4301_Ft_Intl
    (103167, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-111.5 +lat_1=39.0166666666667 +lat_2=40.65 +x_0=500000 +y_0=2000000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Utah_Central_FIPS_4302_Ft_Intl
    (103168, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-111.5 +lat_1=37.2166666666667 +lat_2=38.35 +x_0=500000 +y_0=3000000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_StatePlane_Utah_South_FIPS_4303_Ft_Intl
    (103169, "+proj=lcc +lat_0=40.3333333333333 +lon_0=-111.5 +lat_1=40.7166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Utah_North_FIPS_4301_Ft_US (deprecated)
    (103170, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-111.5 +lat_1=39.0166666666667 +lat_2=40.65 +x_0=500000 +y_0=2000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Utah_Central_FIPS_4302_Ft_US (deprecated)
    (103171, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-111.5 +lat_1=37.2166666666667 +lat_2=38.35 +x_0=500000 +y_0=3000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Utah_South_FIPS_4303_Ft_US (deprecated)
    (103172, "+proj=tmerc +lat_0=42.5 +lon_0=-72.5 +k=0.999964285714286 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Vermont_FIPS_4400 (deprecated)
    (103173, "+proj=tmerc +lat_0=42.5 +lon_0=-72.5 +k=0.999964285714286 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Vermont_FIPS_4400_Ft_US (deprecated)
    (103174, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-78.5 +lat_1=38.0333333333333 +lat_2=39.2 +x_0=3500000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Virginia_North_FIPS_4501 (deprecated)
    (103175, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-78.5 +lat_1=36.7666666666667 +lat_2=37.9666666666667 +x_0=3500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Virginia_South_FIPS_4502 (deprecated)
    (103176, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-78.5 +lat_1=38.0333333333333 +lat_2=39.2 +x_0=3500000 +y_0=2000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Virginia_North_FIPS_4501_Ft_US (deprecated)
    (103177, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-78.5 +lat_1=36.7666666666667 +lat_2=37.9666666666667 +x_0=3500000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Virginia_South_FIPS_4502_Ft_US (deprecated)
    (103178, "+proj=lcc +lat_0=47 +lon_0=-120.833333333333 +lat_1=47.5 +lat_2=48.7333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Washington_North_FIPS_4601 (deprecated)
    (103179, "+proj=lcc +lat_0=45.3333333333333 +lon_0=-120.5 +lat_1=45.8333333333333 +lat_2=47.3333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Washington_South_FIPS_4602 (deprecated)
    (103180, "+proj=lcc +lat_0=47 +lon_0=-120.833333333333 +lat_1=47.5 +lat_2=48.7333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Washington_North_FIPS_4601_Ft_US (deprecated)
    (103181, "+proj=lcc +lat_0=45.3333333333333 +lon_0=-120.5 +lat_1=45.8333333333333 +lat_2=47.3333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Washington_South_FIPS_4602_Ft_US (deprecated)
    (103182, "+proj=lcc +lat_0=38.5 +lon_0=-79.5 +lat_1=39 +lat_2=40.25 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_West_Virginia_North_FIPS_4701 (deprecated)
    (103183, "+proj=lcc +lat_0=37 +lon_0=-81 +lat_1=37.4833333333333 +lat_2=38.8833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_West_Virginia_South_FIPS_4702 (deprecated)
    (103184, "+proj=lcc +lat_0=38.5 +lon_0=-79.5 +lat_1=39 +lat_2=40.25 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_West_Virginia_North_FIPS_4701_FtUS (deprecated)
    (103185, "+proj=lcc +lat_0=37 +lon_0=-81 +lat_1=37.4833333333333 +lat_2=38.8833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_West_Virginia_South_FIPS_4702_FtUS (deprecated)
    (103186, "+proj=lcc +lat_0=45.1666666666667 +lon_0=-90 +lat_1=45.5666666666667 +lat_2=46.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Wisconsin_North_FIPS_4801 (deprecated)
    (103187, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-90 +lat_1=44.25 +lat_2=45.5 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Wisconsin_Central_FIPS_4802 (deprecated)
    (103188, "+proj=lcc +lat_0=42 +lon_0=-90 +lat_1=42.7333333333333 +lat_2=44.0666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Wisconsin_South_FIPS_4803 (deprecated)
    (103189, "+proj=lcc +lat_0=45.1666666666667 +lon_0=-90 +lat_1=45.5666666666667 +lat_2=46.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Wisconsin_North_FIPS_4801_Ft_US (deprecated)
    (103190, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-90 +lat_1=44.25 +lat_2=45.5 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Wisconsin_Central_FIPS_4802_Ft_US (deprecated)
    (103191, "+proj=lcc +lat_0=42 +lon_0=-90 +lat_1=42.7333333333333 +lat_2=44.0666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Wisconsin_South_FIPS_4803_Ft_US (deprecated)
    (103192, "+proj=tmerc +lat_0=40.5 +lon_0=-105.166666666667 +k=0.9999375 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Wyoming_East_FIPS_4901 (deprecated)
    (103193, "+proj=tmerc +lat_0=40.5 +lon_0=-107.333333333333 +k=0.9999375 +x_0=400000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Wyoming_East_Central_FIPS_4902 (deprecated)
    (103194, "+proj=tmerc +lat_0=40.5 +lon_0=-108.75 +k=0.9999375 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Wyoming_West_Central_FIPS_4903 (deprecated)
    (103195, "+proj=tmerc +lat_0=40.5 +lon_0=-110.083333333333 +k=0.9999375 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Wyoming_West_FIPS_4904 (deprecated)
    (103196, "+proj=tmerc +lat_0=40.5 +lon_0=-105.166666666667 +k=0.9999375 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Wyoming_East_FIPS_4901_Ft_US (deprecated)
    (103197, "+proj=tmerc +lat_0=40.5 +lon_0=-107.333333333333 +k=0.9999375 +x_0=400000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Wyoming_E_Central_FIPS_4902_Ft_US (deprecated)
    (103198, "+proj=tmerc +lat_0=40.5 +lon_0=-108.75 +k=0.9999375 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Wyoming_W_Central_FIPS_4903_Ft_US (deprecated)
    (103199, "+proj=tmerc +lat_0=40.5 +lon_0=-110.083333333333 +k=0.9999375 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_StatePlane_Wyoming_West_FIPS_4904_Ft_US (deprecated)
    (103200, "+proj=lcc +lat_0=17.8333333333333 +lon_0=-66.4333333333333 +lat_1=18.0333333333333 +lat_2=18.4333333333333 +x_0=200000 +y_0=200000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_StatePlane_Puerto_Rico_Virgin_Isls_FIPS_5200 (deprecated)
    (103201, "+proj=tmerc +lat_0=0 +lon_0=12 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_12 (deprecated)
    (103202, "+proj=tmerc +lat_0=0 +lon_0=14 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_14 (deprecated)
    (103203, "+proj=tmerc +lat_0=0 +lon_0=16 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_16 (deprecated)
    (103204, "+proj=tmerc +lat_0=0 +lon_0=18 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_18 (deprecated)
    (103205, "+proj=tmerc +lat_0=0 +lon_0=20 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_20 (deprecated)
    (103206, "+proj=tmerc +lat_0=0 +lon_0=22 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_22 (deprecated)
    (103207, "+proj=tmerc +lat_0=0 +lon_0=24 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_24 (deprecated)
    (103208, "+proj=tmerc +lat_0=0 +lon_0=26 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_26 (deprecated)
    (103209, "+proj=tmerc +lat_0=0 +lon_0=28 +k=0.9999 +x_0=500000 +y_0=10000000 +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_Congo_TM_Zone_28 (deprecated)
    (103210, "+proj=utm +zone=33 +south +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_UTM_Zone_33S (deprecated)
    (103211, "+proj=utm +zone=34 +south +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_UTM_Zone_34S (deprecated)
    (103212, "+proj=utm +zone=35 +south +ellps=GRS80 +units=m +no_defs"), // RGRDC_2005_UTM_Zone_35S (deprecated)
    (103213, "+proj=utm +zone=23 +south +ellps=intl +towgs84=-134,229,-29,0,0,0,0 +units=m +no_defs"), // Chua_UTM_Zone_23S (deprecated)
    (103214, "+proj=utm +zone=27 +ellps=GRS80 +units=m +no_defs"), // REGCAN95_UTM_Zone_27N (deprecated)
    (103215, "+proj=utm +zone=28 +ellps=GRS80 +units=m +no_defs"), // REGCAN95_UTM_Zone_28N (deprecated)
    (103216, "+proj=tmerc +lat_0=0 +lon_0=9 +k=0.99998 +x_0=200000 +y_0=-5000000 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_DKTM1 (deprecated)
    (103217, "+proj=tmerc +lat_0=0 +lon_0=10 +k=0.99998 +x_0=400000 +y_0=-5000000 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_DKTM2 (deprecated)
    (103218, "+proj=tmerc +lat_0=0 +lon_0=11.75 +k=0.99998 +x_0=600000 +y_0=-5000000 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_DKTM3 (deprecated)
    (103219, "+proj=tmerc +lat_0=0 +lon_0=15 +k=1 +x_0=800000 +y_0=-5000000 +ellps=GRS80 +units=m +no_defs"), // ETRS_1989_DKTM4 (deprecated)
    (103220, "+proj=tmerc +lat_0=30.5 +lon_0=-85.8333333333333 +k=0.99996 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alabama_East_FIPS_0101
    (103221, "+proj=tmerc +lat_0=30 +lon_0=-87.5 +k=0.999933333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Alabama_West_FIPS_0102
    (103222, "+proj=tmerc +lat_0=31 +lon_0=-110.166666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Arizona_East_FIPS_0201
    (103223, "+proj=tmerc +lat_0=31 +lon_0=-111.916666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Arizona_Central_FIPS_0202
    (103224, "+proj=tmerc +lat_0=31 +lon_0=-113.75 +k=0.999933333333333 +x_0=213360 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Arizona_West_FIPS_0203
    (103225, "+proj=tmerc +lat_0=31 +lon_0=-110.166666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Arizona_East_FIPS_0201_Ft_Intl
    (103226, "+proj=tmerc +lat_0=31 +lon_0=-111.916666666667 +k=0.9999 +x_0=213360 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Arizona_Central_FIPS_0202_Ft_Intl
    (103227, "+proj=tmerc +lat_0=31 +lon_0=-113.75 +k=0.999933333333333 +x_0=213360 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Arizona_West_FIPS_0203_Ft_Intl
    (103228, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-92 +lat_1=34.9333333333333 +lat_2=36.2333333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Arkansas_North_FIPS_0301
    (103229, "+proj=lcc +lat_0=32.6666666666667 +lon_0=-92 +lat_1=33.3 +lat_2=34.7666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Arkansas_South_FIPS_0302
    (103230, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-92 +lat_1=34.9333333333333 +lat_2=36.2333333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Arkansas_North_FIPS_0301_Ft_US
    (103231, "+proj=lcc +lat_0=32.6666666666667 +lon_0=-92 +lat_1=33.3 +lat_2=34.7666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Arkansas_South_FIPS_0302_Ft_US
    (103232, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-122 +lat_1=40 +lat_2=41.6666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_California_I_FIPS_0401
    (103233, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-122 +lat_1=38.3333333333333 +lat_2=39.8333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_California_II_FIPS_0402
    (103234, "+proj=lcc +lat_0=36.5 +lon_0=-120.5 +lat_1=37.0666666666667 +lat_2=38.4333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_California_III_FIPS_0403
    (103235, "+proj=lcc +lat_0=35.3333333333333 +lon_0=-119 +lat_1=36 +lat_2=37.25 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_California_IV_FIPS_0404
    (103236, "+proj=lcc +lat_0=33.5 +lon_0=-118 +lat_1=34.0333333333333 +lat_2=35.4666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_California_V_FIPS_0405
    (103237, "+proj=lcc +lat_0=32.1666666666667 +lon_0=-116.25 +lat_1=32.7833333333333 +lat_2=33.8833333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_California_VI_FIPS_0406
    (103238, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-122 +lat_1=40 +lat_2=41.6666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_California_I_FIPS_0401_Ft_US
    (103239, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-122 +lat_1=38.3333333333333 +lat_2=39.8333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_California_II_FIPS_0402_Ft_US
    (103240, "+proj=lcc +lat_0=36.5 +lon_0=-120.5 +lat_1=37.0666666666667 +lat_2=38.4333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_California_III_FIPS_0403_Ft_US
    (103241, "+proj=lcc +lat_0=35.3333333333333 +lon_0=-119 +lat_1=36 +lat_2=37.25 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_California_IV_FIPS_0404_Ft_US
    (103242, "+proj=lcc +lat_0=33.5 +lon_0=-118 +lat_1=34.0333333333333 +lat_2=35.4666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_California_V_FIPS_0405_Ft_US
    (103243, "+proj=lcc +lat_0=32.1666666666667 +lon_0=-116.25 +lat_1=32.7833333333333 +lat_2=33.8833333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_California_VI_FIPS_0406_Ft_US
    (103244, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-105.5 +lat_1=39.7166666666667 +lat_2=40.7833333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Colorado_North_FIPS_0501
    (103245, "+proj=lcc +lat_0=37.8333333333333 +lon_0=-105.5 +lat_1=38.45 +lat_2=39.75 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Colorado_Central_FIPS_0502
    (103246, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-105.5 +lat_1=37.2333333333333 +lat_2=38.4333333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Colorado_South_FIPS_0503
    (103247, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-105.5 +lat_1=39.7166666666667 +lat_2=40.7833333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Colorado_North_FIPS_0501_Ft_US
    (103248, "+proj=lcc +lat_0=37.8333333333333 +lon_0=-105.5 +lat_1=38.45 +lat_2=39.75 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Colorado_Central_FIPS_0502_Ft_US
    (103249, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-105.5 +lat_1=37.2333333333333 +lat_2=38.4333333333333 +x_0=914401.8289 +y_0=304800.6096 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Colorado_South_FIPS_0503_Ft_US
    (103250, "+proj=lcc +lat_0=40.8333333333333 +lon_0=-72.75 +lat_1=41.2 +lat_2=41.8666666666667 +x_0=304800.6096 +y_0=152400.3048 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Connecticut_FIPS_0600
    (103251, "+proj=lcc +lat_0=40.8333333333333 +lon_0=-72.75 +lat_1=41.2 +lat_2=41.8666666666667 +x_0=304800.6096 +y_0=152400.3048 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Connecticut_FIPS_0600_Ft_US
    (103252, "+proj=tmerc +lat_0=38 +lon_0=-75.4166666666667 +k=0.999995 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Delaware_FIPS_0700
    (103253, "+proj=tmerc +lat_0=38 +lon_0=-75.4166666666667 +k=0.999995 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Delaware_FIPS_0700_Ft_US
    (103254, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-81 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Florida_East_FIPS_0901
    (103255, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-82 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Florida_West_FIPS_0902
    (103256, "+proj=lcc +lat_0=29 +lon_0=-84.5 +lat_1=29.5833333333333 +lat_2=30.75 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Florida_North_FIPS_0903
    (103257, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-81 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Florida_East_FIPS_0901_Ft_US
    (103258, "+proj=tmerc +lat_0=24.3333333333333 +lon_0=-82 +k=0.999941176470588 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Florida_West_FIPS_0902_Ft_US
    (103259, "+proj=lcc +lat_0=29 +lon_0=-84.5 +lat_1=29.5833333333333 +lat_2=30.75 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Florida_North_FIPS_0903_Ft_US
    (103260, "+proj=tmerc +lat_0=30 +lon_0=-82.1666666666667 +k=0.9999 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Georgia_East_FIPS_1001
    (103261, "+proj=tmerc +lat_0=30 +lon_0=-84.1666666666667 +k=0.9999 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Georgia_West_FIPS_1002
    (103262, "+proj=tmerc +lat_0=30 +lon_0=-82.1666666666667 +k=0.9999 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Georgia_East_FIPS_1001_Ft_US
    (103263, "+proj=tmerc +lat_0=30 +lon_0=-84.1666666666667 +k=0.9999 +x_0=700000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Georgia_West_FIPS_1002_Ft_US
    (103264, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-112.166666666667 +k=0.999947368421053 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Idaho_East_FIPS_1101
    (103265, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-114 +k=0.999947368421053 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Idaho_Central_FIPS_1102
    (103266, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-115.75 +k=0.999933333333333 +x_0=800000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Idaho_West_FIPS_1103
    (103267, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-112.166666666667 +k=0.999947368421053 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Idaho_East_FIPS_1101_Ft_US
    (103268, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-114 +k=0.999947368421053 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Idaho_Central_FIPS_1102_Ft_US
    (103269, "+proj=tmerc +lat_0=41.6666666666667 +lon_0=-115.75 +k=0.999933333333333 +x_0=800000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Idaho_West_FIPS_1103_Ft_US
    (103270, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-88.3333333333333 +k=0.999975 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Illinois_East_FIPS_1201
    (103271, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-90.1666666666667 +k=0.999941176470588 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Illinois_West_FIPS_1202
    (103272, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-88.3333333333333 +k=0.999975 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Illinois_East_FIPS_1201_Ft_US
    (103273, "+proj=tmerc +lat_0=36.6666666666667 +lon_0=-90.1666666666667 +k=0.999941176470588 +x_0=700000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Illinois_West_FIPS_1202_Ft_US
    (103274, "+proj=tmerc +lat_0=37.5 +lon_0=-85.6666666666667 +k=0.999966666666667 +x_0=100000 +y_0=250000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Indiana_East_FIPS_1301
    (103275, "+proj=tmerc +lat_0=37.5 +lon_0=-87.0833333333333 +k=0.999966666666667 +x_0=900000 +y_0=250000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Indiana_West_FIPS_1302
    (103276, "+proj=tmerc +lat_0=37.5 +lon_0=-85.6666666666667 +k=0.999966666666667 +x_0=100000 +y_0=250000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Indiana_East_FIPS_1301_Ft_US
    (103277, "+proj=tmerc +lat_0=37.5 +lon_0=-87.0833333333333 +k=0.999966666666667 +x_0=900000 +y_0=250000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Indiana_West_FIPS_1302_Ft_US
    (103278, "+proj=lcc +lat_0=41.5 +lon_0=-93.5 +lat_1=42.0666666666667 +lat_2=43.2666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Iowa_North_FIPS_1401
    (103279, "+proj=lcc +lat_0=40 +lon_0=-93.5 +lat_1=40.6166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Iowa_South_FIPS_1402
    (103280, "+proj=lcc +lat_0=41.5 +lon_0=-93.5 +lat_1=42.0666666666667 +lat_2=43.2666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Iowa_North_FIPS_1401_Ft_US
    (103281, "+proj=lcc +lat_0=40 +lon_0=-93.5 +lat_1=40.6166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Iowa_South_FIPS_1402_Ft_US
    (103282, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-98 +lat_1=38.7166666666667 +lat_2=39.7833333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Kansas_North_FIPS_1501
    (103283, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-98.5 +lat_1=37.2666666666667 +lat_2=38.5666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Kansas_South_FIPS_1502
    (103284, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-98 +lat_1=38.7166666666667 +lat_2=39.7833333333333 +x_0=400000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Kansas_North_FIPS_1501_Ft_US
    (103285, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-98.5 +lat_1=37.2666666666667 +lat_2=38.5666666666667 +x_0=400000 +y_0=400000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Kansas_South_FIPS_1502_Ft_US
    (103286, "+proj=lcc +lat_0=37.5 +lon_0=-84.25 +lat_1=37.9666666666667 +lat_2=38.9666666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Kentucky_North_FIPS_1601
    (103287, "+proj=lcc +lat_0=37.5 +lon_0=-84.25 +lat_1=37.9666666666667 +lat_2=38.9666666666667 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Kentucky_North_FIPS_1601_Ft_US
    (103288, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=37.0833333333333 +lat_2=38.6666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Kentucky_FIPS_1600
    (103289, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=37.0833333333333 +lat_2=38.6666666666667 +x_0=1500000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Kentucky_FIPS_1600_Ft_US
    (103290, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=36.7333333333333 +lat_2=37.9333333333333 +x_0=500000 +y_0=500000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Kentucky_South_FIPS_1602
    (103291, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-85.75 +lat_1=36.7333333333333 +lat_2=37.9333333333333 +x_0=500000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Kentucky_South_FIPS_1602_Ft_US
    (103292, "+proj=lcc +lat_0=30.5 +lon_0=-92.5 +lat_1=31.1666666666667 +lat_2=32.6666666666667 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Louisiana_North_FIPS_1701
    (103293, "+proj=lcc +lat_0=28.5 +lon_0=-91.3333333333333 +lat_1=29.3 +lat_2=30.7 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Louisiana_South_FIPS_1702
    (103294, "+proj=lcc +lat_0=30.5 +lon_0=-92.5 +lat_1=31.1666666666667 +lat_2=32.6666666666667 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Louisiana_North_FIPS_1701_Ft_US
    (103295, "+proj=lcc +lat_0=28.5 +lon_0=-91.3333333333333 +lat_1=29.3 +lat_2=30.7 +x_0=1000000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Louisiana_South_FIPS_1702_Ft_US
    (103296, "+proj=tmerc +lat_0=43.6666666666667 +lon_0=-68.5 +k=0.9999 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Maine_East_FIPS_1801
    (103297, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.1666666666667 +k=0.999966666666667 +x_0=900000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Maine_West_FIPS_1802
    (103298, "+proj=tmerc +lat_0=43.6666666666667 +lon_0=-68.5 +k=0.9999 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Maine_East_FIPS_1801_Ft_US
    (103299, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.1666666666667 +k=0.999966666666667 +x_0=900000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Maine_West_FIPS_1802_Ft_US
    (103300, "+proj=tmerc +lat_0=43.3666666666667 +lon_0=-90 +k=1.0000365285 +x_0=147218.6942 +y_0=0.0037 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Adams_County_Meters
    (103301, "+proj=tmerc +lat_0=45.7061111111111 +lon_0=-90.6222222222222 +k=1.0000495683 +x_0=172821.9461 +y_0=0.0017 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Ashland_County_Meters (deprecated)
    (103302, "+proj=tmerc +lat_0=45.1333333333333 +lon_0=-91.85 +k=1.0000486665 +x_0=93150 +y_0=0.0029 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Barron_County_Meters (deprecated)
    (103303, "+proj=lcc +lat_1=46.6696483772222 +lat_0=46.6696483772222 +lon_0=-91.1527777777778 +k_0=1.0000331195 +x_0=228600.4575 +y_0=148551.4837 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Bayfield_County_Meters (deprecated)
    (103304, "+proj=tmerc +lat_0=43 +lon_0=-88 +k=1.00002 +x_0=31600 +y_0=4600 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Brown_County_Meters (deprecated)
    (103305, "+proj=tmerc +lat_0=43.4813888888889 +lon_0=-91.7972222222222 +k=1.0000382778 +x_0=175260.3502 +y_0=0.0048 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Buffalo_County_Meters (deprecated)
    (103306, "+proj=lcc +lat_1=45.8987148658333 +lat_0=45.8987148658333 +lon_0=-92.4577777777778 +k_0=1.0000383841 +x_0=64008.1276 +y_0=59445.9043 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Burnett_County_Meters (deprecated)
    (103307, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=1.0000286569 +x_0=244754.8893 +y_0=0.0049 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Calumet_County_Meters
    (103308, "+proj=lcc +lat_1=44.9778568986111 +lat_0=44.9778568986111 +lon_0=-91.2944444444444 +k_0=1.0000391127 +x_0=60045.72 +y_0=44091.4346 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Chippewa_County_Meters (deprecated)
    (103309, "+proj=tmerc +lat_0=43.6 +lon_0=-90.7083333333333 +k=1.0000463003 +x_0=199949.1989 +y_0=0.0086 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Clark_County_Meters (deprecated)
    (103310, "+proj=lcc +lat_1=43.4625466458333 +lat_0=43.4625466458333 +lon_0=-89.3944444444444 +k_0=1.00003498 +x_0=169164.3381 +y_0=111569.6134 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Columbia_County_Meters (deprecated)
    (103311, "+proj=lcc +lat_1=43.200055605 +lat_0=43.200055605 +lon_0=-90.9388888888889 +k_0=1.0000349151 +x_0=113690.6274 +y_0=53703.1201 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Crawford_County_Meters (deprecated)
    (103312, "+proj=lcc +lat_1=43.0695160375 +lat_0=43.0695160375 +lon_0=-89.4222222222222 +k_0=1.0000384786 +x_0=247193.2944 +y_0=146591.9896 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Dane_County_Meters (deprecated)
    (103313, "+proj=tmerc +lat_0=41.4722222222222 +lon_0=-88.775 +k=1.0000346418 +x_0=263347.7263 +y_0=0.0076 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Dodge_County_Meters
    (103314, "+proj=tmerc +lat_0=44.4 +lon_0=-87.2722222222222 +k=1.0000187521 +x_0=158801.1176 +y_0=0.0023 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Door_County_Meters (deprecated)
    (103315, "+proj=tmerc +lat_0=45.8833333333333 +lon_0=-91.9166666666667 +k=1.0000385418 +x_0=59131.3183 +y_0=0.0041 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Douglas_County_Meters (deprecated)
    (103316, "+proj=tmerc +lat_0=44.4083333333333 +lon_0=-91.8944444444444 +k=1.0000410324 +x_0=51816.104 +y_0=0.003 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Dunn_County_Meters (deprecated)
    (103317, "+proj=lcc +lat_1=44.8722811263889 +lat_0=44.8722811263889 +lon_0=-91.2888888888889 +k_0=1.000035079 +x_0=120091.4402 +y_0=91687.9239 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_EauClaire_County_Meters (deprecated)
    (103318, "+proj=tmerc +lat_0=45.4388888888889 +lon_0=-88.1416666666667 +k=1.0000552095 +x_0=133502.6683 +y_0=0.0063 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Florence_County_Meters (deprecated)
    (103319, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=1.0000286569 +x_0=244754.8893 +y_0=0.0049 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Fond_du_Lac_County_Meters
    (103320, "+proj=tmerc +lat_0=44.0055555555556 +lon_0=-88.6333333333333 +k=1.0000673004 +x_0=275844.5533 +y_0=0.0157 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Forest_County_Meters (deprecated)
    (103321, "+proj=tmerc +lat_0=41.4111111111111 +lon_0=-90.8 +k=1.0000349452 +x_0=242316.4841 +y_0=0.01 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Grant_County_Meters (deprecated)
    (103322, "+proj=lcc +lat_1=42.6375622769444 +lat_0=42.6375622769444 +lon_0=-89.8388888888889 +k_0=1.0000390487 +x_0=170078.7403 +y_0=45830.2947 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Green_County_Meters
    (103323, "+proj=lcc +lat_1=43.8070001177778 +lat_0=43.8070001177778 +lon_0=-89.2416666666667 +k_0=1.0000344057 +x_0=150876.3018 +y_0=79170.7795 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_GreenLake_County_Meters
    (103324, "+proj=tmerc +lat_0=42.5388888888889 +lon_0=-90.1611111111111 +k=1.0000394961 +x_0=113081.0261 +y_0=0.0045 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Iowa_County_Meters (deprecated)
    (103325, "+proj=tmerc +lat_0=45.4333333333333 +lon_0=-90.2555555555556 +k=1.0000677153 +x_0=220980.4419 +y_0=0.0085 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Iron_County_Meters (deprecated)
    (103326, "+proj=tmerc +lat_0=44.2533351277778 +lon_0=-90.8442965194444 +k=1.0000353 +x_0=27000 +y_0=25000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Jackson_County_Meters (deprecated)
    (103327, "+proj=tmerc +lat_0=41.4722222222222 +lon_0=-88.775 +k=1.0000346418 +x_0=263347.7263 +y_0=0.0076 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Jefferson_County_Meters
    (103328, "+proj=tmerc +lat_0=43.3666666666667 +lon_0=-90 +k=1.0000365285 +x_0=147218.6942 +y_0=0.0037 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Juneau_County_Meters
    (103329, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=1.0000260649 +x_0=185928.3728 +y_0=0.0009 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Kenosha_County_Meters
    (103330, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1.0000233704 +x_0=79857.7614 +y_0=0.0012 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Kewaunee_County_Meters
    (103331, "+proj=tmerc +lat_0=43.4511111111111 +lon_0=-91.3166666666667 +k=1.0000319985 +x_0=130454.6598 +y_0=0.0033 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_LaCrosse_County_Meters (deprecated)
    (103332, "+proj=lcc +lat_1=42.6375622769444 +lat_0=42.6375622769444 +lon_0=-89.8388888888889 +k_0=1.0000390487 +x_0=170078.7403 +y_0=45830.2947 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Lafayette_County_Meters
    (103333, "+proj=lcc +lat_1=45.1542371052778 +lat_0=45.1542371052778 +lon_0=-89.0333333333333 +k_0=1.0000627024 +x_0=198425.197 +y_0=105279.7829 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Langlade_County_Meters (deprecated)
    (103334, "+proj=tmerc +lat_0=44.8444444444444 +lon_0=-89.7333333333333 +k=1.0000599003 +x_0=116129.0323 +y_0=0.0058 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Lincoln_County_Meters (deprecated)
    (103335, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1.0000233704 +x_0=79857.7614 +y_0=0.0012 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Manitowoc_County_Meters
    (103336, "+proj=lcc +lat_1=44.9009044236111 +lat_0=44.9009044236111 +lon_0=-89.77 +k_0=1.000053289 +x_0=74676.1493 +y_0=55049.2669 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Marathon_County_Meters (deprecated)
    (103337, "+proj=tmerc +lat_0=44.6916666666667 +lon_0=-87.7111111111111 +k=1.0000234982 +x_0=238658.8794 +y_0=0.0032 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Marinette_County_Meters (deprecated)
    (103338, "+proj=lcc +lat_1=43.8070001177778 +lat_0=43.8070001177778 +lon_0=-89.2416666666667 +k_0=1.0000344057 +x_0=150876.3018 +y_0=79170.7795 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Marquette_County_Meters
    (103339, "+proj=tmerc +lat_0=44.7166666666667 +lon_0=-88.4166666666667 +k=1.0000362499 +x_0=105461.0121 +y_0=0.0029 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Menominee_County_Meters (deprecated)
    (103340, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=1.0000260649 +x_0=185928.3728 +y_0=0.0009 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Milwaukee_County_Meters
    (103341, "+proj=lcc +lat_1=44.0000739286111 +lat_0=44.0000739286111 +lon_0=-90.6416666666667 +k_0=1.0000434122 +x_0=204521.209 +y_0=121923.9861 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Monroe_County_Meters (deprecated)
    (103342, "+proj=tmerc +lat_0=44.3972222222222 +lon_0=-87.9083333333333 +k=1.0000236869 +x_0=182880.3676 +y_0=0.0033 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Oconto_County_Meters (deprecated)
    (103343, "+proj=lcc +lat_1=45.7042237702778 +lat_0=45.7042237702778 +lon_0=-89.5444444444444 +k_0=1.0000686968 +x_0=70104.1401 +y_0=57588.0346 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Oneida_County_Meters (deprecated)
    (103344, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=1.0000286569 +x_0=244754.8893 +y_0=0.0049 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Outagamie_County_Meters
    (103345, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=1.0000260649 +x_0=185928.3728 +y_0=0.0009 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Ozaukee_County_Meters
    (103346, "+proj=lcc +lat_1=44.6361488719444 +lat_0=44.6361488719444 +lon_0=-92.2277777777778 +k_0=1.0000362977 +x_0=167640.3354 +y_0=86033.0876 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Pepin_County_Meters
    (103347, "+proj=lcc +lat_1=44.6361488719444 +lat_0=44.6361488719444 +lon_0=-92.2277777777778 +k_0=1.0000362977 +x_0=167640.3354 +y_0=86033.0876 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Pierce_County_Meters
    (103348, "+proj=tmerc +lat_0=44.6611111111111 +lon_0=-92.6333333333333 +k=1.0000433849 +x_0=141732.2823 +y_0=0.0059 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Polk_County_Meters (deprecated)
    (103349, "+proj=lcc +lat_1=44.4168239752778 +lat_0=44.4168239752778 +lon_0=-89.5 +k_0=1.000039936 +x_0=56388.1128 +y_0=50022.1874 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Portage_County_Meters (deprecated)
    (103350, "+proj=tmerc +lat_0=44.5555555555556 +lon_0=-90.4888888888889 +k=1.0000649554 +x_0=227990.8546 +y_0=0.0109 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Price_County_Meters (deprecated)
    (103351, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=1.0000260649 +x_0=185928.3728 +y_0=0.0009 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Racine_County_Meters
    (103352, "+proj=lcc +lat_1=43.3223129275 +lat_0=43.3223129275 +lon_0=-90.4305555555556 +k_0=1.0000375653 +x_0=202387.6048 +y_0=134255.4253 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Richland_County_Meters (deprecated)
    (103353, "+proj=tmerc +lat_0=41.9444444444444 +lon_0=-89.0722222222222 +k=1.0000337311 +x_0=146304.2926 +y_0=0.0068 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Rock_County_Meters (deprecated)
    (103354, "+proj=tmerc +lat_0=43.9194444444444 +lon_0=-91.0666666666667 +k=1.0000495976 +x_0=250546.1013 +y_0=0.0234 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Rusk_County_Meters (deprecated)
    (103355, "+proj=tmerc +lat_0=42.8194444444444 +lon_0=-89.9 +k=1.0000373868 +x_0=185623.5716 +y_0=0.0051 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Sauk_County_Meters (deprecated)
    (103356, "+proj=lcc +lat_1=45.9000991313889 +lat_0=45.9000991313889 +lon_0=-91.1166666666667 +k_0=1.0000573461 +x_0=216713.2336 +y_0=120734.1631 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Sawyer_County_Meters (deprecated)
    (103357, "+proj=tmerc +lat_0=44.0361111111111 +lon_0=-88.6055555555556 +k=1.000032144 +x_0=262433.3253 +y_0=0.0096 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Shawano_County_Meters (deprecated)
    (103358, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1.0000233704 +x_0=79857.7614 +y_0=0.0012 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Sheboygan_County_Meters
    (103359, "+proj=tmerc +lat_0=44.0361111111111 +lon_0=-92.6333333333333 +k=1.0000381803 +x_0=165506.7302 +y_0=0.0103 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_St_Croix_County_Meters (deprecated)
    (103360, "+proj=lcc +lat_1=45.1778220858333 +lat_0=45.1778220858333 +lon_0=-90.4833333333333 +k_0=1.0000597566 +x_0=187147.5744 +y_0=107746.7522 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Taylor_County_Meters (deprecated)
    (103361, "+proj=tmerc +lat_0=43.1611111111111 +lon_0=-91.3666666666667 +k=1.0000361538 +x_0=256946.9138 +y_0=0.0041 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Trempealeau_County_Meters (deprecated)
    (103362, "+proj=lcc +lat_1=43.5750329397222 +lat_0=43.5750329397222 +lon_0=-90.7833333333333 +k_0=1.0000408158 +x_0=222504.4451 +y_0=47532.0602 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Vernon_County_Meters (deprecated)
    (103363, "+proj=lcc +lat_1=46.0778440905556 +lat_0=46.0778440905556 +lon_0=-89.4888888888889 +k_0=1.0000730142 +x_0=134417.0689 +y_0=50337.1092 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Vilas_County_Meters (deprecated)
    (103364, "+proj=lcc +lat_1=42.6694620969444 +lat_0=42.6694620969444 +lon_0=-88.5416666666667 +k_0=1.0000367192 +x_0=232562.8651 +y_0=111088.2224 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Walworth_County_Meters (deprecated)
    (103365, "+proj=lcc +lat_1=45.9612198333333 +lat_0=45.9612198333333 +lon_0=-91.7833333333333 +k_0=1.0000475376 +x_0=234086.8682 +y_0=188358.6058 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Washburn_County_Meters (deprecated)
    (103366, "+proj=tmerc +lat_0=42.9180555555555 +lon_0=-88.0638888888889 +k=1.00003738 +x_0=120091.4415 +y_0=0.003 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Washington_County_Meters (deprecated)
    (103367, "+proj=tmerc +lat_0=42.5694444444444 +lon_0=-88.225 +k=1.0000346179 +x_0=208788.418 +y_0=0.0034 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Waukesha_County_Meters (deprecated)
    (103368, "+proj=tmerc +lat_0=43.4202777777778 +lon_0=-88.8166666666667 +k=1.0000333645 +x_0=185013.9709 +y_0=0.007 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Waupaca_County_Meters (deprecated)
    (103369, "+proj=lcc +lat_1=44.1139440458333 +lat_0=44.1139440458333 +lon_0=-89.2416666666667 +k_0=1.0000392096 +x_0=120091.4402 +y_0=45069.7587 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Waushara_County_Meters (deprecated)
    (103370, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=1.0000286569 +x_0=244754.8893 +y_0=0.0049 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Winnebago_County_Meters
    (103371, "+proj=lcc +lat_1=44.3625954694444 +lat_0=44.3625954694444 +lon_0=-90 +k_0=1.0000421209 +x_0=208483.6173 +y_0=134589.754 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_WISCRS_Wood_County_Meters (deprecated)
    (103372, "+proj=tmerc +lat_0=43.8333333333333 +lon_0=-67.875 +k=0.99998 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_Maine_2000_East_Zone
    (103373, "+proj=tmerc +lat_0=43.5 +lon_0=-69.125 +k=0.99998 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_Maine_2000_Central_Zone
    (103374, "+proj=tmerc +lat_0=42.8333333333333 +lon_0=-70.375 +k=0.99998 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_Maine_2000_West_Zone
    (103375, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-77 +lat_1=38.3 +lat_2=39.45 +x_0=400000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Maryland_FIPS_1900
    (103376, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-77 +lat_1=38.3 +lat_2=39.45 +x_0=400000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Maryland_FIPS_1900_Ft_US
    (103377, "+proj=lcc +lat_0=41 +lon_0=-71.5 +lat_1=41.7166666666667 +lat_2=42.6833333333333 +x_0=200000 +y_0=750000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Massachusetts_Mainland_FIPS_2001
    (103378, "+proj=lcc +lat_0=41 +lon_0=-70.5 +lat_1=41.2833333333333 +lat_2=41.4833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Massachusetts_Island_FIPS_2002
    (103379, "+proj=lcc +lat_0=41 +lon_0=-71.5 +lat_1=41.7166666666667 +lat_2=42.6833333333333 +x_0=200000 +y_0=750000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Massachusetts_Mnld_FIPS_2001_FtUS
    (103380, "+proj=lcc +lat_0=41 +lon_0=-70.5 +lat_1=41.2833333333333 +lat_2=41.4833333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Massachusetts_Isl_FIPS_2002_FtUS
    (103381, "+proj=lcc +lat_0=44.7833333333333 +lon_0=-87 +lat_1=45.4833333333333 +lat_2=47.0833333333333 +x_0=8000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Michigan_North_FIPS_2111
    (103382, "+proj=lcc +lat_0=43.3166666666667 +lon_0=-84.3666666666667 +lat_1=44.1833333333333 +lat_2=45.7 +x_0=6000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Michigan_Central_FIPS_2112
    (103383, "+proj=lcc +lat_0=41.5 +lon_0=-84.3666666666667 +lat_1=42.1 +lat_2=43.6666666666667 +x_0=4000000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Michigan_South_FIPS_2113
    (103384, "+proj=lcc +lat_0=44.7833333333333 +lon_0=-87 +lat_1=45.4833333333333 +lat_2=47.0833333333333 +x_0=8000000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Michigan_North_FIPS_2111_Ft_Intl
    (103385, "+proj=lcc +lat_0=43.3166666666667 +lon_0=-84.3666666666667 +lat_1=44.1833333333333 +lat_2=45.7 +x_0=6000000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Michigan_Central_FIPS_2112_Ft_Intl
    (103386, "+proj=lcc +lat_0=41.5 +lon_0=-84.3666666666667 +lat_1=42.1 +lat_2=43.6666666666667 +x_0=4000000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Michigan_South_FIPS_2113_Ft_Intl
    (103387, "+proj=lcc +lat_0=46.5 +lon_0=-93.1 +lat_1=47.0333333333333 +lat_2=48.6333333333333 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Minnesota_North_FIPS_2201
    (103388, "+proj=lcc +lat_0=45 +lon_0=-94.25 +lat_1=45.6166666666667 +lat_2=47.05 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Minnesota_Central_FIPS_2202
    (103389, "+proj=lcc +lat_0=43 +lon_0=-94 +lat_1=43.7833333333333 +lat_2=45.2166666666667 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Minnesota_South_FIPS_2203
    (103390, "+proj=lcc +lat_0=46.5 +lon_0=-93.1 +lat_1=47.0333333333333 +lat_2=48.6333333333333 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Minnesota_North_FIPS_2201_Ft_US
    (103391, "+proj=lcc +lat_0=45 +lon_0=-94.25 +lat_1=45.6166666666667 +lat_2=47.05 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Minnesota_Central_FIPS_2202_Ft_US
    (103392, "+proj=lcc +lat_0=43 +lon_0=-94 +lat_1=43.7833333333333 +lat_2=45.2166666666667 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Minnesota_South_FIPS_2203_Ft_US
    (103393, "+proj=tmerc +lat_0=29.5 +lon_0=-88.8333333333333 +k=0.99995 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Mississippi_East_FIPS_2301
    (103394, "+proj=tmerc +lat_0=29.5 +lon_0=-90.3333333333333 +k=0.99995 +x_0=700000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Mississippi_West_FIPS_2302
    (103395, "+proj=tmerc +lat_0=29.5 +lon_0=-88.8333333333333 +k=0.99995 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Mississippi_East_FIPS_2301_Ft_US
    (103396, "+proj=tmerc +lat_0=29.5 +lon_0=-90.3333333333333 +k=0.99995 +x_0=700000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Mississippi_West_FIPS_2302_Ft_US
    (103397, "+proj=tmerc +lat_0=35.8333333333333 +lon_0=-90.5 +k=0.999933333333333 +x_0=250000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Missouri_East_FIPS_2401
    (103398, "+proj=tmerc +lat_0=35.8333333333333 +lon_0=-92.5 +k=0.999933333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Missouri_Central_FIPS_2402
    (103399, "+proj=tmerc +lat_0=36.1666666666667 +lon_0=-94.5 +k=0.999941176470588 +x_0=850000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Missouri_West_FIPS_2403
    (103400, "+proj=tmerc +lat_0=43.3666666666667 +lon_0=-90 +k=1.0000365285 +x_0=147218.694132588 +y_0=0.00365760731521463 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Adams_County_Feet
    (103401, "+proj=tmerc +lat_0=45.7061111111111 +lon_0=-90.6222222222222 +k=1.0000495683 +x_0=172821.945948692 +y_0=0.00182880365760732 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Ashland_County_Feet (deprecated)
    (103402, "+proj=tmerc +lat_0=45.1333333333333 +lon_0=-91.85 +k=1.0000486665 +x_0=93150 +y_0=0.00304800609601219 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Barron_County_Feet (deprecated)
    (103403, "+proj=lcc +lat_1=46.6696483772222 +lat_0=46.6696483772222 +lon_0=-91.1527777777778 +k_0=1.0000331195 +x_0=228600.457505715 +y_0=148551.483566167 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Bayfield_County_Feet (deprecated)
    (103404, "+proj=tmerc +lat_0=43 +lon_0=-88 +k=1.00002 +x_0=31599.9998983998 +y_0=4599.9998983998 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Brown_County_Feet (deprecated)
    (103405, "+proj=tmerc +lat_0=43.4813888888889 +lon_0=-91.7972222222222 +k=1.0000382778 +x_0=175260.3502159 +y_0=0.00487680975361951 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Buffalo_County_Feet (deprecated)
    (103406, "+proj=lcc +lat_1=45.8987148658333 +lat_0=45.8987148658333 +lon_0=-92.4577777777778 +k_0=1.0000383841 +x_0=64008.1277114554 +y_0=59445.9041910084 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Burnett_County_Feet (deprecated)
    (103407, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=1.0000286569 +x_0=244754.889204978 +y_0=0.00487680975361951 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Calumet_County_Feet
    (103408, "+proj=lcc +lat_1=44.9778568986111 +lat_0=44.9778568986111 +lon_0=-91.2944444444444 +k_0=1.0000391127 +x_0=60045.7200914402 +y_0=44091.434493269 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Chippewa_County_Feet (deprecated)
    (103409, "+proj=tmerc +lat_0=43.6 +lon_0=-90.7083333333333 +k=1.0000463003 +x_0=199949.198983998 +y_0=0.00853441706883414 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Clark_County_Feet (deprecated)
    (103410, "+proj=lcc +lat_1=43.4625466458333 +lat_0=43.4625466458333 +lon_0=-89.3944444444444 +k_0=1.00003498 +x_0=169164.338023876 +y_0=111569.613512827 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Columbia_County_Feet (deprecated)
    (103411, "+proj=lcc +lat_1=43.200055605 +lat_0=43.200055605 +lon_0=-90.9388888888889 +k_0=1.0000349151 +x_0=113690.627381255 +y_0=53703.1202438405 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Crawford_County_Feet (deprecated)
    (103412, "+proj=lcc +lat_1=43.0695160375 +lat_0=43.0695160375 +lon_0=-89.4222222222222 +k_0=1.0000384786 +x_0=247193.294386589 +y_0=146591.989636779 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Dane_County_Feet (deprecated)
    (103413, "+proj=tmerc +lat_0=41.4722222222222 +lon_0=-88.775 +k=1.0000346418 +x_0=263347.726390653 +y_0=0.00762001524003048 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Dodge_County_Feet
    (103414, "+proj=tmerc +lat_0=44.4 +lon_0=-87.2722222222222 +k=1.0000187521 +x_0=158801.117602235 +y_0=0.00243840487680975 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Door_County_Feet (deprecated)
    (103415, "+proj=tmerc +lat_0=45.8833333333333 +lon_0=-91.9166666666667 +k=1.0000385418 +x_0=59131.3182626365 +y_0=0.00396240792481585 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Douglas_County_Feet (deprecated)
    (103416, "+proj=tmerc +lat_0=44.4083333333333 +lon_0=-91.8944444444444 +k=1.0000410324 +x_0=51816.1039370079 +y_0=0.00304800609601219 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Dunn_County_Feet (deprecated)
    (103417, "+proj=lcc +lat_1=44.8722811263889 +lat_0=44.8722811263889 +lon_0=-91.2888888888889 +k_0=1.000035079 +x_0=120091.44018288 +y_0=91687.9239014478 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_EauClaire_County_Feet (deprecated)
    (103418, "+proj=tmerc +lat_0=45.4388888888889 +lon_0=-88.1416666666667 +k=1.0000552095 +x_0=133502.668224536 +y_0=0.0064008128016256 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Florence_County_Feet (deprecated)
    (103419, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=1.0000286569 +x_0=244754.889204978 +y_0=0.00487680975361951 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Fond_du_Lac_County_Feet
    (103420, "+proj=tmerc +lat_0=44.0055555555556 +lon_0=-88.6333333333333 +k=1.0000673004 +x_0=275844.553213106 +y_0=0.0158496316992634 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Forest_County_Feet (deprecated)
    (103421, "+proj=tmerc +lat_0=41.4111111111111 +lon_0=-90.8 +k=1.0000349452 +x_0=242316.484023368 +y_0=0.0100584201168402 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Grant_County_Feet (deprecated)
    (103422, "+proj=lcc +lat_1=42.6375622769444 +lat_0=42.6375622769444 +lon_0=-89.8388888888889 +k_0=1.0000390487 +x_0=170078.74015748 +y_0=45830.2948437897 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Green_County_Feet
    (103423, "+proj=lcc +lat_1=43.8070001177778 +lat_0=43.8070001177778 +lon_0=-89.2416666666667 +k_0=1.0000344057 +x_0=150876.301752604 +y_0=79170.7793751588 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_GreenLake_County_Feet
    (103424, "+proj=tmerc +lat_0=42.5388888888889 +lon_0=-90.1611111111111 +k=1.0000394961 +x_0=113081.026162052 +y_0=0.00457200914401829 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Iowa_County_Feet (deprecated)
    (103425, "+proj=tmerc +lat_0=45.4333333333333 +lon_0=-90.2555555555556 +k=1.0000677153 +x_0=220980.441960884 +y_0=0.00853441706883414 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Iron_County_Feet (deprecated)
    (103426, "+proj=tmerc +lat_0=44.2533351277778 +lon_0=-90.8442965194444 +k=1.0000353 +x_0=27000 +y_0=24999.9998983998 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Jackson_County_Feet (deprecated)
    (103427, "+proj=tmerc +lat_0=41.4722222222222 +lon_0=-88.775 +k=1.0000346418 +x_0=263347.726390653 +y_0=0.00762001524003048 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Jefferson_County_Feet
    (103428, "+proj=tmerc +lat_0=43.3666666666667 +lon_0=-90 +k=1.0000365285 +x_0=147218.694132588 +y_0=0.00365760731521463 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Juneau_County_Feet
    (103429, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=1.0000260649 +x_0=185928.372771146 +y_0=0.000914401828803658 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Kenosha_County_Feet
    (103430, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1.0000233704 +x_0=79857.7615443231 +y_0=0.00121920243840488 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Kewaunee_County_Feet
    (103431, "+proj=tmerc +lat_0=43.4511111111111 +lon_0=-91.3166666666667 +k=1.0000319985 +x_0=130454.659690119 +y_0=0.00335280670561341 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_LaCrosse_County_Feet (deprecated)
    (103432, "+proj=lcc +lat_1=42.6375622769444 +lat_0=42.6375622769444 +lon_0=-89.8388888888889 +k_0=1.0000390487 +x_0=170078.74015748 +y_0=45830.2948437897 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Lafayette_County_Feet
    (103433, "+proj=lcc +lat_1=45.1542371052778 +lat_0=45.1542371052778 +lon_0=-89.0333333333333 +k_0=1.0000627024 +x_0=198425.196850394 +y_0=105279.782880366 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Langlade_County_Feet (deprecated)
    (103434, "+proj=tmerc +lat_0=44.8444444444444 +lon_0=-89.7333333333333 +k=1.0000599003 +x_0=116129.032258065 +y_0=0.00579121158242317 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Lincoln_County_Feet (deprecated)
    (103435, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1.0000233704 +x_0=79857.7615443231 +y_0=0.00121920243840488 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Manitowoc_County_Feet
    (103436, "+proj=lcc +lat_1=44.9009044236111 +lat_0=44.9009044236111 +lon_0=-89.77 +k_0=1.000053289 +x_0=74676.1493522987 +y_0=55049.2669545339 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Marathon_County_Feet (deprecated)
    (103437, "+proj=tmerc +lat_0=44.6916666666667 +lon_0=-87.7111111111111 +k=1.0000234982 +x_0=238658.879451359 +y_0=0.00304800609601219 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Marinette_County_Feet (deprecated)
    (103438, "+proj=lcc +lat_1=43.8070001177778 +lat_0=43.8070001177778 +lon_0=-89.2416666666667 +k_0=1.0000344057 +x_0=150876.301752604 +y_0=79170.7793751588 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Marquette_County_Feet
    (103439, "+proj=tmerc +lat_0=44.7166666666667 +lon_0=-88.4166666666667 +k=1.0000362499 +x_0=105461.012141224 +y_0=0.00304800609601219 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Menominee_County_Feet (deprecated)
    (103440, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=1.0000260649 +x_0=185928.372771146 +y_0=0.000914401828803658 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Milwaukee_County_Feet
    (103441, "+proj=lcc +lat_1=44.0000739286111 +lat_0=44.0000739286111 +lon_0=-90.6416666666667 +k_0=1.0000434122 +x_0=204521.209042418 +y_0=121923.986182372 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Monroe_County_Feet (deprecated)
    (103442, "+proj=tmerc +lat_0=44.3972222222222 +lon_0=-87.9083333333333 +k=1.0000236869 +x_0=182880.367589535 +y_0=0.00335280670561341 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Oconto_County_Feet (deprecated)
    (103443, "+proj=lcc +lat_1=45.7042237702778 +lat_0=45.7042237702778 +lon_0=-89.5444444444444 +k_0=1.0000686968 +x_0=70104.1402082804 +y_0=57588.0347472695 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Oneida_County_Feet (deprecated)
    (103444, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=1.0000286569 +x_0=244754.889204978 +y_0=0.00487680975361951 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Outagamie_County_Feet
    (103445, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=1.0000260649 +x_0=185928.372771146 +y_0=0.000914401828803658 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Ozaukee_County_Feet
    (103446, "+proj=lcc +lat_1=44.6361488719444 +lat_0=44.6361488719444 +lon_0=-92.2277777777778 +k_0=1.0000362977 +x_0=167640.335280671 +y_0=86033.0877317755 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Pepin_County_Feet
    (103447, "+proj=lcc +lat_1=44.6361488719444 +lat_0=44.6361488719444 +lon_0=-92.2277777777778 +k_0=1.0000362977 +x_0=167640.335280671 +y_0=86033.0877317755 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Pierce_County_Feet
    (103448, "+proj=tmerc +lat_0=44.6611111111111 +lon_0=-92.6333333333333 +k=1.0000433849 +x_0=141732.282245365 +y_0=0.00579121158242317 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Polk_County_Feet (deprecated)
    (103449, "+proj=lcc +lat_1=44.4168239752778 +lat_0=44.4168239752778 +lon_0=-89.5 +k_0=1.000039936 +x_0=56388.1127762256 +y_0=50022.1874523749 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Portage_County_Feet (deprecated)
    (103450, "+proj=tmerc +lat_0=44.5555555555556 +lon_0=-90.4888888888889 +k=1.0000649554 +x_0=227990.854457709 +y_0=0.0109728219456439 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Price_County_Feet (deprecated)
    (103451, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=1.0000260649 +x_0=185928.372771146 +y_0=0.000914401828803658 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Racine_County_Feet
    (103452, "+proj=lcc +lat_1=43.3223129275 +lat_0=43.3223129275 +lon_0=-90.4305555555556 +k_0=1.0000375653 +x_0=202387.60477521 +y_0=134255.425450851 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Richland_County_Feet (deprecated)
    (103453, "+proj=tmerc +lat_0=41.9444444444444 +lon_0=-89.0722222222222 +k=1.0000337311 +x_0=146304.292608585 +y_0=0.00670561341122682 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Rock_County_Feet (deprecated)
    (103454, "+proj=tmerc +lat_0=43.9194444444444 +lon_0=-91.0666666666667 +k=1.0000495976 +x_0=250546.101397003 +y_0=0.0234696469392939 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Rusk_County_Feet (deprecated)
    (103455, "+proj=tmerc +lat_0=42.8194444444444 +lon_0=-89.9 +k=1.0000373868 +x_0=185623.571551943 +y_0=0.00518161036322073 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Sauk_County_Feet (deprecated)
    (103456, "+proj=lcc +lat_1=45.9000991313889 +lat_0=45.9000991313889 +lon_0=-91.1166666666667 +k_0=1.0000573461 +x_0=216713.233731268 +y_0=120734.163169926 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Sawyer_County_Feet (deprecated)
    (103457, "+proj=tmerc +lat_0=44.0361111111111 +lon_0=-88.6055555555556 +k=1.000032144 +x_0=262433.32517145 +y_0=0.0094488188976378 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Shawano_County_Feet (deprecated)
    (103458, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1.0000233704 +x_0=79857.7615443231 +y_0=0.00121920243840488 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Sheboygan_County_Feet
    (103459, "+proj=tmerc +lat_0=44.0361111111111 +lon_0=-92.6333333333333 +k=1.0000381803 +x_0=165506.73009906 +y_0=0.0103632207264415 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_St_Croix_County_Feet (deprecated)
    (103460, "+proj=lcc +lat_1=45.1778220858333 +lat_0=45.1778220858333 +lon_0=-90.4833333333333 +k_0=1.0000597566 +x_0=187147.574295149 +y_0=107746.752146304 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Taylor_County_Feet (deprecated)
    (103461, "+proj=tmerc +lat_0=43.1611111111111 +lon_0=-91.3666666666667 +k=1.0000361538 +x_0=256946.913893828 +y_0=0.00396240792481585 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Trempealeau_County_Feet (deprecated)
    (103462, "+proj=lcc +lat_1=43.5750329397222 +lat_0=43.5750329397222 +lon_0=-90.7833333333333 +k_0=1.0000408158 +x_0=222504.44500889 +y_0=47532.0603505207 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Vernon_County_Feet (deprecated)
    (103463, "+proj=lcc +lat_1=46.0778440905556 +lat_0=46.0778440905556 +lon_0=-89.4888888888889 +k_0=1.0000730142 +x_0=134417.068834138 +y_0=50337.1092710185 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Vilas_County_Feet (deprecated)
    (103464, "+proj=lcc +lat_1=42.6694620969444 +lat_0=42.6694620969444 +lon_0=-88.5416666666667 +k_0=1.0000367192 +x_0=232562.86512573 +y_0=111088.222402845 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Walworth_County_Feet (deprecated)
    (103465, "+proj=lcc +lat_1=45.9612198333333 +lat_0=45.9612198333333 +lon_0=-91.7833333333333 +k_0=1.0000475376 +x_0=234086.868173736 +y_0=188358.605943612 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Washburn_County_Feet (deprecated)
    (103466, "+proj=tmerc +lat_0=42.9180555555555 +lon_0=-88.0638888888889 +k=1.00003738 +x_0=120091.441402083 +y_0=0.00304800609601219 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Washington_County_Feet (deprecated)
    (103467, "+proj=tmerc +lat_0=42.5694444444444 +lon_0=-88.225 +k=1.0000346179 +x_0=208788.417881636 +y_0=0.00335280670561341 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Waukesha_County_Feet (deprecated)
    (103468, "+proj=tmerc +lat_0=43.4202777777778 +lon_0=-88.8166666666667 +k=1.0000333645 +x_0=185013.970942342 +y_0=0.00701041402082804 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Waupaca_County_Feet (deprecated)
    (103469, "+proj=lcc +lat_1=44.1139440458333 +lat_0=44.1139440458333 +lon_0=-89.2416666666667 +k_0=1.0000392096 +x_0=120091.44018288 +y_0=45069.7588011176 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Waushara_County_Feet (deprecated)
    (103470, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=1.0000286569 +x_0=244754.889204978 +y_0=0.00487680975361951 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Winnebago_County_Feet
    (103471, "+proj=lcc +lat_1=44.3625954694444 +lat_0=44.3625954694444 +lon_0=-90 +k_0=1.0000421209 +x_0=208483.617272035 +y_0=134589.753924308 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_WISCRS_Wood_County_Feet (deprecated)
    (103472, "+proj=lcc +lat_0=44.25 +lon_0=-109.5 +lat_1=45 +lat_2=49 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Montana_FIPS_2500
    (103473, "+proj=lcc +lat_0=44.25 +lon_0=-109.5 +lat_1=45 +lat_2=49 +x_0=600000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Montana_FIPS_2500_Ft_Intl
    (103474, "+proj=lcc +lat_0=39.8333333333333 +lon_0=-100 +lat_1=40 +lat_2=43 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Nebraska_FIPS_2600
    (103475, "+proj=lcc +lat_0=39.8333333333333 +lon_0=-100 +lat_1=40 +lat_2=43 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Nebraska_FIPS_2600_Ft_US
    (103476, "+proj=tmerc +lat_0=34.75 +lon_0=-115.583333333333 +k=0.9999 +x_0=200000 +y_0=8000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Nevada_East_FIPS_2701
    (103477, "+proj=tmerc +lat_0=34.75 +lon_0=-116.666666666667 +k=0.9999 +x_0=500000 +y_0=6000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Nevada_Central_FIPS_2702
    (103478, "+proj=tmerc +lat_0=34.75 +lon_0=-118.583333333333 +k=0.9999 +x_0=800000 +y_0=4000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Nevada_West_FIPS_2703
    (103479, "+proj=tmerc +lat_0=34.75 +lon_0=-115.583333333333 +k=0.9999 +x_0=200000 +y_0=8000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Nevada_East_FIPS_2701_Ft_US
    (103480, "+proj=tmerc +lat_0=34.75 +lon_0=-116.666666666667 +k=0.9999 +x_0=500000 +y_0=6000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Nevada_Central_FIPS_2702_Ft_US
    (103481, "+proj=tmerc +lat_0=34.75 +lon_0=-118.583333333333 +k=0.9999 +x_0=800000 +y_0=4000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Nevada_West_FIPS_2703_Ft_US
    (103482, "+proj=tmerc +lat_0=42.5 +lon_0=-71.6666666666667 +k=0.999966666666667 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_Hampshire_FIPS_2800
    (103483, "+proj=tmerc +lat_0=42.5 +lon_0=-71.6666666666667 +k=0.999966666666667 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_Hampshire_FIPS_2800_Ft_US
    (103484, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_Jersey_FIPS_2900
    (103485, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_Jersey_FIPS_2900_Ft_US
    (103486, "+proj=tmerc +lat_0=31 +lon_0=-104.333333333333 +k=0.999909090909091 +x_0=165000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_Mexico_East_FIPS_3001
    (103487, "+proj=tmerc +lat_0=31 +lon_0=-106.25 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_Mexico_Central_FIPS_3002
    (103488, "+proj=tmerc +lat_0=31 +lon_0=-107.833333333333 +k=0.999916666666667 +x_0=830000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_Mexico_West_FIPS_3003
    (103489, "+proj=tmerc +lat_0=31 +lon_0=-104.333333333333 +k=0.999909090909091 +x_0=165000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_Mexico_East_FIPS_3001_Ft_US
    (103490, "+proj=tmerc +lat_0=31 +lon_0=-106.25 +k=0.9999 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_Mexico_Central_FIPS_3002_Ft_US
    (103491, "+proj=tmerc +lat_0=31 +lon_0=-107.833333333333 +k=0.999916666666667 +x_0=830000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_Mexico_West_FIPS_3003_Ft_US
    (103492, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_York_East_FIPS_3101
    (103493, "+proj=tmerc +lat_0=40 +lon_0=-76.5833333333333 +k=0.9999375 +x_0=250000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_York_Central_FIPS_3102
    (103494, "+proj=tmerc +lat_0=40 +lon_0=-78.5833333333333 +k=0.9999375 +x_0=350000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_York_West_FIPS_3103
    (103495, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-74 +lat_1=40.6666666666667 +lat_2=41.0333333333333 +x_0=300000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_New_York_Long_Island_FIPS_3104
    (103496, "+proj=tmerc +lat_0=38.8333333333333 +lon_0=-74.5 +k=0.9999 +x_0=150000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_York_East_FIPS_3101_Ft_US
    (103497, "+proj=tmerc +lat_0=40 +lon_0=-76.5833333333333 +k=0.9999375 +x_0=250000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_York_Central_FIPS_3102_Ft_US
    (103498, "+proj=tmerc +lat_0=40 +lon_0=-78.5833333333333 +k=0.9999375 +x_0=350000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_York_West_FIPS_3103_Ft_US
    (103499, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-74 +lat_1=40.6666666666667 +lat_2=41.0333333333333 +x_0=300000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_New_York_Long_Isl_FIPS_3104_Ft_US
    (103500, "+proj=lcc +lat_0=33.75 +lon_0=-79 +lat_1=34.3333333333333 +lat_2=36.1666666666667 +x_0=609601.219202438 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_North_Carolina_FIPS_3200
    (103501, "+proj=lcc +lat_0=33.75 +lon_0=-79 +lat_1=34.3333333333333 +lat_2=36.1666666666667 +x_0=609601.219202438 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_North_Carolina_FIPS_3200_Ft_US
    (103502, "+proj=lcc +lat_0=47 +lon_0=-100.5 +lat_1=47.4333333333333 +lat_2=48.7333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_North_Dakota_North_FIPS_3301
    (103503, "+proj=lcc +lat_0=45.6666666666667 +lon_0=-100.5 +lat_1=46.1833333333333 +lat_2=47.4833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_North_Dakota_South_FIPS_3302
    (103504, "+proj=lcc +lat_0=47 +lon_0=-100.5 +lat_1=47.4333333333333 +lat_2=48.7333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_North_Dakota_North_FIPS_3301_FtI
    (103505, "+proj=lcc +lat_0=45.6666666666667 +lon_0=-100.5 +lat_1=46.1833333333333 +lat_2=47.4833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_North_Dakota_South_FIPS_3302_FtI
    (103506, "+proj=lcc +lat_0=39.6666666666667 +lon_0=-82.5 +lat_1=40.4333333333333 +lat_2=41.7 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Ohio_North_FIPS_3401
    (103507, "+proj=lcc +lat_0=38 +lon_0=-82.5 +lat_1=38.7333333333333 +lat_2=40.0333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Ohio_South_FIPS_3402
    (103508, "+proj=lcc +lat_0=39.6666666666667 +lon_0=-82.5 +lat_1=40.4333333333333 +lat_2=41.7 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Ohio_North_FIPS_3401_Ft_US
    (103509, "+proj=lcc +lat_0=38 +lon_0=-82.5 +lat_1=38.7333333333333 +lat_2=40.0333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Ohio_South_FIPS_3402_Ft_US
    (103510, "+proj=lcc +lat_0=35 +lon_0=-98 +lat_1=35.5666666666667 +lat_2=36.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Oklahoma_North_FIPS_3501
    (103511, "+proj=lcc +lat_0=33.3333333333333 +lon_0=-98 +lat_1=33.9333333333333 +lat_2=35.2333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Oklahoma_South_FIPS_3502
    (103512, "+proj=lcc +lat_0=35 +lon_0=-98 +lat_1=35.5666666666667 +lat_2=36.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Oklahoma_North_FIPS_3501_Ft_US
    (103513, "+proj=lcc +lat_0=33.3333333333333 +lon_0=-98 +lat_1=33.9333333333333 +lat_2=35.2333333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Oklahoma_South_FIPS_3502_Ft_US
    (103514, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-77.75 +lat_1=40.8833333333333 +lat_2=41.95 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Pennsylvania_North_FIPS_3701
    (103515, "+proj=lcc +lat_0=40.1666666666667 +lon_0=-77.75 +lat_1=40.8833333333333 +lat_2=41.95 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Pennsylvania_North_FIPS_3701_Ft_US
    (103516, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-77.75 +lat_1=39.9333333333333 +lat_2=40.9666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Pennsylvania_South_FIPS_3702
    (103517, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-77.75 +lat_1=39.9333333333333 +lat_2=40.9666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Pennsylvania_South_FIPS_3702_Ft_US
    (103518, "+proj=tmerc +lat_0=41.0833333333333 +lon_0=-71.5 +k=0.99999375 +x_0=100000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Rhode_Island_FIPS_3800
    (103519, "+proj=tmerc +lat_0=41.0833333333333 +lon_0=-71.5 +k=0.99999375 +x_0=100000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Rhode_Island_FIPS_3800_Ft_US
    (103520, "+proj=lcc +lat_0=31.8333333333333 +lon_0=-81 +lat_1=32.5 +lat_2=34.8333333333333 +x_0=609600 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_South_Carolina_FIPS_3900
    (103521, "+proj=lcc +lat_0=31.8333333333333 +lon_0=-81 +lat_1=32.5 +lat_2=34.8333333333333 +x_0=609600 +y_0=0 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_South_Carolina_FIPS_3900_Ft_Intl
    (103522, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-100 +lat_1=44.4166666666667 +lat_2=45.6833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_South_Dakota_North_FIPS_4001
    (103523, "+proj=lcc +lat_0=42.3333333333333 +lon_0=-100.333333333333 +lat_1=42.8333333333333 +lat_2=44.4 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_South_Dakota_South_FIPS_4002
    (103524, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-100 +lat_1=44.4166666666667 +lat_2=45.6833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_South_Dakota_North_FIPS_4001_Ft_US
    (103525, "+proj=lcc +lat_0=42.3333333333333 +lon_0=-100.333333333333 +lat_1=42.8333333333333 +lat_2=44.4 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_South_Dakota_South_FIPS_4002_Ft_US
    (103526, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-86 +lat_1=35.25 +lat_2=36.4166666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Tennessee_FIPS_4100
    (103527, "+proj=lcc +lat_0=34.3333333333333 +lon_0=-86 +lat_1=35.25 +lat_2=36.4166666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Tennessee_FIPS_4100_Ft_US
    (103528, "+proj=utm +zone=28 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_28N (deprecated)
    (103529, "+proj=utm +zone=29 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_29N (deprecated)
    (103530, "+proj=utm +zone=30 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_30N (deprecated)
    (103531, "+proj=utm +zone=31 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_31N (deprecated)
    (103532, "+proj=utm +zone=32 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_32N (deprecated)
    (103533, "+proj=utm +zone=33 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_33N (deprecated)
    (103534, "+proj=utm +zone=34 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_34N (deprecated)
    (103535, "+proj=utm +zone=35 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_35N (deprecated)
    (103536, "+proj=utm +zone=36 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_36N (deprecated)
    (103537, "+proj=utm +zone=37 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_37N (deprecated)
    (103538, "+proj=utm +zone=38 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_UTM_Zone_38N (deprecated)
    (103539, "+proj=lcc +lat_0=34 +lon_0=-101.5 +lat_1=34.65 +lat_2=36.1833333333333 +x_0=200000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_North_FIPS_4201
    (103540, "+proj=lcc +lat_0=31.6666666666667 +lon_0=-98.5 +lat_1=32.1333333333333 +lat_2=33.9666666666667 +x_0=600000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_North_Central_FIPS_4202
    (103541, "+proj=lcc +lat_0=29.6666666666667 +lon_0=-100.333333333333 +lat_1=30.1166666666667 +lat_2=31.8833333333333 +x_0=700000 +y_0=3000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_Central_FIPS_4203
    (103542, "+proj=lcc +lat_0=27.8333333333333 +lon_0=-99 +lat_1=28.3833333333333 +lat_2=30.2833333333333 +x_0=600000 +y_0=4000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_South_Central_FIPS_4204
    (103543, "+proj=lcc +lat_0=25.6666666666667 +lon_0=-98.5 +lat_1=26.1666666666667 +lat_2=27.8333333333333 +x_0=300000 +y_0=5000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_South_FIPS_4205
    (103544, "+proj=lcc +lat_0=34 +lon_0=-101.5 +lat_1=34.65 +lat_2=36.1833333333333 +x_0=200000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_North_FIPS_4201_Ft_US
    (103545, "+proj=lcc +lat_0=31.6666666666667 +lon_0=-98.5 +lat_1=32.1333333333333 +lat_2=33.9666666666667 +x_0=600000 +y_0=2000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_North_Central_FIPS_4202_FtUS
    (103546, "+proj=lcc +lat_0=29.6666666666667 +lon_0=-100.333333333333 +lat_1=30.1166666666667 +lat_2=31.8833333333333 +x_0=700000 +y_0=3000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_Central_FIPS_4203_Ft_US
    (103547, "+proj=lcc +lat_0=27.8333333333333 +lon_0=-99 +lat_1=28.3833333333333 +lat_2=30.2833333333333 +x_0=600000 +y_0=4000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_South_Central_FIPS_4204_FtUS
    (103548, "+proj=lcc +lat_0=25.6666666666667 +lon_0=-98.5 +lat_1=26.1666666666667 +lat_2=27.8333333333333 +x_0=300000 +y_0=5000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Texas_South_FIPS_4205_Ft_US
    (103549, "+proj=lcc +lat_0=40.3333333333333 +lon_0=-111.5 +lat_1=40.7166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_North_FIPS_4301
    (103550, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-111.5 +lat_1=39.0166666666667 +lat_2=40.65 +x_0=500000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_Central_FIPS_4302
    (103551, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-111.5 +lat_1=37.2166666666667 +lat_2=38.35 +x_0=500000 +y_0=3000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_South_FIPS_4303
    (103552, "+proj=lcc +lat_0=40.3333333333333 +lon_0=-111.5 +lat_1=40.7166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=1000000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_North_FIPS_4301_Ft_Intl
    (103553, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-111.5 +lat_1=39.0166666666667 +lat_2=40.65 +x_0=500000 +y_0=2000000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_Central_FIPS_4302_Ft_Intl
    (103554, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-111.5 +lat_1=37.2166666666667 +lat_2=38.35 +x_0=500000 +y_0=3000000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_South_FIPS_4303_Ft_Intl
    (103555, "+proj=lcc +lat_0=40.3333333333333 +lon_0=-111.5 +lat_1=40.7166666666667 +lat_2=41.7833333333333 +x_0=500000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_North_FIPS_4301_Ft_US
    (103556, "+proj=lcc +lat_0=38.3333333333333 +lon_0=-111.5 +lat_1=39.0166666666667 +lat_2=40.65 +x_0=500000 +y_0=2000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_Central_FIPS_4302_Ft_US
    (103557, "+proj=lcc +lat_0=36.6666666666667 +lon_0=-111.5 +lat_1=37.2166666666667 +lat_2=38.35 +x_0=500000 +y_0=3000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Utah_South_FIPS_4303_Ft_US
    (103558, "+proj=tmerc +lat_0=42.5 +lon_0=-72.5 +k=0.999964285714286 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Vermont_FIPS_4400
    (103559, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-78.5 +lat_1=38.0333333333333 +lat_2=39.2 +x_0=3500000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Virginia_North_FIPS_4501
    (103560, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-78.5 +lat_1=36.7666666666667 +lat_2=37.9666666666667 +x_0=3500000 +y_0=1000000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Virginia_South_FIPS_4502
    (103561, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-78.5 +lat_1=38.0333333333333 +lat_2=39.2 +x_0=3500000 +y_0=2000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Virginia_North_FIPS_4501_Ft_US
    (103562, "+proj=lcc +lat_0=36.3333333333333 +lon_0=-78.5 +lat_1=36.7666666666667 +lat_2=37.9666666666667 +x_0=3500000 +y_0=1000000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Virginia_South_FIPS_4502_Ft_US
    (103563, "+proj=lcc +lat_0=47 +lon_0=-120.833333333333 +lat_1=47.5 +lat_2=48.7333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Washington_North_FIPS_4601
    (103564, "+proj=lcc +lat_0=45.3333333333333 +lon_0=-120.5 +lat_1=45.8333333333333 +lat_2=47.3333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Washington_South_FIPS_4602
    (103565, "+proj=lcc +lat_0=47 +lon_0=-120.833333333333 +lat_1=47.5 +lat_2=48.7333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Washington_North_FIPS_4601_Ft_US
    (103566, "+proj=lcc +lat_0=45.3333333333333 +lon_0=-120.5 +lat_1=45.8333333333333 +lat_2=47.3333333333333 +x_0=500000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Washington_South_FIPS_4602_Ft_US
    (103567, "+proj=lcc +lat_0=38.5 +lon_0=-79.5 +lat_1=39 +lat_2=40.25 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_West_Virginia_North_FIPS_4701
    (103568, "+proj=lcc +lat_0=37 +lon_0=-81 +lat_1=37.4833333333333 +lat_2=38.8833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_West_Virginia_South_FIPS_4702
    (103569, "+proj=lcc +lat_0=38.5 +lon_0=-79.5 +lat_1=39 +lat_2=40.25 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_West_Virginia_North_FIPS_4701_FtUS
    (103570, "+proj=lcc +lat_0=37 +lon_0=-81 +lat_1=37.4833333333333 +lat_2=38.8833333333333 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_West_Virginia_South_FIPS_4702_FtUS
    (103571, "+proj=lcc +lat_0=45.1666666666667 +lon_0=-90 +lat_1=45.5666666666667 +lat_2=46.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Wisconsin_North_FIPS_4801
    (103572, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-90 +lat_1=44.25 +lat_2=45.5 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Wisconsin_Central_FIPS_4802
    (103573, "+proj=lcc +lat_0=42 +lon_0=-90 +lat_1=42.7333333333333 +lat_2=44.0666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Wisconsin_South_FIPS_4803
    (103574, "+proj=lcc +lat_0=45.1666666666667 +lon_0=-90 +lat_1=45.5666666666667 +lat_2=46.7666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Wisconsin_North_FIPS_4801_Ft_US
    (103575, "+proj=lcc +lat_0=43.8333333333333 +lon_0=-90 +lat_1=44.25 +lat_2=45.5 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Wisconsin_Central_FIPS_4802_Ft_US
    (103576, "+proj=lcc +lat_0=42 +lon_0=-90 +lat_1=42.7333333333333 +lat_2=44.0666666666667 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Wisconsin_South_FIPS_4803_Ft_US
    (103577, "+proj=tmerc +lat_0=40.5 +lon_0=-105.166666666667 +k=0.9999375 +x_0=200000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Wyoming_East_FIPS_4901
    (103578, "+proj=tmerc +lat_0=40.5 +lon_0=-107.333333333333 +k=0.9999375 +x_0=400000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Wyoming_East_Central_FIPS_4902
    (103579, "+proj=tmerc +lat_0=40.5 +lon_0=-108.75 +k=0.9999375 +x_0=600000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Wyoming_West_Central_FIPS_4903
    (103580, "+proj=tmerc +lat_0=40.5 +lon_0=-110.083333333333 +k=0.9999375 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_CORS96_StatePlane_Wyoming_West_FIPS_4904
    (103581, "+proj=tmerc +lat_0=40.5 +lon_0=-105.166666666667 +k=0.9999375 +x_0=200000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Wyoming_East_FIPS_4901_Ft_US
    (103582, "+proj=tmerc +lat_0=40.5 +lon_0=-107.333333333333 +k=0.9999375 +x_0=400000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Wyoming_E_Central_FIPS_4902_Ft_US
    (103583, "+proj=tmerc +lat_0=40.5 +lon_0=-108.75 +k=0.9999375 +x_0=600000 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Wyoming_W_Central_FIPS_4903_Ft_US
    (103584, "+proj=tmerc +lat_0=0 +lon_0=24 +k=0.9996 +x_0=500000 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // ETRF_1989_TM_Baltic_1993 (deprecated)
    (103585, "+proj=tmerc +lat_0=40.5 +lon_0=-110.083333333333 +k=0.9999375 +x_0=800000 +y_0=100000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_CORS96_StatePlane_Wyoming_West_FIPS_4904_Ft_US
    (103586, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Navajo_Nation_Coordinate_System_Meters
    (103587, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Navajo_Nation_Coordinate_System_US_Feet
    (103588, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_HARN_Navajo_Nation_Coordinate_System_Intl_Feet
    (103589, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_NSRS2007_Navajo_Nation_Coordinate_System_Meters
    (103590, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_NSRS2007_Navajo_Nation_Coordinate_System_US_Feet
    (103591, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_NSRS2007_Navajo_Nation_Coordinate_System_Intl_Feet
    (103592, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(2011)_Navajo_Nation_Coordinate_System_Meters
    (103593, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_(2011)_Navajo_Nation_Coordinate_System_US_Feet
    (103594, "+proj=lcc +lat_1=36 +lat_0=36 +lon_0=-109.5 +k_0=1.00023 +x_0=300000 +y_0=600000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_(2011)_Navajo_Nation_Coordinate_System_Intl_Feet
    (103595, "+proj=utm +zone=39 +ellps=GRS80 +towgs84=1.16835,-1.42001,-2.24431,-0.00822,-0.05508,0.01818,0.23388 +units=m +no_defs"), // ONGD17_UTM_Zone_39N (deprecated)
    (103596, "+proj=utm +zone=40 +ellps=GRS80 +towgs84=1.16835,-1.42001,-2.24431,-0.00822,-0.05508,0.01818,0.23388 +units=m +no_defs"), // ONGD17_UTM_Zone_40N (deprecated)
    (103597, "+proj=utm +zone=41 +ellps=GRS80 +towgs84=1.16835,-1.42001,-2.24431,-0.00822,-0.05508,0.01818,0.23388 +units=m +no_defs"), // ONGD17_UTM_Zone_41N (deprecated)
    (103598, "+proj=tmerc +lat_0=0 +lon_0=-90.5 +k=0.9998 +x_0=500000 +y_0=0 +datum=WGS84 +units=m +no_defs"), // GTM
    (103599, "+proj=tmerc +lat_0=4 +lon_0=-73 +k=0.9992 +x_0=5000000 +y_0=2000000 +ellps=GRS80 +units=m +no_defs"), // MAGNA-SIRGAS_CMT12
    (103600, "+proj=tmerc +lat_0=46.1541666666667 +lon_0=-93.4325 +k=1.000059152669 +x_0=152409.319685395 +y_0=30481.863937079 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Aitkin_Meters
    (103601, "+proj=tmerc +lat_0=46.63 +lon_0=-96.7 +k=1.000045317862 +x_0=152407.211256591 +y_0=30481.4422513183 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Clay_Meters
    (103602, "+proj=tmerc +lat_0=47.1516666666667 +lon_0=-95.3758333333333 +k=1.000072505661 +x_0=152411.354685446 +y_0=30482.2709370892 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Clearwater_Meters
    (103603, "+proj=tmerc +lat_0=46.8036111111111 +lon_0=-94.9205555555556 +k=1.000071553661 +x_0=152411.209600356 +y_0=30482.2419200711 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Hubbard_Meters
    (103604, "+proj=tmerc +lat_0=47.0666666666667 +lon_0=-91.4091666666667 +k=1.000075844621 +x_0=152411.863543968 +y_0=30482.3727087935 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Lake_Meters
    (103605, "+proj=tmerc +lat_0=45.5588888888889 +lon_0=-93.6205555555556 +k=1.000054146138 +x_0=152408.556688545 +y_0=30481.7113377089 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Mille_Lacs_Meters
    (103606, "+proj=tmerc +lat_0=44.7458333333333 +lon_0=-92.8333333333333 +k=1.000039836799 +x_0=152406.37594092 +y_0=30481.2751881839 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Washington_Meters
    (103607, "+proj=tmerc +lat_0=46.0216666666667 +lon_0=-96.5244444444444 +k=1.000048901066 +x_0=152407.757337973 +y_0=30481.5514675946 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Wilkin_Meters
    (103608, "+proj=lcc +lat_0=45.0352777777778 +lon_0=-93.2666666666667 +lat_1=45.0666666666667 +lat_2=45.3666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378418.941 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Anoka_Meters
    (103609, "+proj=lcc +lat_0=46.7177777777778 +lon_0=-95.6833333333333 +lat_1=46.7833333333333 +lat_2=47.0833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378586.581 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Becker_Meters
    (103610, "+proj=lcc +lat_0=48.02 +lon_0=-95.0166666666667 +lat_1=48.1166666666667 +lat_2=48.4666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378505.809 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Beltrami_North_Meters
    (103611, "+proj=lcc +lat_0=47.4125 +lon_0=-94.85 +lat_1=47.5 +lat_2=47.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378544.823 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Beltrami_South_Meters
    (103612, "+proj=lcc +lat_0=45.5591666666667 +lon_0=-94.05 +lat_1=45.5833333333333 +lat_2=45.7833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378490.569 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Benton_Meters
    (103613, "+proj=lcc +lat_0=45.1522222222222 +lon_0=-96.05 +lat_1=45.2166666666667 +lat_2=45.5333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378470.757 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Big_Stone_Meters
    (103614, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-94.2666666666667 +lat_1=43.9333333333333 +lat_2=44.3666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378403.701 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Blue_Earth_Meters
    (103615, "+proj=lcc +lat_0=44.1080555555556 +lon_0=-94.7333333333333 +lat_1=44.1666666666667 +lat_2=44.4666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378434.181 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Brown_Meters
    (103616, "+proj=lcc +lat_0=46.4172222222222 +lon_0=-92.6833333333333 +lat_1=46.4666666666667 +lat_2=46.7333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378454.907 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Carlton_Meters
    (103617, "+proj=lcc +lat_0=44.6397222222222 +lon_0=-93.7666666666667 +lat_1=44.6833333333333 +lat_2=44.9 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378400.653 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Carver_Meters
    (103618, "+proj=lcc +lat_0=46.8036111111111 +lon_0=-94.2166666666667 +lat_1=46.9166666666667 +lat_2=47.3166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378567.378 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Cass_North_Meters
    (103619, "+proj=lcc +lat_0=46.1563888888889 +lon_0=-94.4666666666667 +lat_1=46.2666666666667 +lat_2=46.7333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378546.957 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Cass_South_Meters
    (103620, "+proj=lcc +lat_0=44.7527777777778 +lon_0=-95.85 +lat_1=44.8333333333333 +lat_2=45.2 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378476.853 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Chippewa_Meters
    (103621, "+proj=lcc +lat_0=45.2963888888889 +lon_0=-93.0833333333333 +lat_1=45.3333333333333 +lat_2=45.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378411.321 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Chisago_Meters
    (103622, "+proj=lcc +lat_0=47.8833333333333 +lon_0=-90.25 +lat_1=47.9333333333333 +lat_2=48.1666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378647.541 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Cook_North_Meters
    (103623, "+proj=lcc +lat_0=47.4388888888889 +lon_0=-90.25 +lat_1=47.55 +lat_2=47.8166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378647.541 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Cook_South_Meters
    (103624, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-94.9166666666667 +lat_1=43.9 +lat_2=44.1666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378514.953 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Cottonwood_Meters
    (103625, "+proj=lcc +lat_0=46.1563888888889 +lon_0=-94.4666666666667 +lat_1=46.2666666666667 +lat_2=46.7333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378546.957 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Crow_Wing_Meters
    (103626, "+proj=lcc +lat_0=44.4719444444445 +lon_0=-93.3166666666667 +lat_1=44.5166666666667 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378421.989 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Dakota_Meters
    (103627, "+proj=lcc +lat_0=43.8338888888889 +lon_0=-92.9166666666667 +lat_1=43.8833333333333 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378481.425 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Dodge_Meters
    (103628, "+proj=lcc +lat_0=45.7588888888889 +lon_0=-96.05 +lat_1=45.8 +lat_2=46.05 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378518.001 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Douglas_Meters
    (103629, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Faribault_Meters
    (103630, "+proj=lcc +lat_0=43.5 +lon_0=-92.0833333333333 +lat_1=43.55 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378464.661 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Fillmore_Meters
    (103631, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Freeborn_Meters
    (103632, "+proj=lcc +lat_0=44.1947222222222 +lon_0=-93.1333333333333 +lat_1=44.3 +lat_2=44.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378434.181 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Goodhue_Meters
    (103633, "+proj=lcc +lat_0=45.7588888888889 +lon_0=-96.05 +lat_1=45.8 +lat_2=46.05 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378518.001 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Grant_Meters
    (103634, "+proj=lcc +lat_0=44.7911111111111 +lon_0=-93.3833333333333 +lat_1=44.8833333333333 +lat_2=45.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378418.941 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Hennepin_Meters
    (103635, "+proj=lcc +lat_0=43.5 +lon_0=-91.4666666666667 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378436.619 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Houston_Meters
    (103636, "+proj=lcc +lat_0=45.2963888888889 +lon_0=-93.0833333333333 +lat_1=45.3333333333333 +lat_2=45.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378411.321 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Isanti_Meters
    (103637, "+proj=lcc +lat_0=47.5 +lon_0=-93.7333333333333 +lat_1=47.5666666666667 +lat_2=47.8166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378574.389 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Itasca_North_Meters
    (103638, "+proj=lcc +lat_0=47.0263888888889 +lon_0=-93.7333333333333 +lat_1=47.0833333333333 +lat_2=47.4166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378574.389 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Itasca_South_Meters
    (103639, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Jackson_Meters
    (103640, "+proj=lcc +lat_0=45.73 +lon_0=-92.9 +lat_1=45.8166666666667 +lat_2=46.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378472.281 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Kanabec_Meters
    (103641, "+proj=lcc +lat_0=44.8913888888889 +lon_0=-94.75 +lat_1=44.9666666666667 +lat_2=45.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378498.189 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Kandiyohi_Meters
    (103642, "+proj=lcc +lat_0=48.5438888888889 +lon_0=-96.15 +lat_1=48.6 +lat_2=48.9333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378449.421 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Kittson_Meters
    (103643, "+proj=lcc +lat_0=47.8458333333333 +lon_0=-93.75 +lat_1=48 +lat_2=48.6166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378525.621 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Koochiching_Meters
    (103644, "+proj=lcc +lat_0=44.7527777777778 +lon_0=-95.85 +lat_1=44.8333333333333 +lat_2=45.2 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378476.853 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Lac_Qui_Parle_Meters
    (103645, "+proj=lcc +lat_0=49.15 +lon_0=-94.9833333333333 +lat_1=49.1833333333333 +lat_2=49.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378466.185 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Lake_of_the_Woods_North_Meters
    (103646, "+proj=lcc +lat_0=48.3661111111111 +lon_0=-94.8833333333333 +lat_1=48.45 +lat_2=48.8833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378496.665 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Lake_of_the_Woods_South_Meters
    (103647, "+proj=lcc +lat_0=44.1947222222222 +lon_0=-93.1333333333333 +lat_1=44.3 +lat_2=44.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378434.181 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Le_Sueur_Meters
    (103648, "+proj=lcc +lat_0=44.1966666666667 +lon_0=-96.2666666666667 +lat_1=44.2833333333333 +lat_2=44.6166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378643.579 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Lincoln_Meters
    (103649, "+proj=lcc +lat_0=44.1955555555556 +lon_0=-95.85 +lat_1=44.25 +lat_2=44.5833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378559.758 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Lyon_Meters
    (103650, "+proj=lcc +lat_0=44.4561111111111 +lon_0=-94.6333333333333 +lat_1=44.5333333333333 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378414.369 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_McLeod_Meters
    (103651, "+proj=lcc +lat_0=47.1516666666667 +lon_0=-95.8166666666667 +lat_1=47.2 +lat_2=47.45 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378586.581 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Mahnomen_Meters
    (103652, "+proj=lcc +lat_0=48.1730555555555 +lon_0=-96.3833333333333 +lat_1=48.2333333333333 +lat_2=48.4833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378441.801 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Marshall_Meters
    (103653, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Martin_Meters
    (103654, "+proj=lcc +lat_0=44.8913888888889 +lon_0=-94.75 +lat_1=44.9666666666667 +lat_2=45.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378498.189 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Meeker_Meters
    (103655, "+proj=lcc +lat_0=45.7738888888889 +lon_0=-94.2 +lat_1=45.85 +lat_2=46.2666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378502.761 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Morrison_Meters
    (103656, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Mower_Meters
    (103657, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-95.7666666666667 +lat_1=43.9166666666667 +lat_2=44.1666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378617.061 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Murray_Meters
    (103658, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-94.2666666666667 +lat_1=43.9333333333333 +lat_2=44.3666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378403.701 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Nicollet_Meters
    (103659, "+proj=lcc +lat_0=43.5 +lon_0=-95.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378624.681 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Nobles_Meters
    (103660, "+proj=lcc +lat_0=47.1505555555556 +lon_0=-96.45 +lat_1=47.2 +lat_2=47.45 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378468.623 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Norman_Meters
    (103661, "+proj=lcc +lat_0=43.8338888888889 +lon_0=-92.9166666666667 +lat_1=43.8833333333333 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378481.425 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Olmsted_Meters
    (103662, "+proj=lcc +lat_0=46.1063888888889 +lon_0=-95.7166666666667 +lat_1=46.1833333333333 +lat_2=46.65 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378525.621 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Ottertail_Meters
    (103663, "+proj=lcc +lat_0=47.4988888888889 +lon_0=-96.3666666666667 +lat_1=47.6 +lat_2=48.0833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378445.763 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Pennington_Meters
    (103664, "+proj=lcc +lat_0=45.73 +lon_0=-92.9 +lat_1=45.8166666666667 +lat_2=46.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378472.281 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Pine_Meters
    (103665, "+proj=lcc +lat_0=43.8491666666667 +lon_0=-96.25 +lat_1=43.8833333333333 +lat_2=44.15 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378670.401 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Pipestone_Meters
    (103666, "+proj=lcc +lat_0=47.4988888888889 +lon_0=-96.3666666666667 +lat_1=47.6 +lat_2=48.0833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378445.763 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Polk_Meters
    (103667, "+proj=lcc +lat_0=45.2827777777778 +lon_0=-95.15 +lat_1=45.35 +lat_2=45.7 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378502.761 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Pope_Meters
    (103668, "+proj=lcc +lat_0=44.7911111111111 +lon_0=-93.3833333333333 +lat_1=44.8833333333333 +lat_2=45.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378418.941 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Ramsey_Meters
    (103669, "+proj=lcc +lat_0=47.4988888888889 +lon_0=-96.3666666666667 +lat_1=47.6 +lat_2=48.0833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378445.763 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Red_Lake_Meters
    (103670, "+proj=lcc +lat_0=44.1947222222222 +lon_0=-95.2333333333333 +lat_1=44.2666666666667 +lat_2=44.5666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378438.753 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Redwood_Meters
    (103671, "+proj=lcc +lat_0=44.4561111111111 +lon_0=-94.6333333333333 +lat_1=44.5333333333333 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378414.369 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Renville_Meters
    (103672, "+proj=lcc +lat_0=44.1947222222222 +lon_0=-93.1333333333333 +lat_1=44.3 +lat_2=44.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378434.181 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Rice_Meters
    (103673, "+proj=lcc +lat_0=43.5 +lon_0=-95.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378624.681 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Rock_Meters
    (103674, "+proj=lcc +lat_0=48.5438888888889 +lon_0=-96.15 +lat_1=48.6 +lat_2=48.9333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378449.421 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Roseau_Meters
    (103675, "+proj=lcc +lat_0=47.8333333333333 +lon_0=-92.45 +lat_1=47.9833333333333 +lat_2=48.5333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378543.909 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_St_Louis_North_Meters
    (103676, "+proj=lcc +lat_0=47.25 +lon_0=-92.45 +lat_1=47.3333333333333 +lat_2=47.75 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378605.783 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_St_Louis_Central_Meters
    (103677, "+proj=lcc +lat_0=46.65 +lon_0=-92.45 +lat_1=46.7833333333333 +lat_2=47.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378540.861 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_St_Louis_South_Meters
    (103678, "+proj=lcc +lat_0=44.4719444444445 +lon_0=-93.3166666666667 +lat_1=44.5166666666667 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378421.989 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Scott_Meters
    (103679, "+proj=lcc +lat_0=44.9775 +lon_0=-93.8833333333333 +lat_1=45.0333333333333 +lat_2=45.4666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378443.325 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Sherburne_Meters
    (103680, "+proj=lcc +lat_0=44.4561111111111 +lon_0=-94.6333333333333 +lat_1=44.5333333333333 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378414.369 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Sibley_Meters
    (103681, "+proj=lcc +lat_0=45.2827777777778 +lon_0=-95.15 +lat_1=45.35 +lat_2=45.7 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378502.761 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Stearns_Meters
    (103682, "+proj=lcc +lat_0=43.8338888888889 +lon_0=-92.9166666666667 +lat_1=43.8833333333333 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378481.425 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Steele_Meters
    (103683, "+proj=lcc +lat_0=45.2827777777778 +lon_0=-95.15 +lat_1=45.35 +lat_2=45.7 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378502.761 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Stevens_Meters
    (103684, "+proj=lcc +lat_0=45.1522222222222 +lon_0=-96.05 +lat_1=45.2166666666667 +lat_2=45.5333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378470.757 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Swift_Meters
    (103685, "+proj=lcc +lat_0=45.7733333333333 +lon_0=-94.9 +lat_1=45.8666666666667 +lat_2=46.2833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378548.481 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Todd_Meters
    (103686, "+proj=lcc +lat_0=45.5855555555556 +lon_0=-96.55 +lat_1=45.6333333333333 +lat_2=45.9666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378463.746 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Traverse_Meters
    (103687, "+proj=lcc +lat_0=44.1069444444444 +lon_0=-92.2666666666667 +lat_1=44.15 +lat_2=44.4166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378426.561 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Wabasha_Meters
    (103688, "+proj=lcc +lat_0=46.1563888888889 +lon_0=-94.4666666666667 +lat_1=46.2666666666667 +lat_2=46.7333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378546.957 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Wadena_Meters
    (103689, "+proj=lcc +lat_0=43.8338888888889 +lon_0=-92.9166666666667 +lat_1=43.8833333333333 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378481.425 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Waseca_Meters
    (103690, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-94.9166666666667 +lat_1=43.9 +lat_2=44.1666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378514.953 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Watonwan_Meters
    (103691, "+proj=lcc +lat_0=43.8472222222222 +lon_0=-91.6166666666667 +lat_1=43.9 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378453.688 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Winona_Meters
    (103692, "+proj=lcc +lat_0=44.9775 +lon_0=-93.8833333333333 +lat_1=45.0333333333333 +lat_2=45.4666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378443.325 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Wright_Meters
    (103693, "+proj=lcc +lat_0=44.5416666666667 +lon_0=-95.9 +lat_1=44.6666666666667 +lat_2=44.95 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378530.193 +rf=298.257222100883 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_Yellow_Medicine_Meters
    (103694, "+proj=tmerc +lat_0=46.6166666666667 +lon_0=-92.45 +k=0.99998529 +x_0=1450000 +y_0=1000000 +a=6378523 +rf=298.2752724 +units=m +no_defs"), // NAD_1983_HARN_Adj_MN_St_Louis_CS96_Meters
    (103695, "+proj=tmerc +lat_0=46.6166666666667 +lon_0=-92.45 +k=0.99998529 +x_0=1449999.9999999 +y_0=999999.999999899 +a=6378523 +rf=298.2752724 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_St_Louis_CS96_Feet
    (103696, "+proj=utm +zone=19 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(CSRS)_v6_UTM_Zone_19N (deprecated)
    (103697, "+proj=utm +zone=20 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(CSRS)_v6_UTM_Zone_20N (deprecated)
    (103698, "+proj=utm +zone=21 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(CSRS)_v6_UTM_Zone_21N (deprecated)
    (103699, "+proj=tmerc +lat_0=31.7343936111111 +lon_0=35.2045169444444 +k=1.0000067 +x_0=169529.584 +y_0=126907.39 +ellps=GRS80 +towgs84=23.772,17.49,17.859,-0.3132,-1.85274,1.67299,-5.4262 +units=m +no_defs"), // Palestine_Grid_1923_Modified_TM
    (103700, "+proj=tmerc +lat_0=46.1541666666667 +lon_0=-93.4325 +k=1.000059152669 +x_0=152409.319685395 +y_0=30481.863937079 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Aitkin_Feet
    (103701, "+proj=tmerc +lat_0=46.63 +lon_0=-96.7 +k=1.000045317862 +x_0=152407.211256591 +y_0=30481.4422513183 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Clay_Feet
    (103702, "+proj=tmerc +lat_0=47.1516666666667 +lon_0=-95.3758333333333 +k=1.000072505661 +x_0=152411.354685446 +y_0=30482.2709370892 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Clearwater_Feet
    (103703, "+proj=tmerc +lat_0=46.8036111111111 +lon_0=-94.9205555555556 +k=1.000071553661 +x_0=152411.209600356 +y_0=30482.2419200711 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Hubbard_Feet
    (103704, "+proj=tmerc +lat_0=47.0666666666667 +lon_0=-91.4091666666667 +k=1.000075844621 +x_0=152411.863543968 +y_0=30482.3727087935 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Lake_Feet
    (103705, "+proj=tmerc +lat_0=45.5588888888889 +lon_0=-93.6205555555556 +k=1.000054146138 +x_0=152408.556688545 +y_0=30481.7113377089 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Mille_Lacs_Feet
    (103706, "+proj=tmerc +lat_0=44.7458333333333 +lon_0=-92.8333333333333 +k=1.000039836799 +x_0=152406.37594092 +y_0=30481.2751881839 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Washington_Feet
    (103707, "+proj=tmerc +lat_0=46.0216666666667 +lon_0=-96.5244444444444 +k=1.000048901066 +x_0=152407.757337973 +y_0=30481.5514675946 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Wilkin_Feet
    (103708, "+proj=lcc +lat_0=45.0352777777778 +lon_0=-93.2666666666667 +lat_1=45.0666666666667 +lat_2=45.3666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378418.941 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Anoka_Feet
    (103709, "+proj=lcc +lat_0=46.7177777777778 +lon_0=-95.6833333333333 +lat_1=46.7833333333333 +lat_2=47.0833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378586.581 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Becker_Feet
    (103710, "+proj=lcc +lat_0=48.02 +lon_0=-95.0166666666667 +lat_1=48.1166666666667 +lat_2=48.4666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378505.809 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Beltrami_North_Feet
    (103711, "+proj=lcc +lat_0=47.4125 +lon_0=-94.85 +lat_1=47.5 +lat_2=47.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378544.823 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Beltrami_South_Feet
    (103712, "+proj=lcc +lat_0=45.5591666666667 +lon_0=-94.05 +lat_1=45.5833333333333 +lat_2=45.7833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378490.569 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Benton_Feet
    (103713, "+proj=lcc +lat_0=45.1522222222222 +lon_0=-96.05 +lat_1=45.2166666666667 +lat_2=45.5333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378470.757 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Big_Stone_Feet
    (103714, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-94.2666666666667 +lat_1=43.9333333333333 +lat_2=44.3666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378403.701 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Blue_Earth_Feet
    (103715, "+proj=lcc +lat_0=44.1080555555556 +lon_0=-94.7333333333333 +lat_1=44.1666666666667 +lat_2=44.4666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378434.181 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Brown_Feet
    (103716, "+proj=lcc +lat_0=46.4172222222222 +lon_0=-92.6833333333333 +lat_1=46.4666666666667 +lat_2=46.7333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378454.907 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Carlton_Feet
    (103717, "+proj=lcc +lat_0=44.6397222222222 +lon_0=-93.7666666666667 +lat_1=44.6833333333333 +lat_2=44.9 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378400.653 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Carver_Feet
    (103718, "+proj=lcc +lat_0=46.8036111111111 +lon_0=-94.2166666666667 +lat_1=46.9166666666667 +lat_2=47.3166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378567.378 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Cass_North_Feet
    (103719, "+proj=lcc +lat_0=46.1563888888889 +lon_0=-94.4666666666667 +lat_1=46.2666666666667 +lat_2=46.7333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378546.957 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Cass_South_Feet
    (103720, "+proj=lcc +lat_0=44.7527777777778 +lon_0=-95.85 +lat_1=44.8333333333333 +lat_2=45.2 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378476.853 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Chippewa_Feet
    (103721, "+proj=lcc +lat_0=45.2963888888889 +lon_0=-93.0833333333333 +lat_1=45.3333333333333 +lat_2=45.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378411.321 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Chisago_Feet
    (103722, "+proj=lcc +lat_0=47.8833333333333 +lon_0=-90.25 +lat_1=47.9333333333333 +lat_2=48.1666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378647.541 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Cook_North_Feet
    (103723, "+proj=lcc +lat_0=47.4388888888889 +lon_0=-90.25 +lat_1=47.55 +lat_2=47.8166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378647.541 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Cook_South_Feet
    (103724, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-94.9166666666667 +lat_1=43.9 +lat_2=44.1666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378514.953 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Cottonwood_Feet
    (103725, "+proj=lcc +lat_0=46.1563888888889 +lon_0=-94.4666666666667 +lat_1=46.2666666666667 +lat_2=46.7333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378546.957 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Crow_Wing_Feet
    (103726, "+proj=lcc +lat_0=44.4719444444445 +lon_0=-93.3166666666667 +lat_1=44.5166666666667 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378421.989 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Dakota_Feet
    (103727, "+proj=lcc +lat_0=43.8338888888889 +lon_0=-92.9166666666667 +lat_1=43.8833333333333 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378481.425 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Dodge_Feet
    (103728, "+proj=lcc +lat_0=45.7588888888889 +lon_0=-96.05 +lat_1=45.8 +lat_2=46.05 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378518.001 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Douglas_Feet
    (103729, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Faribault_Feet
    (103730, "+proj=lcc +lat_0=43.5 +lon_0=-92.0833333333333 +lat_1=43.55 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378464.661 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Fillmore_Feet
    (103731, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Freeborn_Feet
    (103732, "+proj=lcc +lat_0=44.1947222222222 +lon_0=-93.1333333333333 +lat_1=44.3 +lat_2=44.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378434.181 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Goodhue_Feet
    (103733, "+proj=lcc +lat_0=45.7588888888889 +lon_0=-96.05 +lat_1=45.8 +lat_2=46.05 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378518.001 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Grant_Feet
    (103734, "+proj=lcc +lat_0=44.7911111111111 +lon_0=-93.3833333333333 +lat_1=44.8833333333333 +lat_2=45.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378418.941 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Hennepin_Feet
    (103735, "+proj=lcc +lat_0=43.5 +lon_0=-91.4666666666667 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378436.619 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Houston_Feet
    (103736, "+proj=lcc +lat_0=45.2963888888889 +lon_0=-93.0833333333333 +lat_1=45.3333333333333 +lat_2=45.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378411.321 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Isanti_Feet
    (103737, "+proj=lcc +lat_0=47.5 +lon_0=-93.7333333333333 +lat_1=47.5666666666667 +lat_2=47.8166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378574.389 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Itasca_North_Feet
    (103738, "+proj=lcc +lat_0=47.0263888888889 +lon_0=-93.7333333333333 +lat_1=47.0833333333333 +lat_2=47.4166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378574.389 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Itasca_South_Feet
    (103739, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Jackson_Feet
    (103740, "+proj=lcc +lat_0=45.73 +lon_0=-92.9 +lat_1=45.8166666666667 +lat_2=46.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378472.281 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Kanabec_Feet
    (103741, "+proj=lcc +lat_0=44.8913888888889 +lon_0=-94.75 +lat_1=44.9666666666667 +lat_2=45.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378498.189 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Kandiyohi_Feet
    (103742, "+proj=lcc +lat_0=48.5438888888889 +lon_0=-96.15 +lat_1=48.6 +lat_2=48.9333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378449.421 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Kittson_Feet
    (103743, "+proj=lcc +lat_0=47.8458333333333 +lon_0=-93.75 +lat_1=48 +lat_2=48.6166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378525.621 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Koochiching_Feet
    (103744, "+proj=lcc +lat_0=44.7527777777778 +lon_0=-95.85 +lat_1=44.8333333333333 +lat_2=45.2 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378476.853 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Lac_Qui_Parle_Feet
    (103745, "+proj=lcc +lat_0=49.15 +lon_0=-94.9833333333333 +lat_1=49.1833333333333 +lat_2=49.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378466.185 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Lake_of_the_Woods_North_Feet
    (103746, "+proj=lcc +lat_0=48.3661111111111 +lon_0=-94.8833333333333 +lat_1=48.45 +lat_2=48.8833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378496.665 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Lake_of_the_Woods_South_Feet
    (103747, "+proj=lcc +lat_0=44.1947222222222 +lon_0=-93.1333333333333 +lat_1=44.3 +lat_2=44.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378434.181 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Le_Sueur_Feet
    (103748, "+proj=lcc +lat_0=44.1966666666667 +lon_0=-96.2666666666667 +lat_1=44.2833333333333 +lat_2=44.6166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378643.579 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Lincoln_Feet
    (103749, "+proj=lcc +lat_0=44.1955555555556 +lon_0=-95.85 +lat_1=44.25 +lat_2=44.5833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378559.758 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Lyon_Feet
    (103750, "+proj=lcc +lat_0=44.4561111111111 +lon_0=-94.6333333333333 +lat_1=44.5333333333333 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378414.369 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_McLeod_Feet
    (103751, "+proj=lcc +lat_0=47.1516666666667 +lon_0=-95.8166666666667 +lat_1=47.2 +lat_2=47.45 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378586.581 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Mahnomen_Feet
    (103752, "+proj=lcc +lat_0=48.1730555555555 +lon_0=-96.3833333333333 +lat_1=48.2333333333333 +lat_2=48.4833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378441.801 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Marshall_Feet
    (103753, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Martin_Feet
    (103754, "+proj=lcc +lat_0=44.8913888888889 +lon_0=-94.75 +lat_1=44.9666666666667 +lat_2=45.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378498.189 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Meeker_Feet
    (103755, "+proj=lcc +lat_0=45.7738888888889 +lon_0=-94.2 +lat_1=45.85 +lat_2=46.2666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378502.761 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Morrison_Feet
    (103756, "+proj=lcc +lat_0=43.5 +lon_0=-93.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378521.049 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Mower_Feet
    (103757, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-95.7666666666667 +lat_1=43.9166666666667 +lat_2=44.1666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378617.061 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Murray_Feet
    (103758, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-94.2666666666667 +lat_1=43.9333333333333 +lat_2=44.3666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378403.701 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Nicollet_Feet
    (103759, "+proj=lcc +lat_0=43.5 +lon_0=-95.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378624.681 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Nobles_Feet
    (103760, "+proj=lcc +lat_0=47.1505555555556 +lon_0=-96.45 +lat_1=47.2 +lat_2=47.45 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378468.623 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Norman_Feet
    (103761, "+proj=lcc +lat_0=43.8338888888889 +lon_0=-92.9166666666667 +lat_1=43.8833333333333 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378481.425 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Olmsted_Feet
    (103762, "+proj=lcc +lat_0=46.1063888888889 +lon_0=-95.7166666666667 +lat_1=46.1833333333333 +lat_2=46.65 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378525.621 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Ottertail_Feet
    (103763, "+proj=lcc +lat_0=47.4988888888889 +lon_0=-96.3666666666667 +lat_1=47.6 +lat_2=48.0833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378445.763 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Pennington_Feet
    (103764, "+proj=lcc +lat_0=45.73 +lon_0=-92.9 +lat_1=45.8166666666667 +lat_2=46.3333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378472.281 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Pine_Feet
    (103765, "+proj=lcc +lat_0=43.8491666666667 +lon_0=-96.25 +lat_1=43.8833333333333 +lat_2=44.15 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378670.401 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Pipestone_Feet
    (103766, "+proj=lcc +lat_0=47.4988888888889 +lon_0=-96.3666666666667 +lat_1=47.6 +lat_2=48.0833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378445.763 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Polk_Feet
    (103767, "+proj=lcc +lat_0=45.2827777777778 +lon_0=-95.15 +lat_1=45.35 +lat_2=45.7 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378502.761 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Pope_Feet
    (103768, "+proj=lcc +lat_0=44.7911111111111 +lon_0=-93.3833333333333 +lat_1=44.8833333333333 +lat_2=45.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378418.941 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Ramsey_Feet
    (103769, "+proj=lcc +lat_0=47.4988888888889 +lon_0=-96.3666666666667 +lat_1=47.6 +lat_2=48.0833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378445.763 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Red_Lake_Feet
    (103770, "+proj=lcc +lat_0=44.1947222222222 +lon_0=-95.2333333333333 +lat_1=44.2666666666667 +lat_2=44.5666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378438.753 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Redwood_Feet
    (103771, "+proj=lcc +lat_0=44.4561111111111 +lon_0=-94.6333333333333 +lat_1=44.5333333333333 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378414.369 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Renville_Feet
    (103772, "+proj=lcc +lat_0=44.1947222222222 +lon_0=-93.1333333333333 +lat_1=44.3 +lat_2=44.6666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378434.181 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Rice_Feet
    (103773, "+proj=lcc +lat_0=43.5 +lon_0=-95.95 +lat_1=43.5666666666667 +lat_2=43.8 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378624.681 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Rock_Feet
    (103774, "+proj=lcc +lat_0=48.5438888888889 +lon_0=-96.15 +lat_1=48.6 +lat_2=48.9333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378449.421 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Roseau_Feet
    (103775, "+proj=lcc +lat_0=47.8333333333333 +lon_0=-92.45 +lat_1=47.9833333333333 +lat_2=48.5333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378543.909 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_St_Louis_North_Feet
    (103776, "+proj=lcc +lat_0=47.25 +lon_0=-92.45 +lat_1=47.3333333333333 +lat_2=47.75 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378605.783 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_St_Louis_Central_Feet
    (103777, "+proj=lcc +lat_0=46.65 +lon_0=-92.45 +lat_1=46.7833333333333 +lat_2=47.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378540.861 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_St_Louis_South_Feet
    (103778, "+proj=lcc +lat_0=44.4719444444445 +lon_0=-93.3166666666667 +lat_1=44.5166666666667 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378421.989 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Scott_Feet
    (103779, "+proj=lcc +lat_0=44.9775 +lon_0=-93.8833333333333 +lat_1=45.0333333333333 +lat_2=45.4666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378443.325 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Sherburne_Feet
    (103780, "+proj=lcc +lat_0=44.4561111111111 +lon_0=-94.6333333333333 +lat_1=44.5333333333333 +lat_2=44.9166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378414.369 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Sibley_Feet
    (103781, "+proj=lcc +lat_0=45.2827777777778 +lon_0=-95.15 +lat_1=45.35 +lat_2=45.7 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378502.761 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Stearns_Feet
    (103782, "+proj=lcc +lat_0=43.8338888888889 +lon_0=-92.9166666666667 +lat_1=43.8833333333333 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378481.425 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Steele_Feet
    (103783, "+proj=lcc +lat_0=45.2827777777778 +lon_0=-95.15 +lat_1=45.35 +lat_2=45.7 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378502.761 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Stevens_Feet
    (103784, "+proj=lcc +lat_0=45.1522222222222 +lon_0=-96.05 +lat_1=45.2166666666667 +lat_2=45.5333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378470.757 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Swift_Feet
    (103785, "+proj=lcc +lat_0=45.7733333333333 +lon_0=-94.9 +lat_1=45.8666666666667 +lat_2=46.2833333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378548.481 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Todd_Feet
    (103786, "+proj=lcc +lat_0=45.5855555555556 +lon_0=-96.55 +lat_1=45.6333333333333 +lat_2=45.9666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378463.746 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Traverse_Feet
    (103787, "+proj=lcc +lat_0=44.1069444444444 +lon_0=-92.2666666666667 +lat_1=44.15 +lat_2=44.4166666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378426.561 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Wabasha_Feet
    (103788, "+proj=lcc +lat_0=46.1563888888889 +lon_0=-94.4666666666667 +lat_1=46.2666666666667 +lat_2=46.7333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378546.957 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Wadena_Feet
    (103789, "+proj=lcc +lat_0=43.8338888888889 +lon_0=-92.9166666666667 +lat_1=43.8833333333333 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378481.425 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Waseca_Feet
    (103790, "+proj=lcc +lat_0=43.8480555555556 +lon_0=-94.9166666666667 +lat_1=43.9 +lat_2=44.1666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378514.953 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Watonwan_Feet
    (103791, "+proj=lcc +lat_0=43.8472222222222 +lon_0=-91.6166666666667 +lat_1=43.9 +lat_2=44.1333333333333 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378453.688 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Winona_Feet
    (103792, "+proj=lcc +lat_0=44.9775 +lon_0=-93.8833333333333 +lat_1=45.0333333333333 +lat_2=45.4666666666667 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378443.325 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Wright_Feet
    (103793, "+proj=lcc +lat_0=44.5416666666667 +lon_0=-95.9 +lat_1=44.6666666666667 +lat_2=44.95 +x_0=152400.30480061 +y_0=30480.0609601219 +a=6378530.193 +rf=298.257222100883 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_MN_Yellow_Medicine_Feet
    (103794, "+proj=utm +zone=11 +ellps=GRS80 +units=m +no_defs"), // Mexican_Datum_1993_UTM_Zone_11N (deprecated)
    (103795, "+proj=utm +zone=12 +ellps=GRS80 +units=m +no_defs"), // Mexican_Datum_1993_UTM_Zone_12N (deprecated)
    (103796, "+proj=utm +zone=13 +ellps=GRS80 +units=m +no_defs"), // Mexican_Datum_1993_UTM_Zone_13N (deprecated)
    (103797, "+proj=utm +zone=14 +ellps=GRS80 +units=m +no_defs"), // Mexican_Datum_1993_UTM_Zone_14N (deprecated)
    (103798, "+proj=utm +zone=15 +ellps=GRS80 +units=m +no_defs"), // Mexican_Datum_1993_UTM_Zone_15N (deprecated)
    (103799, "+proj=utm +zone=16 +ellps=GRS80 +units=m +no_defs"), // Mexican_Datum_1993_UTM_Zone_16N (deprecated)
    (103800, "+proj=tmerc +lat_0=43.3666666666667 +lon_0=-90 +k=0.999999 +x_0=147218.694437389 +y_0=0 +a=6378376.271 +rf=298.268410995005 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Adams_Meters (deprecated)
    (103801, "+proj=tmerc +lat_0=45.7061111111111 +lon_0=-90.6222222222222 +k=0.999997 +x_0=172821.945643891 +y_0=0 +a=6378471.92 +rf=298.272883775229 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Ashland_Meters (deprecated)
    (103802, "+proj=tmerc +lat_0=45.1333333333333 +lon_0=-91.85 +k=0.999996 +x_0=93150 +y_0=0 +a=6378472.931 +rf=298.272931052052 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Barron_Meters (deprecated)
    (103803, "+proj=tmerc +lat_0=43 +lon_0=-88 +k=1.00002 +x_0=31599.99998984 +y_0=4599.98983997968 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Brown_Meters (deprecated)
    (103804, "+proj=tmerc +lat_0=43.4813888888889 +lon_0=-91.7972222222222 +k=1 +x_0=175260.350520701 +y_0=0 +a=6378380.991 +rf=298.268631713702 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Buffalo_Meters (deprecated)
    (103805, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=0.999996 +x_0=244754.889509779 +y_0=0 +a=6378345.09 +rf=298.266952895494 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Calumet_Meters (deprecated)
    (103806, "+proj=tmerc +lat_0=43.6 +lon_0=-90.7083333333333 +k=0.999994 +x_0=199949.1998984 +y_0=0 +a=6378470.401 +rf=298.272812743089 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Clark_Meters (deprecated)
    (103807, "+proj=tmerc +lat_0=41.4722222222222 +lon_0=-88.775 +k=0.999997 +x_0=263347.726695453 +y_0=0 +a=6378376.811 +rf=298.268436246721 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Dodge_Meters (deprecated)
    (103808, "+proj=tmerc +lat_0=44.4 +lon_0=-87.2722222222222 +k=0.999991 +x_0=158801.117602235 +y_0=0 +a=6378313.92 +rf=298.26549531037 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Door_Meters (deprecated)
    (103809, "+proj=tmerc +lat_0=45.8833333333333 +lon_0=-91.9166666666667 +k=0.999995 +x_0=59131.3182626365 +y_0=0 +a=6378414.93 +rf=298.270218784012 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Douglas_Meters (deprecated)
    (103810, "+proj=tmerc +lat_0=44.4083333333333 +lon_0=-91.8944444444444 +k=0.999998 +x_0=51816.1036322073 +y_0=0 +a=6378413.021 +rf=298.270129514522 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Dunn_Meters (deprecated)
    (103811, "+proj=tmerc +lat_0=45.4388888888889 +lon_0=-88.1416666666667 +k=0.999993 +x_0=133502.667005334 +y_0=0 +a=6378530.851 +rf=298.275639532334 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Florence_Meters (deprecated)
    (103812, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=0.999996 +x_0=244754.889509779 +y_0=0 +a=6378345.09 +rf=298.266952895494 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Fond_du_Lac_Meters (deprecated)
    (103813, "+proj=tmerc +lat_0=44.0055555555556 +lon_0=-88.6333333333333 +k=0.999996 +x_0=275844.551689103 +y_0=0 +a=6378591.521 +rf=298.278476609315 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Forest_Meters (deprecated)
    (103814, "+proj=tmerc +lat_0=41.4111111111111 +lon_0=-90.8 +k=0.999997 +x_0=242316.484632969 +y_0=0 +a=6378378.881 +rf=298.268533044963 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Grant_Meters (deprecated)
    (103815, "+proj=tmerc +lat_0=42.5388888888889 +lon_0=-90.1611111111111 +k=0.999997 +x_0=113081.026162052 +y_0=0 +a=6378408.041 +rf=298.269896637591 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Iowa_Meters (deprecated)
    (103816, "+proj=tmerc +lat_0=45.4333333333333 +lon_0=-90.2555555555556 +k=0.999996 +x_0=220980.441960884 +y_0=0 +a=6378655.071 +rf=298.281448362111 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Iron_Meters (deprecated)
    (103817, "+proj=tmerc +lat_0=41.4722222222222 +lon_0=-88.775 +k=0.999997 +x_0=263347.726695453 +y_0=0 +a=6378376.811 +rf=298.268436246721 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Jefferson_Meters (deprecated)
    (103818, "+proj=tmerc +lat_0=43.3666666666667 +lon_0=-90 +k=0.999999 +x_0=147218.694437389 +y_0=0 +a=6378376.271 +rf=298.268410995005 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Juneau_Meters (deprecated)
    (103819, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=0.999998 +x_0=185928.371856744 +y_0=0 +a=6378315.7 +rf=298.265578547505 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Kenosha_Meters (deprecated)
    (103820, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1 +x_0=79857.7597155194 +y_0=0 +a=6378285.86 +rf=298.264183156421 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Kewaunee_Meters (deprecated)
    (103821, "+proj=tmerc +lat_0=43.4511111111111 +lon_0=-91.3166666666667 +k=0.999994 +x_0=130454.660909322 +y_0=0 +a=6378379.301 +rf=298.268552685186 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_LaCrosse_Meters (deprecated)
    (103822, "+proj=tmerc +lat_0=44.8444444444444 +lon_0=-89.7333333333333 +k=0.999998 +x_0=116129.032258065 +y_0=0 +a=6378531.821 +rf=298.275684891897 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Lincoln_Meters (deprecated)
    (103823, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1 +x_0=79857.7597155194 +y_0=0 +a=6378285.86 +rf=298.264183156421 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Manitowoc_Meters (deprecated)
    (103824, "+proj=tmerc +lat_0=44.6916666666667 +lon_0=-87.7111111111111 +k=0.999986 +x_0=238658.877317755 +y_0=0 +a=6378376.041 +rf=298.268400239645 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Marinette_Meters (deprecated)
    (103825, "+proj=tmerc +lat_0=44.7166666666667 +lon_0=-88.4166666666667 +k=0.999994 +x_0=105461.010922022 +y_0=0 +a=6378406.601 +rf=298.269829299684 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Menominee_Meters (deprecated)
    (103826, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=0.999998 +x_0=185928.371856744 +y_0=0 +a=6378315.7 +rf=298.265578547505 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Milwaukee_Meters (deprecated)
    (103827, "+proj=tmerc +lat_0=44.3972222222222 +lon_0=-87.9083333333333 +k=0.999991 +x_0=182880.365760732 +y_0=0 +a=6378345.42 +rf=298.266968327098 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Oconto_Meters (deprecated)
    (103828, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=0.999996 +x_0=244754.889509779 +y_0=0 +a=6378345.09 +rf=298.266952895494 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Outagamie_Meters (deprecated)
    (103829, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=0.999998 +x_0=185928.371856744 +y_0=0 +a=6378315.7 +rf=298.265578547505 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Ozaukee_Meters (deprecated)
    (103830, "+proj=tmerc +lat_0=44.6611111111111 +lon_0=-92.6333333333333 +k=1 +x_0=141732.283464567 +y_0=0 +a=6378413.671 +rf=298.270159910105 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Polk_Meters (deprecated)
    (103831, "+proj=tmerc +lat_0=44.5555555555556 +lon_0=-90.4888888888889 +k=0.999998 +x_0=227990.855981712 +y_0=0 +a=6378563.891 +rf=298.277184563214 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Price_Meters (deprecated)
    (103832, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=0.999998 +x_0=185928.371856744 +y_0=0 +a=6378315.7 +rf=298.265578547505 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Racine_Meters (deprecated)
    (103833, "+proj=tmerc +lat_0=41.9444444444444 +lon_0=-89.0722222222222 +k=0.999996 +x_0=146304.292608585 +y_0=0 +a=6378377.671 +rf=298.268476462415 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Rock_Meters (deprecated)
    (103834, "+proj=tmerc +lat_0=43.9194444444444 +lon_0=-91.0666666666667 +k=0.999997 +x_0=250546.101092202 +y_0=0 +a=6378472.751 +rf=298.272922634813 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Rusk_Meters (deprecated)
    (103835, "+proj=tmerc +lat_0=44.0361111111111 +lon_0=-92.6333333333333 +k=0.999995 +x_0=165506.731013462 +y_0=0 +a=6378412.511 +rf=298.270105665679 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_St_Croix_Meters (deprecated)
    (103836, "+proj=tmerc +lat_0=42.8194444444444 +lon_0=-89.9 +k=0.999995 +x_0=185623.571247143 +y_0=0 +a=6378407.281 +rf=298.26986109814 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Sauk_Meters (deprecated)
    (103837, "+proj=tmerc +lat_0=44.0361111111111 +lon_0=-88.6055555555556 +k=0.99999 +x_0=262433.32486665 +y_0=0 +a=6378406.051 +rf=298.269803580344 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Shawano_Meters (deprecated)
    (103838, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1 +x_0=79857.7597155194 +y_0=0 +a=6378285.86 +rf=298.264183156421 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Sheboygan_Meters (deprecated)
    (103839, "+proj=tmerc +lat_0=43.1611111111111 +lon_0=-91.3666666666667 +k=0.999998 +x_0=256946.913893828 +y_0=0 +a=6378380.091 +rf=298.26858962751 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Trempealeau_Meters (deprecated)
    (103840, "+proj=tmerc +lat_0=42.9180555555555 +lon_0=-88.0638888888889 +k=0.999995 +x_0=120091.44018288 +y_0=0 +a=6378407.141 +rf=298.269854551399 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Washington_Meters (deprecated)
    (103841, "+proj=tmerc +lat_0=42.5694444444444 +lon_0=-88.225 +k=0.999997 +x_0=208788.417576835 +y_0=0 +a=6378376.871 +rf=298.268439052467 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Waukesha_Meters (deprecated)
    (103842, "+proj=tmerc +lat_0=43.4202777777778 +lon_0=-88.8166666666667 +k=0.999996 +x_0=185013.97002794 +y_0=0 +a=6378375.251 +rf=298.268363297321 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Waupaca_Meters (deprecated)
    (103843, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=0.999996 +x_0=244754.889509779 +y_0=0 +a=6378345.09 +rf=298.266952895494 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Winnebago_Meters (deprecated)
    (103844, "+proj=lcc +lat_0=45.3333333333333 +lon_0=-91.1527777777778 +lat_1=46.4138888888889 +lat_2=46.925 +x_0=228600.457200914 +y_0=0 +a=6378411.351 +rf=298.270051421254 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Bayfield_Meters (deprecated)
    (103845, "+proj=lcc +lat_0=45.3638888888889 +lon_0=-92.4577777777778 +lat_1=45.7138888888889 +lat_2=46.0833333333333 +x_0=64008.128016256 +y_0=0 +a=6378414.96 +rf=298.270220186885 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Burnett_Meters (deprecated)
    (103847, "+proj=lcc +lat_0=42.4583333333333 +lon_0=-89.3944444444444 +lat_1=43.3333333333333 +lat_2=43.5916666666667 +x_0=169164.338328677 +y_0=0 +a=6378376.331 +rf=298.268413800752 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Columbia_Meters (deprecated)
    (103848, "+proj=lcc +lat_0=42.7166666666667 +lon_0=-90.9388888888889 +lat_1=43.0583333333333 +lat_2=43.3416666666667 +x_0=113690.627381255 +y_0=0 +a=6378379.031 +rf=298.268540059328 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Crawford_Meters (deprecated)
    (103849, "+proj=lcc +lat_0=41.75 +lon_0=-89.4222222222222 +lat_1=42.9083333333333 +lat_2=43.2305555555555 +x_0=247193.294386589 +y_0=0 +a=6378407.621 +rf=298.269876997368 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Dane_Meters (deprecated)
    (103850, "+proj=lcc +lat_0=44.0472222222222 +lon_0=-91.2888888888889 +lat_1=44.7305555555555 +lat_2=45.0138888888889 +x_0=120091.44018288 +y_0=0 +a=6378380.381 +rf=298.268603188617 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_EauClaire_Meters (deprecated)
    (103851, "+proj=lcc +lat_0=42.225 +lon_0=-89.8388888888889 +lat_1=42.4861111111111 +lat_2=42.7888888888889 +x_0=170078.74015748 +y_0=0 +a=6378408.481 +rf=298.269917213063 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Green_Meters (deprecated)
    (103852, "+proj=lcc +lat_0=43.0944444444444 +lon_0=-89.2416666666667 +lat_1=43.6666666666667 +lat_2=43.9472222222222 +x_0=150876.301752603 +y_0=0 +a=6378375.601 +rf=298.268379664173 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_GreenLake_Meters (deprecated)
    (103853, "+proj=lcc +lat_0=43.7944444444444 +lon_0=-90.7388888888889 +lat_1=44.1638888888889 +lat_2=44.4194444444444 +x_0=125882.651765303 +y_0=0 +a=6378409.151 +rf=298.269948543895 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Jackson_Meters (deprecated)
    (103854, "+proj=lcc +lat_0=42.225 +lon_0=-89.8388888888889 +lat_1=42.4861111111111 +lat_2=42.7888888888889 +x_0=170078.74015748 +y_0=0 +a=6378408.481 +rf=298.269917213063 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Lafayette_Meters (deprecated)
    (103855, "+proj=lcc +lat_0=44.2069444444445 +lon_0=-89.0333333333333 +lat_1=45 +lat_2=45.3083333333333 +x_0=198425.196850394 +y_0=0 +a=6378560.121 +rf=298.277008268831 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Langlade_Meters (deprecated)
    (103856, "+proj=lcc +lat_0=44.4055555555556 +lon_0=-89.77 +lat_1=44.7452777777778 +lat_2=45.0563888888889 +x_0=74676.1493522987 +y_0=0 +a=6378500.6 +rf=298.274224921888 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Marathon_Meters (deprecated)
    (103857, "+proj=lcc +lat_0=43.0944444444444 +lon_0=-89.2416666666667 +lat_1=43.6666666666667 +lat_2=43.9472222222222 +x_0=150876.301752603 +y_0=0 +a=6378375.601 +rf=298.268379664173 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Marquette_Meters (deprecated)
    (103858, "+proj=lcc +lat_0=42.9027777777778 +lon_0=-90.6416666666667 +lat_1=43.8388888888889 +lat_2=44.1611111111111 +x_0=204521.209042418 +y_0=0 +a=6378438.991 +rf=298.27134393498 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Monroe_Meters (deprecated)
    (103859, "+proj=lcc +lat_0=45.1861111111111 +lon_0=-89.5444444444444 +lat_1=45.5666666666667 +lat_2=45.8416666666667 +x_0=70104.1402082804 +y_0=0 +a=6378593.86 +rf=298.278585986653 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Oneida_Meters (deprecated)
    (103860, "+proj=lcc +lat_0=43.8619444444444 +lon_0=-92.2277777777778 +lat_1=44.5222222222222 +lat_2=44.75 +x_0=167640.335280671 +y_0=0 +a=6378381.271 +rf=298.268644807185 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Pepin_Meters (deprecated)
    (103861, "+proj=lcc +lat_0=43.8619444444444 +lon_0=-92.2277777777778 +lat_1=44.5222222222222 +lat_2=44.75 +x_0=167640.335280671 +y_0=0 +a=6378381.271 +rf=298.268644807185 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Pierce_Meters (deprecated)
    (103862, "+proj=lcc +lat_0=43.9666666666667 +lon_0=-89.5 +lat_1=44.1833333333333 +lat_2=44.65 +x_0=56388.1127762256 +y_0=0 +a=6378344.377 +rf=298.266919538913 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Portage_Meters (deprecated)
    (103863, "+proj=lcc +lat_0=42.1138888888889 +lon_0=-90.4305555555556 +lat_1=43.1416666666667 +lat_2=43.5027777777778 +x_0=202387.60477521 +y_0=0 +a=6378408.091 +rf=298.269898975713 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Richland_Meters (deprecated)
    (103864, "+proj=lcc +lat_0=44.8138888888889 +lon_0=-91.1166666666667 +lat_1=45.7194444444444 +lat_2=46.0805555555556 +x_0=216713.233426467 +y_0=0 +a=6378534.451 +rf=298.275807877103 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Sawyer_Meters (deprecated)
    (103865, "+proj=lcc +lat_0=44.2083333333333 +lon_0=-90.4833333333333 +lat_1=45.0555555555556 +lat_2=45.3 +x_0=187147.574295149 +y_0=0 +a=6378532.921 +rf=298.275736330576 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Taylor_Meters (deprecated)
    (103866, "+proj=lcc +lat_0=43.1472222222222 +lon_0=-90.7833333333333 +lat_1=43.4666666666667 +lat_2=43.6833333333333 +x_0=222504.44500889 +y_0=0 +a=6378408.941 +rf=298.269938723784 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Vernon_Meters (deprecated)
    (103867, "+proj=lcc +lat_0=45.625 +lon_0=-89.4888888888889 +lat_1=45.9305555555556 +lat_2=46.225 +x_0=134417.068834138 +y_0=0 +a=6378624.171 +rf=298.280003402845 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Vilas_Meters (deprecated)
    (103868, "+proj=lcc +lat_0=41.6694444444444 +lon_0=-88.5416666666667 +lat_1=42.5888888888889 +lat_2=42.75 +x_0=232562.86512573 +y_0=0 +a=6378377.411 +rf=298.268464304182 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Walworth_Meters (deprecated)
    (103869, "+proj=lcc +lat_0=44.2666666666667 +lon_0=-91.7833333333333 +lat_1=45.7722222222222 +lat_2=46.15 +x_0=234086.868173736 +y_0=0 +a=6378474.591 +rf=298.273008677695 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Washburn_Meters (deprecated)
    (103870, "+proj=lcc +lat_0=43.7083333333333 +lon_0=-89.2416666666667 +lat_1=43.975 +lat_2=44.2527777777778 +x_0=120091.44018288 +y_0=0 +a=6378405.971 +rf=298.269799839349 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Waushara_Meters (deprecated)
    (103871, "+proj=lcc +lat_0=43.1513888888889 +lon_0=-90 +lat_1=44.1805555555556 +lat_2=44.5444444444444 +x_0=208483.616967234 +y_0=0 +a=6378437.651 +rf=298.271281273316 +units=m +no_defs"), // NAD_1983_HARN_Adj_WI_Wood_Meters (deprecated)
    (103872, "+proj=lcc +lat_0=-20.2755163 +lon_0=57.5645566222222 +lat_1=-20.0611111111111 +lat_2=-20.4444444444444 +x_0=250000 +y_0=350000 +ellps=GRS80 +units=m +no_defs"), // GDM2008_LGM2012
    (103873, "+proj=utm +zone=15 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(CSRS)_v6_UTM_Zone_15N (deprecated)
    (103874, "+proj=utm +zone=16 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(CSRS)_v6_UTM_Zone_16N (deprecated)
    (103875, "+proj=utm +zone=17 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(CSRS)_v6_UTM_Zone_17N (deprecated)
    (103876, "+proj=utm +zone=18 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_(CSRS)_v6_UTM_Zone_18N (deprecated)
    (103888, "+proj=tmerc +lat_0=0 +lon_0=15 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo15_(E-N)
    (103889, "+proj=tmerc +lat_0=0 +lon_0=17 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo17_(E-N)
    (103890, "+proj=tmerc +lat_0=0 +lon_0=33 +k=1 +x_0=0 +y_0=0 +ellps=WGS84 +units=m +no_defs"), // Hartebeesthoek94_Lo33_(E-N)
    (103891, "+proj=lcc +lat_0=39.3333333333333 +lon_0=-122 +lat_1=40 +lat_2=41.6666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // California_SRS_Epoch_2017.50_(NAD83)_StatePlane_CA_I_US_Feet
    (103892, "+proj=lcc +lat_0=37.6666666666667 +lon_0=-122 +lat_1=38.3333333333333 +lat_2=39.8333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // California_SRS_Epoch_2017.50_(NAD83)_StatePlane_CA_II_US_Feet
    (103893, "+proj=lcc +lat_0=36.5 +lon_0=-120.5 +lat_1=37.0666666666667 +lat_2=38.4333333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // California_SRS_Epoch_2017.50_(NAD83)_StatePlane_CA_III_US_Feet
    (103894, "+proj=lcc +lat_0=35.3333333333333 +lon_0=-119 +lat_1=36 +lat_2=37.25 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // California_SRS_Epoch_2017.50_(NAD83)_StatePlane_CA_IV_US_Feet
    (103895, "+proj=lcc +lat_0=33.5 +lon_0=-118 +lat_1=34.0333333333333 +lat_2=35.4666666666667 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // California_SRS_Epoch_2017.50_(NAD83)_StatePlane_CA_V_US_Feet
    (103896, "+proj=lcc +lat_0=32.1666666666667 +lon_0=-116.25 +lat_1=32.7833333333333 +lat_2=33.8833333333333 +x_0=2000000 +y_0=500000 +ellps=GRS80 +units=us-ft +no_defs"), // California_SRS_Epoch_2017.50_(NAD83)_StatePlane_CA_VI_US_Feet
    (103897, "+proj=tmerc +lat_0=41.1833333333333 +lon_0=-96.05 +k=1.0000482 +x_0=40000 +y_0=25000 +ellps=GRS80 +units=ft +no_defs"), // NAD_1983_2011_Nebraska_LDP_Douglas-Sarpy_County_Ft_Intl
    (103900, "+proj=tmerc +lat_0=43.3666666666667 +lon_0=-90 +k=0.999999 +x_0=147218.694437389 +y_0=0 +a=6378376.271 +rf=298.268410995005 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Adams_Feet (deprecated)
    (103901, "+proj=tmerc +lat_0=45.7061111111111 +lon_0=-90.6222222222222 +k=0.999997 +x_0=172821.945643891 +y_0=0 +a=6378471.92 +rf=298.272883775229 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Ashland_Feet (deprecated)
    (103902, "+proj=tmerc +lat_0=45.1333333333333 +lon_0=-91.85 +k=0.999996 +x_0=93150 +y_0=0 +a=6378472.931 +rf=298.272931052052 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Barron_Feet (deprecated)
    (103903, "+proj=tmerc +lat_0=43 +lon_0=-88 +k=1.00002 +x_0=31599.99998984 +y_0=4599.98983997968 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Brown_Feet (deprecated)
    (103904, "+proj=tmerc +lat_0=43.4813888888889 +lon_0=-91.7972222222222 +k=1 +x_0=175260.350520701 +y_0=0 +a=6378380.991 +rf=298.268631713702 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Buffalo_Feet (deprecated)
    (103905, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=0.999996 +x_0=244754.889509779 +y_0=0 +a=6378345.09 +rf=298.266952895494 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Calumet_Feet (deprecated)
    (103906, "+proj=tmerc +lat_0=43.6 +lon_0=-90.7083333333333 +k=0.999994 +x_0=199949.1998984 +y_0=0 +a=6378470.401 +rf=298.272812743089 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Clark_Feet (deprecated)
    (103907, "+proj=tmerc +lat_0=41.4722222222222 +lon_0=-88.775 +k=0.999997 +x_0=263347.726695453 +y_0=0 +a=6378376.811 +rf=298.268436246721 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Dodge_Feet (deprecated)
    (103908, "+proj=tmerc +lat_0=44.4 +lon_0=-87.2722222222222 +k=0.999991 +x_0=158801.117602235 +y_0=0 +a=6378313.92 +rf=298.26549531037 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Door_Feet (deprecated)
    (103909, "+proj=tmerc +lat_0=45.8833333333333 +lon_0=-91.9166666666667 +k=0.999995 +x_0=59131.3182626365 +y_0=0 +a=6378414.93 +rf=298.270218784012 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Douglas_Feet (deprecated)
    (103910, "+proj=tmerc +lat_0=44.4083333333333 +lon_0=-91.8944444444444 +k=0.999998 +x_0=51816.1036322073 +y_0=0 +a=6378413.021 +rf=298.270129514522 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Dunn_Feet (deprecated)
    (103911, "+proj=tmerc +lat_0=45.4388888888889 +lon_0=-88.1416666666667 +k=0.999993 +x_0=133502.667005334 +y_0=0 +a=6378530.851 +rf=298.275639532334 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Florence_Feet (deprecated)
    (103912, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=0.999996 +x_0=244754.889509779 +y_0=0 +a=6378345.09 +rf=298.266952895494 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Fond_du_Lac_Feet (deprecated)
    (103913, "+proj=tmerc +lat_0=44.0055555555556 +lon_0=-88.6333333333333 +k=0.999996 +x_0=275844.551689103 +y_0=0 +a=6378591.521 +rf=298.278476609315 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Forest_Feet (deprecated)
    (103914, "+proj=tmerc +lat_0=41.4111111111111 +lon_0=-90.8 +k=0.999997 +x_0=242316.484632969 +y_0=0 +a=6378378.881 +rf=298.268533044963 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Grant_Feet (deprecated)
    (103915, "+proj=tmerc +lat_0=42.5388888888889 +lon_0=-90.1611111111111 +k=0.999997 +x_0=113081.026162052 +y_0=0 +a=6378408.041 +rf=298.269896637591 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Iowa_Feet (deprecated)
    (103916, "+proj=tmerc +lat_0=45.4333333333333 +lon_0=-90.2555555555556 +k=0.999996 +x_0=220980.441960884 +y_0=0 +a=6378655.071 +rf=298.281448362111 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Iron_Feet (deprecated)
    (103917, "+proj=tmerc +lat_0=41.4722222222222 +lon_0=-88.775 +k=0.999997 +x_0=263347.726695453 +y_0=0 +a=6378376.811 +rf=298.268436246721 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Jefferson_Feet (deprecated)
    (103918, "+proj=tmerc +lat_0=43.3666666666667 +lon_0=-90 +k=0.999999 +x_0=147218.694437389 +y_0=0 +a=6378376.271 +rf=298.268410995005 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Juneau_Feet (deprecated)
    (103919, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=0.999998 +x_0=185928.371856744 +y_0=0 +a=6378315.7 +rf=298.265578547505 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Kenosha_Feet (deprecated)
    (103920, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1 +x_0=79857.7597155194 +y_0=0 +a=6378285.86 +rf=298.264183156421 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Kewaunee_Feet (deprecated)
    (103921, "+proj=tmerc +lat_0=43.4511111111111 +lon_0=-91.3166666666667 +k=0.999994 +x_0=130454.660909322 +y_0=0 +a=6378379.301 +rf=298.268552685186 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_LaCrosse_Feet (deprecated)
    (103922, "+proj=tmerc +lat_0=44.8444444444444 +lon_0=-89.7333333333333 +k=0.999998 +x_0=116129.032258065 +y_0=0 +a=6378531.821 +rf=298.275684891897 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Lincoln_Feet (deprecated)
    (103923, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1 +x_0=79857.7597155194 +y_0=0 +a=6378285.86 +rf=298.264183156421 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Manitowoc_Feet (deprecated)
    (103924, "+proj=tmerc +lat_0=44.6916666666667 +lon_0=-87.7111111111111 +k=0.999986 +x_0=238658.877317755 +y_0=0 +a=6378376.041 +rf=298.268400239645 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Marinette_Feet (deprecated)
    (103925, "+proj=tmerc +lat_0=44.7166666666667 +lon_0=-88.4166666666667 +k=0.999994 +x_0=105461.010922022 +y_0=0 +a=6378406.601 +rf=298.269829299684 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Menominee_Feet (deprecated)
    (103926, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=0.999998 +x_0=185928.371856744 +y_0=0 +a=6378315.7 +rf=298.265578547505 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Milwaukee_Feet (deprecated)
    (103927, "+proj=tmerc +lat_0=44.3972222222222 +lon_0=-87.9083333333333 +k=0.999991 +x_0=182880.365760732 +y_0=0 +a=6378345.42 +rf=298.266968327098 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Oconto_Feet (deprecated)
    (103928, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=0.999996 +x_0=244754.889509779 +y_0=0 +a=6378345.09 +rf=298.266952895494 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Outagamie_Feet (deprecated)
    (103929, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=0.999998 +x_0=185928.371856744 +y_0=0 +a=6378315.7 +rf=298.265578547505 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Ozaukee_Feet (deprecated)
    (103930, "+proj=tmerc +lat_0=44.6611111111111 +lon_0=-92.6333333333333 +k=1 +x_0=141732.283464567 +y_0=0 +a=6378413.671 +rf=298.270159910105 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Polk_Feet (deprecated)
    (103931, "+proj=tmerc +lat_0=44.5555555555556 +lon_0=-90.4888888888889 +k=0.999998 +x_0=227990.855981712 +y_0=0 +a=6378563.891 +rf=298.277184563214 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Price_Feet (deprecated)
    (103932, "+proj=tmerc +lat_0=42.2166666666667 +lon_0=-87.8944444444444 +k=0.999998 +x_0=185928.371856744 +y_0=0 +a=6378315.7 +rf=298.265578547505 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Racine_Feet (deprecated)
    (103933, "+proj=tmerc +lat_0=41.9444444444444 +lon_0=-89.0722222222222 +k=0.999996 +x_0=146304.292608585 +y_0=0 +a=6378377.671 +rf=298.268476462415 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Rock_Feet (deprecated)
    (103934, "+proj=tmerc +lat_0=43.9194444444444 +lon_0=-91.0666666666667 +k=0.999997 +x_0=250546.101092202 +y_0=0 +a=6378472.751 +rf=298.272922634813 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Rusk_Feet (deprecated)
    (103935, "+proj=tmerc +lat_0=44.0361111111111 +lon_0=-92.6333333333333 +k=0.999995 +x_0=165506.731013462 +y_0=0 +a=6378412.511 +rf=298.270105665679 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_St_Croix_Feet (deprecated)
    (103936, "+proj=tmerc +lat_0=42.8194444444444 +lon_0=-89.9 +k=0.999995 +x_0=185623.571247143 +y_0=0 +a=6378407.281 +rf=298.26986109814 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Sauk_Feet (deprecated)
    (103937, "+proj=tmerc +lat_0=44.0361111111111 +lon_0=-88.6055555555556 +k=0.99999 +x_0=262433.32486665 +y_0=0 +a=6378406.051 +rf=298.269803580344 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Shawano_Feet (deprecated)
    (103938, "+proj=tmerc +lat_0=43.2666666666667 +lon_0=-87.55 +k=1 +x_0=79857.7597155194 +y_0=0 +a=6378285.86 +rf=298.264183156421 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Sheboygan_Feet (deprecated)
    (103939, "+proj=tmerc +lat_0=43.1611111111111 +lon_0=-91.3666666666667 +k=0.999998 +x_0=256946.913893828 +y_0=0 +a=6378380.091 +rf=298.26858962751 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Trempealeau_Feet (deprecated)
    (103940, "+proj=tmerc +lat_0=42.9180555555555 +lon_0=-88.0638888888889 +k=0.999995 +x_0=120091.44018288 +y_0=0 +a=6378407.141 +rf=298.269854551399 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Washington_Feet (deprecated)
    (103941, "+proj=tmerc +lat_0=42.5694444444444 +lon_0=-88.225 +k=0.999997 +x_0=208788.417576835 +y_0=0 +a=6378376.871 +rf=298.268439052467 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Waukesha_Feet (deprecated)
    (103942, "+proj=tmerc +lat_0=43.4202777777778 +lon_0=-88.8166666666667 +k=0.999996 +x_0=185013.97002794 +y_0=0 +a=6378375.251 +rf=298.268363297321 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Waupaca_Feet (deprecated)
    (103943, "+proj=tmerc +lat_0=42.7194444444444 +lon_0=-88.5 +k=0.999996 +x_0=244754.889509779 +y_0=0 +a=6378345.09 +rf=298.266952895494 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Winnebago_Feet (deprecated)
    (103944, "+proj=lcc +lat_0=45.3333333333333 +lon_0=-91.1527777777778 +lat_1=46.4138888888889 +lat_2=46.925 +x_0=228600.457200914 +y_0=0 +a=6378411.351 +rf=298.270051421254 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Bayfield_Feet (deprecated)
    (103945, "+proj=lcc +lat_0=45.3638888888889 +lon_0=-92.4577777777778 +lat_1=45.7138888888889 +lat_2=46.0833333333333 +x_0=64008.128016256 +y_0=0 +a=6378414.96 +rf=298.270220186885 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Burnett_Feet (deprecated)
    (103947, "+proj=lcc +lat_0=42.4583333333333 +lon_0=-89.3944444444444 +lat_1=43.3333333333333 +lat_2=43.5916666666667 +x_0=169164.338328677 +y_0=0 +a=6378376.331 +rf=298.268413800752 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Columbia_Feet (deprecated)
    (103948, "+proj=lcc +lat_0=42.7166666666667 +lon_0=-90.9388888888889 +lat_1=43.0583333333333 +lat_2=43.3416666666667 +x_0=113690.627381255 +y_0=0 +a=6378379.031 +rf=298.268540059328 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Crawford_Feet (deprecated)
    (103949, "+proj=lcc +lat_0=41.75 +lon_0=-89.4222222222222 +lat_1=42.9083333333333 +lat_2=43.2305555555555 +x_0=247193.294386589 +y_0=0 +a=6378407.621 +rf=298.269876997368 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Dane_Feet (deprecated)
    (103950, "+proj=lcc +lat_0=44.0472222222222 +lon_0=-91.2888888888889 +lat_1=44.7305555555555 +lat_2=45.0138888888889 +x_0=120091.44018288 +y_0=0 +a=6378380.381 +rf=298.268603188617 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_EauClaire_Feet (deprecated)
    (103951, "+proj=lcc +lat_0=42.225 +lon_0=-89.8388888888889 +lat_1=42.4861111111111 +lat_2=42.7888888888889 +x_0=170078.74015748 +y_0=0 +a=6378408.481 +rf=298.269917213063 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Green_Feet (deprecated)
    (103952, "+proj=lcc +lat_0=43.0944444444444 +lon_0=-89.2416666666667 +lat_1=43.6666666666667 +lat_2=43.9472222222222 +x_0=150876.301752604 +y_0=0 +a=6378375.601 +rf=298.268379664173 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_GreenLake_Feet (deprecated)
    (103953, "+proj=lcc +lat_0=43.7944444444444 +lon_0=-90.7388888888889 +lat_1=44.1638888888889 +lat_2=44.4194444444444 +x_0=125882.651765304 +y_0=0 +a=6378409.151 +rf=298.269948543895 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Jackson_Feet (deprecated)
    (103954, "+proj=lcc +lat_0=42.225 +lon_0=-89.8388888888889 +lat_1=42.4861111111111 +lat_2=42.7888888888889 +x_0=170078.74015748 +y_0=0 +a=6378408.481 +rf=298.269917213063 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Lafayette_Feet (deprecated)
    (103955, "+proj=lcc +lat_0=44.2069444444445 +lon_0=-89.0333333333333 +lat_1=45 +lat_2=45.3083333333333 +x_0=198425.196850394 +y_0=0 +a=6378560.121 +rf=298.277008268831 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Langlade_Feet (deprecated)
    (103956, "+proj=lcc +lat_0=44.4055555555556 +lon_0=-89.77 +lat_1=44.7452777777778 +lat_2=45.0563888888889 +x_0=74676.1493522987 +y_0=0 +a=6378500.6 +rf=298.274224921888 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Marathon_Feet (deprecated)
    (103957, "+proj=lcc +lat_0=43.0944444444444 +lon_0=-89.2416666666667 +lat_1=43.6666666666667 +lat_2=43.9472222222222 +x_0=150876.301752604 +y_0=0 +a=6378375.601 +rf=298.268379664173 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Marquette_Feet (deprecated)
    (103958, "+proj=lcc +lat_0=42.9027777777778 +lon_0=-90.6416666666667 +lat_1=43.8388888888889 +lat_2=44.1611111111111 +x_0=204521.209042418 +y_0=0 +a=6378438.991 +rf=298.27134393498 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Monroe_Feet (deprecated)
    (103959, "+proj=lcc +lat_0=45.1861111111111 +lon_0=-89.5444444444444 +lat_1=45.5666666666667 +lat_2=45.8416666666667 +x_0=70104.1402082804 +y_0=0 +a=6378593.86 +rf=298.278585986653 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Oneida_Feet (deprecated)
    (103960, "+proj=lcc +lat_0=43.8619444444444 +lon_0=-92.2277777777778 +lat_1=44.5222222222222 +lat_2=44.75 +x_0=167640.335280671 +y_0=0 +a=6378381.271 +rf=298.268644807185 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Pepin_Feet (deprecated)
    (103961, "+proj=lcc +lat_0=43.8619444444444 +lon_0=-92.2277777777778 +lat_1=44.5222222222222 +lat_2=44.75 +x_0=167640.335280671 +y_0=0 +a=6378381.271 +rf=298.268644807185 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Pierce_Feet (deprecated)
    (103962, "+proj=lcc +lat_0=43.9666666666667 +lon_0=-89.5 +lat_1=44.1833333333333 +lat_2=44.65 +x_0=56388.1127762256 +y_0=0 +a=6378344.377 +rf=298.266919538913 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Portage_Feet (deprecated)
    (103963, "+proj=lcc +lat_0=42.1138888888889 +lon_0=-90.4305555555556 +lat_1=43.1416666666667 +lat_2=43.5027777777778 +x_0=202387.60477521 +y_0=0 +a=6378408.091 +rf=298.269898975713 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Richland_Feet (deprecated)
    (103964, "+proj=lcc +lat_0=44.8138888888889 +lon_0=-91.1166666666667 +lat_1=45.7194444444444 +lat_2=46.0805555555556 +x_0=216713.233426467 +y_0=0 +a=6378534.451 +rf=298.275807877103 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Sawyer_Feet (deprecated)
    (103965, "+proj=lcc +lat_0=44.2083333333333 +lon_0=-90.4833333333333 +lat_1=45.0555555555556 +lat_2=45.3 +x_0=187147.574295149 +y_0=0 +a=6378532.921 +rf=298.275736330576 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Taylor_Feet (deprecated)
    (103966, "+proj=lcc +lat_0=43.1472222222222 +lon_0=-90.7833333333333 +lat_1=43.4666666666667 +lat_2=43.6833333333333 +x_0=222504.44500889 +y_0=0 +a=6378408.941 +rf=298.269938723784 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Vernon_Feet (deprecated)
    (103967, "+proj=lcc +lat_0=45.625 +lon_0=-89.4888888888889 +lat_1=45.9305555555556 +lat_2=46.225 +x_0=134417.068834138 +y_0=0 +a=6378624.171 +rf=298.280003402845 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Vilas_Feet (deprecated)
    (103968, "+proj=lcc +lat_0=41.6694444444444 +lon_0=-88.5416666666667 +lat_1=42.5888888888889 +lat_2=42.75 +x_0=232562.86512573 +y_0=0 +a=6378377.411 +rf=298.268464304182 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Walworth_Feet (deprecated)
    (103969, "+proj=lcc +lat_0=44.2666666666667 +lon_0=-91.7833333333333 +lat_1=45.7722222222222 +lat_2=46.15 +x_0=234086.868173736 +y_0=0 +a=6378474.591 +rf=298.273008677695 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Washburn_Feet (deprecated)
    (103970, "+proj=lcc +lat_0=43.7083333333333 +lon_0=-89.2416666666667 +lat_1=43.975 +lat_2=44.2527777777778 +x_0=120091.44018288 +y_0=0 +a=6378405.971 +rf=298.269799839349 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Waushara_Feet (deprecated)
    (103971, "+proj=lcc +lat_0=43.1513888888889 +lon_0=-90 +lat_1=44.1805555555556 +lat_2=44.5444444444444 +x_0=208483.616967234 +y_0=0 +a=6378437.651 +rf=298.271281273316 +units=us-ft +no_defs"), // NAD_1983_HARN_Adj_WI_Wood_Feet (deprecated)
    (103972, "+proj=tmerc +lat_0=0 +lon_0=3 +k=1 +x_0=1500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_1
    (103973, "+proj=tmerc +lat_0=0 +lon_0=6 +k=1 +x_0=2500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_2
    (103974, "+proj=tmerc +lat_0=0 +lon_0=9 +k=1 +x_0=3500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_3
    (103975, "+proj=tmerc +lat_0=0 +lon_0=12 +k=1 +x_0=4500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_4
    (103976, "+proj=tmerc +lat_0=0 +lon_0=15 +k=1 +x_0=5500000 +y_0=0 +ellps=bessel +towgs84=598.1,73.7,418.2,0.202,0.045,-2.455,6.7 +units=m +no_defs"), // Germany_Zone_5
    (103977, "+proj=tmerc +lat_0=0 +lon_0=12 +k=0.9996 +x_0=500000 +y_0=500000 +datum=WGS84 +units=m +no_defs"), // WGS_1984_UTM_GTM_2010
    (103978, "+proj=aea +lat_0=40 +lon_0=-107.5 +lat_1=42 +lat_2=44 +x_0=0 +y_0=0 +datum=NAD83 +units=us-ft +no_defs"), // NAD_1983_WYDOT_Albers_(ftUS)
    (103979, "+proj=aea +lat_0=40 +lon_0=-107.5 +lat_1=42 +lat_2=44 +x_0=0 +y_0=0 +datum=NAD83 +units=m +no_defs"), // NAD_1983_WYDOT_Albers_(m)
    (103980, "+proj=aea +lat_0=40 +lon_0=-107.5 +lat_1=42 +lat_2=44 +x_0=0 +y_0=0 +ellps=GRS80 +units=us-ft +no_defs"), // NAD_1983_2011_WYDOT_Albers_(ftUS)
    (103981, "+proj=aea +lat_0=40 +lon_0=-107.5 +lat_1=42 +lat_2=44 +x_0=0 +y_0=0 +ellps=GRS80 +units=m +no_defs"), // NAD_1983_2011_WYDOT_Albers_(m)
    (112000, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999260028821883 +x_0=261910.558703157 +y_0=70975.7620913136 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A01H1
    (112001, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999313972680282 +x_0=252927.284354225 +y_0=70979.593633004 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A02H1
    (112002, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99936492500576 +x_0=243942.308403333 +y_0=70983.2126911338 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A03H1
    (112003, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999389910065824 +x_0=243948.407189748 +y_0=70984.9873380003 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A03H2
    (112004, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999414883310307 +x_0=234956.181256649 +y_0=70986.7611456259 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A04H1
    (112005, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999464846609941 +x_0=225969.155631124 +y_0=70990.3099549106 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A05H1
    (112006, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99948983416814 +x_0=225974.805071869 +y_0=70992.0847792154 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A05H2
    (112007, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999515814322282 +x_0=215981.533843434 +y_0=70993.9301059435 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A06H1
    (112008, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999540803154722 +x_0=215986.93358427 +y_0=70995.7050207555 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A06H2
    (112009, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999563788682652 +x_0=205992.275418417 +y_0=70997.3376442117 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A07H1
    (112010, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999588778714495 +x_0=205997.425418427 +y_0=70999.1126442153 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A07H2
    (112011, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999612767259319 +x_0=196002.254028371 +y_0=71000.8165102774 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A08H1
    (112012, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999637758515672 +x_0=196007.154268481 +y_0=71002.5915972559 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A08H2
    (112013, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999661750636116 +x_0=185011.193066174 +y_0=71004.2957172883 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A09H1
    (112014, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.9996867431171 +x_0=185015.818519455 +y_0=71006.0708912503 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A09H2
    (112015, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999712738433191 +x_0=173019.291436884 +y_0=71007.9172949061 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A10H1
    (112016, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999737732188917 +x_0=173023.617081381 +y_0=71009.6925594109 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A10H2
    (112017, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999766731181957 +x_0=159026.318551582 +y_0=71011.7523091972 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A11H1
    (112018, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999791726287553 +x_0=159030.294358639 +y_0=71013.5276695809 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A11H2
    (112019, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999816722642982 +x_0=159034.270364498 +y_0=71015.3031187382 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A11H3
    (112020, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999816729668208 +x_0=144031.03832328 +y_0=71015.3036177283 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A12H1
    (112021, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999841726023814 +x_0=144034.639234272 +y_0=71017.0790668981 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A12H2
    (112022, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999866723629314 +x_0=144038.240325321 +y_0=71018.8546048459 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A12H3
    (112023, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999864732920049 +x_0=128033.736487859 +y_0=71018.7132081091 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A13H1
    (112024, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99988973047578 +x_0=128036.937451307 +y_0=71020.4887425219 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A13H2
    (112025, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999913740994384 +x_0=111034.697926722 +y_0=71022.1941693447 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A14H1
    (112026, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999938739775363 +x_0=111037.473898269 +y_0=71023.9697907847 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A14H2
    (112027, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999966755136649 +x_0=88032.1753716564 +y_0=71025.9596748591 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A15H1
    (112028, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999991755243032 +x_0=88034.3762585739 +y_0=71027.7353904403 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A15H2
    (112029, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00001177155717 +x_0=54022.1758344197 +y_0=71029.1571156259 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A16H1
    (112030, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000036772789 +x_0=54023.5264394632 +y_0=71030.9329111461 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A16H2
    (112031, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999980759810936 +x_0=-24009.1113476146 +y_0=71026.9544033599 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A17H1
    (112032, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00000576026745 +x_0=-24009.7115979077 +y_0=71028.7301438103 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A17H2
    (112033, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99992774418376 +x_0=-58018.9429571512 +y_0=71023.1887923747 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A18H1
    (112034, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999952743314832 +x_0=-58020.3934851199 +y_0=71024.9644386812 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A18H2
    (112035, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999873734042937 +x_0=-88023.9862501752 +y_0=71019.352542755 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A19H1
    (112036, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999898731823705 +x_0=-88026.1869323571 +y_0=71021.1280931517 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A19H2
    (112037, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99982572992683 +x_0=-105023.57751484 +y_0=71015.9428909868 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A20H1
    (112038, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99985072650745 +x_0=-105026.203202741 +y_0=71017.718356139 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A20H2
    (112039, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999770730876833 +x_0=-122020.682266178 +y_0=71012.0364008088 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A21H1
    (112040, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999795726082426 +x_0=-122023.732897634 +y_0=71013.8117682951 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A21H2
    (112041, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999707739399589 +x_0=-139014.804913381 +y_0=71007.5622219427 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A22H1
    (112042, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999732733030336 +x_0=-139018.280413835 +y_0=71009.3374775704 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A22H2
    (112043, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999655752413687 +x_0=-156008.502381475 +y_0=71003.8696736202 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A23H1
    (112044, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99968074474471 +x_0=-156012.402740298 +y_0=71005.6448369306 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A23H2
    (112045, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999612767259319 +x_0=-165001.897523884 +y_0=71000.8165102774 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A24H1
    (112046, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999637758515672 +x_0=-165006.022726017 +y_0=71002.5915972559 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A24H2
    (112047, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999564788197464 +x_0=-175993.57634226 +y_0=70997.4086380706 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A25H1
    (112048, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999589778254296 +x_0=-175997.976346668 +y_0=70999.1836398491 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A25H2
    (112049, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999516813741153 +x_0=-185984.284553461 +y_0=70994.0010929879 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A26H1
    (112050, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999541802598579 +x_0=-185988.934334942 +y_0=70995.7760095746 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A26H2
    (112051, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999466845245835 +x_0=-195973.641905265 +y_0=70990.4519146624 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A27H1
    (112052, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999491832854001 +x_0=-195978.541430045 +y_0=70992.2267425163 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A27H2
    (112053, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999415882527346 +x_0=-205961.794596858 +y_0=70986.8321183345 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A28H1
    (112054, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999440868861396 +x_0=-205966.943834819 +y_0=70988.6068556901 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A28H2
    (112055, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999365924122903 +x_0=-214949.380087333 +y_0=70983.283656747 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A29H1
    (112056, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999313972680282 +x_0=-223935.619349196 +y_0=70979.593633004 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_A30H1
    (112057, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999966755136649 +x_0=88032.1753716564 +y_0=111040.584843794 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B15H1
    (112058, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999991755243032 +x_0=88034.3762585739 +y_0=111043.360962519 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B15H2
    (112059, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.0000167565995 +x_0=88036.5772555427 +y_0=111046.13722006 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B15H3
    (112060, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00001177155717 +x_0=54022.1758344197 +y_0=111045.58365964 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B16H1
    (112061, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000036772789 +x_0=54023.5264394632 +y_0=111048.359903341 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B16H2
    (112062, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00006177527098 +x_0=54024.8771120413 +y_0=111051.136285863 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B16H3
    (112063, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00008677900318 +x_0=54026.2278521588 +y_0=111053.912807215 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B16H4
    (112064, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999980759810936 +x_0=-24009.1113476146 +y_0=111042.139982718 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B17H1
    (112065, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00000576026745 +x_0=-24009.7115979077 +y_0=111044.916140323 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B17H2
    (112066, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99992774418376 +x_0=-58018.9429571512 +y_0=111036.252900755 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B18H1
    (112067, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999952743314832 +x_0=-58020.3934851199 +y_0=111039.028911178 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B18H2
    (112068, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999873734042937 +x_0=-88023.9862501752 +y_0=111030.255383744 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B19H1
    (112069, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99982572992683 +x_0=-105023.57751484 +y_0=111024.924801402 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B20H1
    (112070, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999770730876833 +x_0=-122020.682266178 +y_0=111018.817471687 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B21H1
    (112071, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99970873920231 +x_0=-139014.943941393 +y_0=111011.933651041 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B22H1
    (112072, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999655752413687 +x_0=-156008.502381475 +y_0=111006.049771434 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B23H1
    (112073, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999613766872086 +x_0=-165002.062525782 +y_0=111001.387517344 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B24H1
    (112074, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999564788197464 +x_0=-175993.57634226 +y_0=110995.948715857 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B25H1
    (112075, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999516813741153 +x_0=-185984.284553461 +y_0=110990.621427066 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B26H1
    (112076, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999466845245835 +x_0=-195973.641905265 +y_0=110985.072711655 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B27H1
    (112077, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999415882527346 +x_0=-205961.794596858 +y_0=110979.413593452 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B28H1
    (112078, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999366923242045 +x_0=-214949.594983633 +y_0=110973.976945039 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B29H1
    (112079, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999314971695549 +x_0=-223935.843217878 +y_0=110968.208023145 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B30H1
    (112080, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999261027729297 +x_0=-232920.691532296 +y_0=110962.217854441 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B31H1
    (112081, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999206090787045 +x_0=-241904.32807196 +y_0=110956.117421436 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_B32H1
    (112082, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999865733036811 +x_0=128033.864553868 +y_0=126033.335420214 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C13H1
    (112083, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999913740994384 +x_0=111034.697926722 +y_0=126039.386835739 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C14H1
    (112084, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999938739775363 +x_0=111037.473898269 +y_0=126042.537938576 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C14H2
    (112085, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99996373980636 +x_0=111040.250008623 +y_0=126045.689198978 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C14H3
    (112086, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999988741087467 +x_0=111043.026257795 +y_0=126048.840616957 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C14H4
    (112087, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999966755136649 +x_0=88032.1753716564 +y_0=126046.069282144 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C15H1
    (112088, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999991755243032 +x_0=88034.3762585739 +y_0=126049.220552049 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C15H2
    (112089, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.0000167565995 +x_0=88036.5772555427 +y_0=126052.371979527 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C15H3
    (112090, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00004175920614 +x_0=88038.778362571 +y_0=126055.52356459 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C15H4
    (112091, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00006676306306 +x_0=88040.9795796669 +y_0=126058.67530725 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C15H5
    (112092, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00001177155717 +x_0=54022.1758344197 +y_0=126051.743613646 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C16H1
    (112093, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.000036772789 +x_0=54023.5264394632 +y_0=126054.895025414 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C16H2
    (112094, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00006177527098 +x_0=54024.8771120413 +y_0=126058.046594763 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C16H3
    (112095, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00008677900318 +x_0=54026.2278521588 +y_0=126061.198321704 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C16H4
    (112096, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999980759810936 +x_0=-24009.1113476146 +y_0=126047.834574977 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C17H1
    (112097, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=1.00000576026745 +x_0=-24009.7115979077 +y_0=126050.985889015 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C17H2
    (112098, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.99992774418376 +x_0=-58018.9429571512 +y_0=126041.151941397 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C18H1
    (112099, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999952743314832 +x_0=-58020.3934851199 +y_0=126044.303088364 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C18H2
    (112100, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999873734042937 +x_0=-88023.9862501752 +y_0=126034.343949114 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C19H1
    (112101, "+proj=tmerc +lat_0=49 +lon_0=-2 +k=0.999898731823705 +x_0=-88026.1869323571 +y_0=126037.494925875 +ellps=airy +towgs84=446.448,-125.157,542.06,0.15,0.247,0.842,-20.489 +units=m +no_defs"), // OSGB36_National_Highways_C19H2
];
