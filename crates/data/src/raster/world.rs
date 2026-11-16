//! World files: the six numbers, in a small text file beside a plain picture (a TIFF without
//! GeoTIFF tags, a PNG, a JPEG), that place it on the ground (#2075). As Esri documents them
//! ("World files for raster datasets"), the lines are, in order:
//!
//! 1. A: the pixel's size in x;
//! 2. D: rotation, how far y moves per column;
//! 3. B: rotation, how far x moves per row;
//! 4. E: the pixel's size in y, negative for a picture with north up;
//! 5. C: x of the center of the top-left pixel;
//! 6. F: y of the center of the top-left pixel.
//!
//! So a pixel center (column, row) is at `x = A·col + B·row + C`, `y = D·col + E·row + F`. A world
//! file names no coordinate system: that comes from the `.prj` beside the picture.
//!
//! Its name is the picture's with the extension's first and last letters and a `w` (`.tfw` for
//! `.tif`, `.jgw` for `.jpg`, `.pgw` for `.png`), the extension and a `w` (`.tifw`, `.jpgw`), or
//! `.wld`.

use std::path::{Path, PathBuf};

/// A world file is six numbers: a larger file isn't one.
const MAX_BYTES: u64 = 4096;

/// The six numbers of a world file, as written.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorldFile {
    pub a: f64,
    pub d: f64,
    pub b: f64,
    pub e: f64,
    pub c: f64,
    pub f: f64,
}

impl WorldFile {
    /// Reads the six numbers: one a line (blank lines and spaces around them are passed over).
    /// Anything else, a number that isn't finite, or pixels of no area, is refused, saying why.
    pub fn parse(text: &str) -> Result<WorldFile, String> {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.len() != 6 {
            return Err(format!("a world file holds six numbers, this one {}", words.len()));
        }
        let mut n = [0.0f64; 6];
        for (slot, word) in n.iter_mut().zip(&words) {
            *slot = word.parse::<f64>().ok().filter(|v| v.is_finite()).ok_or_else(|| format!("{word:?} isn't a number"))?;
        }
        let [a, d, b, e, c, f] = n;
        let w = WorldFile { a, d, b, e, c, f };
        if !(a * e - b * d).is_normal() {
            return Err("its pixels have no area (the pixel sizes and rotation cancel out)".into());
        }
        Ok(w)
    }

    /// The transform of pixel corners, in GDAL's order: x of the top-left corner, x per column,
    /// x per row, y of the top-left corner, y per column, y per row. The corner is half a pixel
    /// up and left of the center the file gives.
    pub fn affine(&self) -> [f64; 6] {
        let WorldFile { a, d, b, e, c, f } = *self;
        [c - (a + b) / 2.0, a, b, f - (d + e) / 2.0, d, e]
    }

    /// Whether it places the picture north up, unrotated and unmirrored: rows run south, columns
    /// east.
    pub fn is_north_up(&self) -> bool {
        self.b == 0.0 && self.d == 0.0 && self.a > 0.0 && self.e < 0.0
    }
}

/// The names a world file for `path` may have, most specific first: `.tfw`, `.tifw`, `.wld` for
/// a `.tif`, each in lower and upper case (`.TFW`), for file systems that tell them apart.
pub fn names(path: &Path) -> Vec<PathBuf> {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return Vec::new() };
    let lower = ext.to_ascii_lowercase();
    let mut chars = lower.chars();
    let mut out = Vec::new();
    if let (Some(first), Some(last)) = (chars.next(), lower.chars().last()) {
        out.push(format!("{first}{last}w"));
    }
    out.push(format!("{lower}w"));
    out.push("wld".to_string());
    let upper: Vec<String> = out.iter().map(|e| e.to_ascii_uppercase()).collect();
    out.extend(upper);
    out.into_iter().map(|e| path.with_extension(e)).collect()
}

/// The most entries of a folder looked through for a world file named in mixed case.
const MAX_LISTED: usize = 100_000;

/// The world file beside `path`, if there is one: its path and what reading it gave. Each name
/// there is tried in turn ([`names`]), so a stray empty `.tfw` doesn't hide a good `.wld`; when
/// none can be read, the first one's reason is given. When no name is there as written, the
/// folder is looked through for one in any case (`Scan.Tfw`, from a system that ignores case).
pub fn beside(path: &Path) -> Option<(PathBuf, Result<WorldFile, String>)> {
    let mut there: Vec<(PathBuf, u64)> = names(path)
        .into_iter()
        .filter_map(|p| match std::fs::metadata(&p) {
            Ok(m) if m.is_file() => Some((p, m.len())),
            _ => None,
        })
        .collect();
    if there.is_empty() {
        there = in_any_case(path);
    }
    let mut first_failed = None;
    for (found, len) in there {
        let read = if len > MAX_BYTES {
            Err("it is too large to be a world file".to_string())
        } else {
            crate::provenance::read_to_string(&found).map_err(|e| e.to_string()).and_then(|t| WorldFile::parse(&t))
        };
        match read {
            Ok(wf) => return Some((found, Ok(wf))),
            Err(e) => {
                first_failed.get_or_insert((found, Err(e)));
            }
        }
    }
    first_failed
}

/// World files beside `path` whose names match [`names`] in any case, in that order: looked for in
/// its folder, at most [`MAX_LISTED`] entries of it.
fn in_any_case(path: &Path) -> Vec<(PathBuf, u64)> {
    let wanted: Vec<String> =
        names(path).iter().filter_map(|p| p.file_name().and_then(|n| n.to_str())).map(str::to_ascii_lowercase).collect();
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(usize, PathBuf, u64)> = entries
        .take(MAX_LISTED)
        .filter_map(Result::ok)
        .filter_map(|e| {
            let at = wanted.iter().position(|w| e.file_name().to_str().is_some_and(|n| n.to_ascii_lowercase() == *w))?;
            let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
            Some((at, e.path(), meta.len()))
        })
        .collect();
    found.sort_by_key(|(at, ..)| *at);
    found.into_iter().map(|(_, p, len)| (p, len)).collect()
}

/// Whether the picture at `path` is placed by a world file: one beside it can be read, a `.prj`
/// beside it says a coordinate system RustGIS uses, and together they lay the picture (its size
/// read from its header, not its pixels) somewhere on Earth. A JPEG so placed opens as a raster,
/// not as a photo; a stray world file, one without a usable `.prj`, or one whose numbers belong
/// to another system leaves it a photo.
pub fn places(path: &Path) -> bool {
    // Only a regular file's header is read: a FIFO or a device named like a picture isn't opened.
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Some((_, Ok(wf))) = beside(path) else { return false };
    let Ok((Some(crs), _)) = super::sidecar_crs(path) else { return false };
    image::image_dimensions(path).is_ok_and(|(w, h)| super::lands_on_earth(&wf, crs, w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_six_numbers_and_gives_the_corner() {
        let w = WorldFile::parse("2.0\n0.0\n0.0\n-2.0\n101.0\n499.0\n").unwrap();
        assert!(w.is_north_up());
        // The top-left pixel's center is (101, 499): its corner is a pixel's half up and left.
        assert_eq!(w.affine(), [100.0, 2.0, 0.0, 500.0, 0.0, -2.0]);
        // CRLF, spaces and a blank line are passed over.
        assert_eq!(WorldFile::parse(" 2\r\n0\r\n\r\n0\r\n-2\r\n101\r\n499\r\n").unwrap(), w);
        // Rotated and mirrored ones aren't north up.
        assert!(!WorldFile::parse("2 0.5 0.5 -2 0 0").unwrap().is_north_up());
        assert!(!WorldFile::parse("2 0 0 2 0 0").unwrap().is_north_up());
    }

    #[test]
    fn junk_is_refused_saying_why() {
        assert!(WorldFile::parse("1 0 0 -1 0").unwrap_err().contains("six numbers"));
        assert!(WorldFile::parse("1 0 0 -1 0 0 7").unwrap_err().contains("six numbers"));
        assert!(WorldFile::parse("1 0 0 -1 x 0").unwrap_err().contains("\"x\""));
        assert!(WorldFile::parse("1 0 0 -1 inf 0").unwrap_err().contains("isn't a number"));
        assert!(WorldFile::parse("0 0 0 -1 0 0").unwrap_err().contains("no area"));
        assert!(WorldFile::parse("1 1 1 1 0 0").unwrap_err().contains("no area"));
        assert!(WorldFile::parse("").is_err());
    }

    #[test]
    fn names_follow_the_picture() {
        let ext = |p: &str| names(Path::new(p)).iter().map(|n| n.extension().unwrap().to_str().unwrap().to_string()).collect::<Vec<_>>();
        assert_eq!(ext("a.tif"), ["tfw", "tifw", "wld", "TFW", "TIFW", "WLD"]);
        assert_eq!(ext("a.JPG"), ["jgw", "jpgw", "wld", "JGW", "JPGW", "WLD"]);
        assert_eq!(ext("a.jpeg"), ["jgw", "jpegw", "wld", "JGW", "JPEGW", "WLD"]);
        assert_eq!(ext("a.png")[0], "pgw");
        assert!(names(Path::new("noext")).is_empty());
    }

    #[test]
    fn found_beside_and_too_large_refused() {
        let dir = std::env::temp_dir().join(format!("rustgis-world-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let pic = dir.join("p.png");
        assert!(beside(&pic).is_none() && !places(&pic));
        std::fs::write(dir.join("p.wld"), "1\n0\n0\n-1\n0.5\n9.5\n").unwrap();
        let (found, read) = beside(&pic).unwrap();
        assert_eq!(found.extension().unwrap(), "wld");
        assert!(read.unwrap().is_north_up());
        // A specific name that can be read wins over `.wld`.
        std::fs::write(dir.join("p.pgw"), "2\n0\n0\n-2\n1\n9\n").unwrap();
        let (found, read) = beside(&pic).unwrap();
        assert_eq!((found.extension().unwrap().to_str(), read.unwrap().a), (Some("pgw"), 2.0));
        // One that can't be read doesn't hide the `.wld`.
        std::fs::write(dir.join("p.pgw"), "x").unwrap();
        let (found, read) = beside(&pic).unwrap();
        assert_eq!(found.extension().unwrap(), "wld");
        assert!(read.is_ok());
        // With none readable, the first one's reason is given.
        std::fs::remove_file(dir.join("p.wld")).unwrap();
        std::fs::write(dir.join("p.pgw"), "1 ".repeat(3000)).unwrap();
        assert!(beside(&pic).unwrap().1.unwrap_err().contains("too large"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_world_file_named_in_mixed_case_is_found() {
        let dir = std::env::temp_dir().join(format!("rustgis-world-mixed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Scan.Tfw"), "1\n0\n0\n-1\n0.5\n9.5\n").unwrap();
        // Found by name where the file system ignores case (macOS, Windows), by looking through the
        // folder where it doesn't (Linux).
        let (found, read) = beside(&dir.join("Scan.Tif")).unwrap();
        assert_eq!(found.file_name().unwrap().to_ascii_lowercase(), "scan.tfw");
        assert!(read.is_ok());
        assert!(beside(&dir.join("Other.tif")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// WGS 84 longitude/latitude, as an Esri `.prj` writes it.
    const PRJ: &str = r#"GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]]"#;

    /// A folder for one test, emptied, with a 4 × 2 gray PNG `p.png` in it.
    fn picture(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("rustgis-world-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        image::GrayImage::from_fn(4, 2, |x, y| image::Luma([(x + 4 * y) as u8 * 10])).save(dir.join("p.png")).unwrap();
        dir
    }

    #[test]
    fn a_picture_is_placed_by_its_world_file_and_prj() {
        let dir = picture("placed");
        // Half-degree pixels, the top-left pixel's center at (10.25, 49.75): its corner at (10, 50).
        std::fs::write(dir.join("p.pgw"), "0.5\n0\n0\n-0.5\n10.25\n49.75\n").unwrap();
        std::fs::write(dir.join("p.prj"), PRJ).unwrap();
        let r = crate::raster::read_path(&dir.join("p.png")).unwrap();
        assert!(r.georeferenced && r.lost.is_empty(), "{:?}", r.lost);
        let t = r.transform;
        assert_eq!((t.origin_x, t.origin_y, t.pixel_width, t.pixel_height), (10.0, 50.0, 0.5, -0.5));
        let e = r.extent();
        assert!((e.min_x - 10.0).abs() < 1e-9 && (e.max_x - 12.0).abs() < 1e-9 && (e.min_y - 49.0).abs() < 1e-9, "{e:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_a_prj_or_with_a_bad_world_file_it_stays_a_scan_and_says_why() {
        let dir = picture("scan");
        std::fs::write(dir.join("p.pgw"), "0.5\n0\n0\n-0.5\n10.25\n49.75\n").unwrap();
        let r = crate::raster::read_path(&dir.join("p.png")).unwrap();
        assert!(!r.georeferenced);
        assert!(r.lost.iter().any(|m| m.contains("p.pgw") && m.contains("no .prj")), "{:?}", r.lost);

        std::fs::write(dir.join("p.pgw"), "0.5 0 0 -0.5 x 49.75").unwrap();
        std::fs::write(dir.join("p.prj"), PRJ).unwrap();
        let r = crate::raster::read_path(&dir.join("p.png")).unwrap();
        assert!(!r.georeferenced);
        assert!(r.lost.iter().any(|m| m.contains("can't be read") && m.contains("without coordinates")), "{:?}", r.lost);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_turned_world_file_is_resampled_north_up() {
        let dir = picture("turned");
        // Turned a quarter: columns run south, rows run west. The 4 × 2 picture lies 2 wide, 4 tall.
        std::fs::write(dir.join("p.wld"), "0\n-1\n-1\n0\n11.5\n49.5\n").unwrap();
        std::fs::write(dir.join("p.prj"), PRJ).unwrap();
        let r = crate::raster::read_path(&dir.join("p.png")).unwrap();
        assert!(r.georeferenced, "{:?}", r.lost);
        assert_eq!((r.width, r.height), (2, 4));
        let e = r.extent();
        assert!((e.min_x - 10.0).abs() < 1e-9 && (e.max_x - 12.0).abs() < 1e-9, "{e:?}");
        assert!((e.min_y - 46.0).abs() < 1e-9 && (e.max_y - 50.0).abs() < 1e-9, "{e:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_world_file_that_cant_place_it_leaves_a_scan_never_an_error() {
        let dir = picture("fallback");
        // UTM numbers in a file whose .prj says longitude/latitude: nowhere to draw it.
        std::fs::write(dir.join("p.pgw"), "1\n0\n0\n-1\n500000\n7000000\n").unwrap();
        std::fs::write(dir.join("p.prj"), PRJ).unwrap();
        let r = crate::raster::read_path(&dir.join("p.png")).unwrap();
        assert!(!r.georeferenced && (r.width, r.height) == (4, 2));
        // Nor does it count as placing the picture (a camera JPEG with it stays a photo).
        assert!(!places(&dir.join("p.png")));
        assert!(
            r.lost.iter().any(|m| m.contains("p.pgw") && m.contains("isn't used") && m.contains("without coordinates")),
            "{:?}",
            r.lost
        );
        // A .prj RustGIS refuses (another body's system it can't read): still a scan, said.
        std::fs::write(dir.join("p.pgw"), "0.5\n0\n0\n-0.5\n10.25\n49.75\n").unwrap();
        std::fs::write(
            dir.join("p.prj"),
            r#"PROJCS["Mars_X",GEOGCS["Mars 2000",DATUM["D_Mars_2000",SPHEROID["Mars_2000_IAU_IAG",3396190.0,169.8944472]],PRIMEM["Reference_Meridian",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Hotine_Oblique_Mercator"],UNIT["Meter",1.0]]"#,
        )
        .unwrap();
        let r = crate::raster::read_path(&dir.join("p.png")).unwrap();
        assert!(!r.georeferenced, "{:?}", r.lost);
        assert!(r.lost.iter().any(|m| m.contains("p.pgw")), "{:?}", r.lost);
        // Nor does that .prj make the picture one a world file places (a JPEG stays a photo).
        assert!(!places(&dir.join("p.png")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_all_caps_set_is_placed() {
        let dir = picture("caps");
        std::fs::rename(dir.join("p.png"), dir.join("P.PNG")).unwrap();
        std::fs::write(dir.join("P.PGW"), "0.5\n0\n0\n-0.5\n10.25\n49.75\n").unwrap();
        std::fs::write(dir.join("P.PRJ"), PRJ).unwrap();
        let r = crate::raster::read_path(&dir.join("P.PNG")).unwrap();
        assert!(r.georeferenced, "{:?}", r.lost);
        assert!(places(&dir.join("P.PNG")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_projected_world_file_is_placed_in_its_prj_system() {
        let dir = picture("utm");
        // UTM zone 33N meters: 10 m pixels, the top-left pixel's center at (500005, 5299995).
        std::fs::write(dir.join("p.pgw"), "10\n0\n0\n-10\n500005\n5299995\n").unwrap();
        std::fs::write(
            dir.join("p.prj"),
            r#"PROJCS["WGS_1984_UTM_Zone_33N",GEOGCS["GCS_WGS_1984",DATUM["D_WGS_1984",SPHEROID["WGS_1984",6378137.0,298.257223563]],PRIMEM["Greenwich",0.0],UNIT["Degree",0.0174532925199433]],PROJECTION["Transverse_Mercator"],PARAMETER["False_Easting",500000.0],PARAMETER["False_Northing",0.0],PARAMETER["Central_Meridian",15.0],PARAMETER["Scale_Factor",0.9996],PARAMETER["Latitude_Of_Origin",0.0],UNIT["Meter",1.0]]"#,
        )
        .unwrap();
        let r = crate::raster::read_path(&dir.join("p.png")).unwrap();
        assert!(r.georeferenced, "{:?}", r.lost);
        assert_eq!((r.transform.origin_x, r.transform.origin_y), (500000.0, 5300000.0));
        // On the central meridian, about 47.8° N.
        let e = r.extent();
        assert!((e.min_x - 15.0).abs() < 0.01 && (e.max_y - 47.84).abs() < 0.05, "{e:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_unplaced_the_world_file_is_passed_over() {
        let dir = picture("unplaced");
        std::fs::write(dir.join("p.wld"), "0\n-1\n-1\n0\n11.5\n49.5\n").unwrap();
        std::fs::write(dir.join("p.prj"), PRJ).unwrap();
        let r = crate::raster::read_path_unplaced(&dir.join("p.png"), None, None).unwrap();
        // The file's pixels, not turned, and nothing said of a world file it wasn't asked to use.
        assert!(!r.georeferenced && (r.width, r.height) == (4, 2) && r.lost.is_empty(), "{:?}", r.lost);
        assert!(places(&dir.join("p.png")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_picture_read_at_every_second_pixel_keeps_exact_pixel_sizes() {
        // 101 × 3 read at every second pixel: 51 × 2 pixels, each exactly two of the file's (not
        // 101/51 of them).
        let mut png = Vec::new();
        image::GrayImage::from_pixel(101, 3, image::Luma([9]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let r = super::super::read_scan_limited("p", std::io::Cursor::new(png), 300).unwrap();
        assert_eq!((r.width, r.height, r.step, r.overview), (51, 2, 2, Some((101, 3))));
        let wf = WorldFile::parse("1\n0\n0\n-1\n0.5\n9.5\n").unwrap();
        let r = r.placed_by_world_file(&wf, crate::raster::Crs::LonLat).map_err(|(_, e)| e).unwrap();
        assert_eq!((r.transform.origin_x, r.transform.origin_y, r.transform.pixel_width, r.transform.pixel_height), (0.0, 10.0, 2.0, -2.0));
    }

    #[test]
    fn a_turned_scan_keeps_its_bands_classes_and_names() {
        let dir = picture("classes");
        let mut r = crate::raster::read_path_unplaced(&dir.join("p.png"), None, None).unwrap();
        r.bands[0].name = "landcover".into();
        r.bands[0].classes = vec!["water".into(), "forest".into()];
        let wf = WorldFile::parse("0\n-1\n-1\n0\n11.5\n49.5\n").unwrap();
        let r = r.placed_by_world_file(&wf, crate::raster::Crs::LonLat).map_err(|(_, e)| e).unwrap();
        assert_eq!((r.width, r.height), (2, 4));
        assert_eq!((r.bands[0].name.as_str(), r.bands[0].classes.len()), ("landcover", 2));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_geotiff_keeps_its_tags_and_says_when_a_world_file_differs() {
        let dir = picture("tagged");
        std::fs::write(dir.join("p.pgw"), "0.5\n0\n0\n-0.5\n10.25\n49.75\n").unwrap();
        std::fs::write(dir.join("p.prj"), PRJ).unwrap();
        let placed = crate::raster::read_path(&dir.join("p.png")).unwrap();
        let tif = dir.join("t.tif");
        crate::raster::cogwrite::write_file(&placed, 0, &tif).unwrap();
        // A world file that agrees says nothing.
        std::fs::write(dir.join("t.tfw"), "0.5\n0\n0\n-0.5\n10.25\n49.75\n").unwrap();
        let r = crate::raster::read_path(&tif).unwrap();
        assert!(r.georeferenced && r.lost.iter().all(|m| !m.contains("t.tfw")), "{:?}", r.lost);
        // One that doesn't is said, and the tags are used.
        std::fs::write(dir.join("t.tfw"), "0.5\n0\n0\n-0.5\n20.25\n49.75\n").unwrap();
        let r = crate::raster::read_path(&tif).unwrap();
        assert_eq!(r.transform.origin_x, 10.0);
        assert!(r.lost.iter().any(|m| m.contains("t.tfw") && m.contains("tags, which are used")), "{:?}", r.lost);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
