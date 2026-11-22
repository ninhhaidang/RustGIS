//! Raster layers: a shared raster, where it came from, and how its pixels are coloured.
//!
//! Renderers follow the raster layer's Symbology gallery: Stretch (a colour ramp over one band),
//! RGB (three bands as red, green and blue), Classify (class breaks), Unique Values (one colour per
//! whole-number value), Colormap (the file's colour table) and Shaded Relief (hillshade tinted by
//! elevation). Stretch types, dynamic range adjustment (statistics from the current view),
//! resampling and brightness/contrast/gamma apply on top.

use std::sync::{Arc, Mutex, Weak};

use rustgis_data::raster::{BandStats, ColorHint, Raster};
use serde::{Deserialize, Serialize};
use web_time::Instant;

use crate::color::{Ramp, Rgba};
use crate::symbology::{ClassMethod, class_breaks, fmt_num};

/// A raster layer's data source and style.
#[derive(Clone, Serialize, Deserialize)]
pub struct RasterLayer {
    /// The file it was read from; reopened when the project loads.
    #[serde(default)]
    pub path: Option<String>,
    /// `path` is a cloud-optimised GeoTIFF's web address, read in place: the layer holds an
    /// overview of it (its statistics, histograms and tools read that), and the map reads the part
    /// in view as it draws (see [`set_in_place_reader`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub in_place: bool,
    /// The file's own name (for its format) when it is embedded.
    #[serde(default)]
    pub file_name: Option<String>,
    /// The original file, kept in the project when there is no path (a browser upload).
    #[serde(default, skip_serializing_if = "Option::is_none", with = "b64")]
    pub embedded: Option<Arc<Vec<u8>>>,
    /// For a raster GDAL reads: the subdataset added (a NetCDF variable), so it is read again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdataset: Option<String>,
    /// The coordinate system given for a file or store that doesn't say: for a raster GDAL reads
    /// (applied when it's read again from its path), and for a Zarr slice (which its series and
    /// summaries read the store with; a slice kept as its GeoTIFF's bytes has it written in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crs: Option<String>,
    /// A Zarr variable that follows the time slider: its slice is the step the slider shows,
    /// read by the engine as the slider moves (opengeos/RustGIS#554).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_zarr: Option<ZarrTime>,
    /// For a scan with no coordinates: the longitude/latitude box (west, south, east, north) it
    /// was laid over when added, so it is laid there again when the project opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placement: Option<[f64; 4]>,
    pub style: RasterStyle,
    /// The pixels; `None` when the source couldn't be read.
    #[serde(skip)]
    pub data: Option<Arc<Raster>>,
    /// The last reload failure; transient, like the decoded pixels.
    #[serde(skip)]
    pub error: Option<String>,
    /// Statistics of the pixels the style's mask leaves, worked out on the first draw.
    #[serde(skip)]
    masked: MaskedStats,
    /// The light its cast shadows leave and the sky it sees, worked out off the frame (see
    /// [`RasterLayer::cast_shadows`] and [`RasterLayer::sky_view`]).
    #[serde(skip)]
    cast: CastLight,
    /// A remote reload shared with render snapshots, never serialized.
    #[serde(skip)]
    opening: Option<RasterRead>,
    /// The initiating read's runtime lifetime, retained through queued application and retry.
    #[serde(skip)]
    read_still: Option<rustgis_data::source::remote::Still>,
    /// Process-local opening identity; render and undo copies retain it, new reads do not.
    #[serde(skip, default = "next_read_generation")]
    read_generation: u64,
    /// The style is the file's own look once its pixels land (a layer added before they have;
    /// saved with a project saved before they did, so it opens in that look still).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    restyle: bool,
    /// The look its address gave a layer read in place, applied when its pixels land (saved, as
    /// `restyle` is, until they have).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    look: Option<AddressLook>,
    /// What reading its pixels lost has been said (see [`RasterLayer::pixels_unsaid`]).
    #[serde(skip)]
    pixels_said: bool,
    /// Why it couldn't be read in place has been said (see [`RasterLayer::error_unsaid`]).
    #[serde(skip)]
    error_said: bool,
    /// When reading its overview in place failed, for the read to be made again once its wait is
    /// over (see [`RasterLayer::read_failure`]).
    #[serde(skip)]
    failed_at: Option<Instant>,
    /// How many reads of its overview in place have failed in a row: each waits longer than the
    /// last (see [`RasterLayer::read_failure`]).
    #[serde(skip)]
    failures: u32,
    /// What reading its pixels left out (`Raster::lost`) has been said (see
    /// [`RasterLayer::lost_unsaid`]).
    #[serde(skip)]
    lost_said: bool,
    /// What its address's look asked for that the file hasn't (bands past its own), said with
    /// what the pixels lost.
    #[serde(skip)]
    look_lost: Option<String>,
    /// Settings of the tile layer this one was opened from that a raster layer hasn't (see
    /// [`RasterLayer::dropped_unsaid`]).
    #[serde(skip)]
    dropped: Vec<&'static str>,
}

/// Called once with a cloud-optimised GeoTIFF's overview, or why it couldn't be read.
pub type InPlaceRead = Box<dyn FnOnce(Result<Raster, String>) + Send>;

/// Reads the overviews of `layers` read in place and not read yet, all at once: a project of
/// several opens in the time of the slowest, not of them all. Waited for natively when `wait`
/// (at most two minutes in all); otherwise, as always in the browser, they land as the map draws
/// (see [`wait_for_reads`]). Other layers are left as they are. Returns each failure, by its
/// place in `layers`: without waiting, the reads that couldn't start.
pub fn read_together(layers: &mut [&mut RasterLayer], wait: bool) -> Vec<(usize, String)> {
    let mut failed = Vec::new();
    let unread = |l: &RasterLayer| l.in_place && l.data.is_none() && l.opening.is_none() && l.time_zarr.is_none();
    for (i, l) in layers.iter_mut().enumerate() {
        // Started without waiting: waited for below, together.
        if unread(l)
            && let Err(e) = l.begin_reload()
        {
            failed.push((i, e));
        }
    }
    if wait {
        failed.extend(wait_for_reads(layers));
    }
    failed
}

/// Waits (natively; at most two minutes for all) for the overviews of `layers` being read in
/// place (see [`read_together`]), installing them. Returns each failure, by its place in
/// `layers`. In the browser, where nothing may wait, they land as the map draws.
pub fn wait_for_reads(layers: &mut [&mut RasterLayer]) -> Vec<(usize, String)> {
    #[cfg(target_arch = "wasm32")]
    {
        let _ = layers;
        Vec::new()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let deadline = Instant::now() + rustgis_data::raster::IN_PLACE_WAIT;
        let mut failed = Vec::new();
        for (i, l) in layers.iter_mut().enumerate() {
            if l.in_place
                && l.opening.is_some()
                && let Err(e) = l.wait_for_read(deadline)
            {
                failed.push((i, e));
            }
        }
        failed
    }
}
type InPlaceReader = Arc<dyn Fn(&str, InPlaceRead) + Send + Sync>;

fn in_place_reader() -> &'static Mutex<Option<InPlaceReader>> {
    static R: std::sync::OnceLock<Mutex<Option<InPlaceReader>>> = std::sync::OnceLock::new();
    R.get_or_init(Mutex::default)
}

/// Sets how a cloud-optimised GeoTIFF's overview is read by its address (the tile cache's reader,
/// which checks addresses and signs requests; this crate does no networking).
pub fn set_in_place_reader(f: impl Fn(&str, InPlaceRead) + Send + Sync + 'static) {
    *in_place_reader().lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(f));
}

/// The renderer bands' statistics without the masked pixels, and what they were worked out for:
/// the mask, the renderer's bands and the pixels (held weakly, so their address isn't reused).
#[derive(Clone, Default)]
struct MaskedStats(Arc<Mutex<Option<(RasterMask, Vec<usize>, Weak<Raster>, Arc<Vec<BandStats>>)>>>);

type RasterRead = Arc<Mutex<Option<Result<Arc<Raster>, String>>>>;

impl std::fmt::Debug for RasterLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RasterLayer")
            .field("path", &self.path)
            .field("style", &self.style)
            .field("data", &self.data)
            .finish_non_exhaustive()
    }
}

impl RasterLayer {
    /// Whether a captured opening still owns the live source and any deferred address look.
    pub fn same_opening(&self, other: &Self) -> bool {
        self.read_generation == other.read_generation && self.restyle == other.restyle && self.look == other.look
    }

    /// Detaches a fully read replacement from the prior opening's pending reads and notices.
    pub fn reset_replacement_read(&mut self) {
        self.read_generation = next_read_generation();
        self.masked = MaskedStats::default();
        self.cast = CastLight::default();
        self.opening = None;
        self.read_still = None;
        self.restyle = false;
        self.look = None;
        self.pixels_said = false;
        self.error_said = false;
        self.failed_at = None;
        self.failures = 0;
        self.lost_said = false;
        self.look_lost = None;
        self.dropped.clear();
    }

    /// Reads a replacement without silently changing the renderer or resetting its style.
    pub fn relinked(&self, path: &str, subdataset: Option<&str>) -> Result<RasterLayer, String> {
        if self.embedded.is_some() {
            return Err("the raster's data is kept in the project: there's no file to repair".into());
        }
        if self.in_place {
            return Err("the raster is read in place by its address: add the address again to change it".into());
        }
        if !crate::paths::is_local(path) {
            return Err("give a raster file or Zarr store on this computer: web addresses aren't repaired here".into());
        }
        let sub = subdataset.or(self.subdataset.as_deref());
        let r = self.read_placed(path, sub, false).map_err(|e| format!("{path}: {e}"))?;
        self.validate_replacement(&r, path)?;
        let mut style = self.style.clone();
        style.repair(&r);
        // An SRTM tile or DTED file takes no subdataset or coordinate system: the ones the layer
        // had were said to be ignored as it was read, and aren't kept.
        let elevation = rustgis_data::raster::is_elevation_name(path);
        Ok(RasterLayer {
            path: Some(path.into()),
            in_place: false,
            file_name: None,
            embedded: None,
            subdataset: sub.filter(|_| !elevation).map(String::from),
            crs: self.crs.clone().filter(|_| !elevation),
            time_zarr: None,
            placement: self.placement,
            style,
            data: Some(Arc::new(r)),
            error: None,
            masked: MaskedStats::default(),
            cast: CastLight::default(),
            opening: None,
            read_still: None,
            read_generation: next_read_generation(),
            restyle: false,
            look: None,
            pixels_said: false,
            error_said: false,
            failed_at: None,
            failures: 0,
            lost_said: false,
            look_lost: None,
            dropped: Vec::new(),
        })
    }

    /// Checks a fresh candidate without changing the configured renderer or its palette.
    pub fn validate_replacement(&self, r: &Raster, path: &str) -> Result<(), String> {
        let need = self.style.renderer.bands().into_iter().max().map_or(1, |b| b.saturating_add(1));
        if r.band_count() < need {
            return Err(format!(
                "{path} has {} band(s); the layer's style draws band {need}: choose a raster with at least {need} bands",
                r.band_count()
            ));
        }
        if matches!(self.style.renderer, RasterRenderer::Colormap) && !matches!(r.color, ColorHint::Palette(_)) {
            return Err(format!(
                "{path} has no color palette; the layer's Colormap style uses the file's palette: choose a raster with a color palette"
            ));
        }
        Ok(())
    }

    /// A raster read from a file on disk.
    pub fn from_path(raster: Raster, path: impl Into<String>) -> RasterLayer {
        let style = RasterStyle::default_for(&raster);
        RasterLayer {
            path: Some(path.into()),
            in_place: false,
            file_name: None,
            embedded: None,
            subdataset: None,
            crs: None,
            time_zarr: None,
            placement: None,
            style,
            data: Some(Arc::new(raster)),
            error: None,
            masked: MaskedStats::default(),
            cast: CastLight::default(),
            opening: None,
            read_still: None,
            read_generation: next_read_generation(),
            restyle: false,
            look: None,
            pixels_said: false,
            error_said: false,
            failed_at: None,
            failures: 0,
            lost_said: false,
            look_lost: None,
            dropped: Vec::new(),
        }
    }

    /// A raster from a file's bytes, kept in the project.
    pub fn from_bytes(raster: Raster, file_name: impl Into<String>, bytes: Vec<u8>) -> RasterLayer {
        Self::from_shared(raster, file_name, Arc::new(bytes))
    }

    /// Keeps the original shared allocation used to decode an uploaded raster.
    pub fn from_shared(raster: Raster, file_name: impl Into<String>, bytes: Arc<Vec<u8>>) -> RasterLayer {
        let style = RasterStyle::default_for(&raster);
        RasterLayer {
            path: None,
            in_place: false,
            file_name: Some(file_name.into()),
            embedded: Some(bytes),
            subdataset: None,
            crs: None,
            time_zarr: None,
            placement: None,
            style,
            data: Some(Arc::new(raster)),
            error: None,
            masked: MaskedStats::default(),
            cast: CastLight::default(),
            opening: None,
            read_still: None,
            read_generation: next_read_generation(),
            restyle: false,
            look: None,
            pixels_said: false,
            error_said: false,
            failed_at: None,
            failures: 0,
            lost_said: false,
            look_lost: None,
            dropped: Vec::new(),
        }
    }

    /// A cloud-optimised GeoTIFF read in place by its address, its overview `raster` read.
    pub fn in_place(raster: Raster, address: impl Into<String>) -> RasterLayer {
        RasterLayer { in_place: true, ..RasterLayer::from_path(raster, address) }
    }

    /// A cloud-optimised GeoTIFF read in place by its address, its overview not read yet: drawn,
    /// in its own look, once [`RasterLayer::reload`] (or [`read_together`]) has read it.
    pub fn unread_in_place(address: impl Into<String>) -> RasterLayer {
        RasterLayer {
            path: Some(address.into()),
            in_place: true,
            file_name: None,
            embedded: None,
            subdataset: None,
            crs: None,
            time_zarr: None,
            placement: None,
            style: RasterStyle::placeholder(),
            data: None,
            error: None,
            masked: MaskedStats::default(),
            cast: CastLight::default(),
            opening: None,
            read_still: None,
            read_generation: next_read_generation(),
            restyle: true,
            look: None,
            pixels_said: false,
            error_said: false,
            failed_at: None,
            failures: 0,
            lost_said: false,
            look_lost: None,
            dropped: Vec::new(),
        }
    }

    /// [`RasterLayer::unread_in_place`], its overview being read: it lands as the map draws
    /// ([`RasterLayer::poll_reload`]), in the browser and on the desktop alike, and failing is
    /// said then (see [`RasterLayer::error_unsaid`]).
    pub fn opening_in_place(address: impl Into<String>) -> RasterLayer {
        let mut layer = RasterLayer::unread_in_place(address);
        let _ = layer.begin_reload();
        layer
    }

    /// The layer in the look its address gives (see [`AddressLook`]): now, when its pixels are
    /// here, else once they land.
    pub fn with_look(mut self, look: AddressLook) -> RasterLayer {
        match self.data.clone() {
            Some(d) => {
                self.look_lost = RasterStyle::look_lost(&d, &look);
                if let Some(style) = RasterStyle::for_look(&d, &look) {
                    self.style = style;
                }
            }
            None => self.look = Some(look),
        }
        self
    }

    /// Marks the layer as one that couldn't be read, and why (a project opened without the
    /// connection its address needs), for [`RasterLayer::error_unsaid`] to say.
    pub fn fail(&mut self, why: impl Into<String>) {
        self.error = Some(why.into());
        self.error_said = false;
        self.failed_at = None;
    }

    /// Starts reading its overview in place by the reader the app installed, which answers later
    /// (installed by [`RasterLayer::poll_reload`] as the map draws, or waited for by
    /// [`RasterLayer::reload`] natively). What it can't start (no address, an address that isn't a
    /// tile layer's, no reader here) is the caller's to say.
    fn begin_in_place_read(&mut self) -> Result<(), String> {
        use rustgis_data::source::remote;
        let still = remote::current_still();
        remote::check_still(&still)?;
        let Some(address) = self.path.clone() else { return Err("the raster has no source".to_string()) };
        // Checked as a tile layer's address is (http(s) or s3, a host, no control characters): a
        // project edited by hand asks for nothing else.
        crate::tiles::check_url(&address)?;
        let Some(reader) = in_place_reader().lock().unwrap_or_else(|e| e.into_inner()).clone() else {
            return Err(format!("{address}: can't be read in place here"));
        };
        self.data = None;
        let slot = Arc::new(Mutex::new(None));
        let land = slot.clone();
        self.opening = Some(slot);
        self.read_still = still.clone();
        reader(
            &address,
            Box::new(move |result| {
                *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(remote::check_still(&still).and(result.map(Arc::new)));
                crate::source::wake();
            }),
        );
        Ok(())
    }

    /// Why reading its overview in place failed, when, and how many reads in a row have failed:
    /// for the read to be made again once its wait is over (see [`RasterLayer::read_again`]), as a
    /// tile or a file's header is, waiting longer each time. `None` for a layer with its pixels,
    /// one being read, and one that waits for something other than time (its bucket's connection:
    /// see [`RasterLayer::fail`]).
    pub fn read_failure(&self) -> Option<(&str, Instant, u32)> {
        if !self.in_place || self.data.is_some() || self.opening.is_some() {
            return None;
        }
        Some((self.error.as_deref()?, self.failed_at?, self.failures))
    }

    /// Reads its overview again after a read that failed (see [`RasterLayer::read_failure`]),
    /// without waiting: it lands as the map draws, natively as in the browser (nothing may wait
    /// each frame). The layer keeps its error until then, and failing the same way again isn't
    /// said again.
    pub fn read_again(&mut self) {
        if self.read_failure().is_none() {
            return;
        }
        use rustgis_data::source::remote;
        let still = self.read_still.clone();
        if let Err(e) = remote::check_still(&still) {
            self.fail(e);
            return;
        }
        self.read_generation = next_read_generation();
        if let Err(e) = remote::with_optional_still(still, || self.begin_in_place_read()) {
            self.land(Err(e));
        }
    }

    /// Reads the pixels again from the path or the embedded file (after a project opens). A
    /// layer read in place is waited for natively (at most two minutes), as a file is read;
    /// another layer's S3 or login-wrapped address is downloaded whole, keeping its wrapper. A
    /// layer that follows the time slider is left to the engine, which reads the step shown.
    pub fn reload(&mut self) -> Result<(), String> {
        self.reload_in(true)
    }

    /// [`RasterLayer::reload`] without waiting for a layer read in place: its overview lands as
    /// the map draws ([`RasterLayer::poll_reload`]), natively as in the browser, so a command on
    /// the desktop's own thread isn't held by a slow host. The error is a read that can't start.
    pub fn begin_reload(&mut self) -> Result<(), String> {
        self.reload_in(false)
    }

    fn reload_in(&mut self, wait: bool) -> Result<(), String> {
        if self.time_zarr.is_some() {
            return Ok(());
        }
        use rustgis_data::source::remote;
        let still = remote::current_still();
        remote::check_still(&still)?;
        self.read_generation = next_read_generation();
        self.read_still = still;
        self.opening = None;
        self.error = None;
        self.failed_at = None;
        self.failures = 0;
        // Read in place: its overview, by the reader the app installed; waited for natively when
        // asked to (as a file is read), else landing later, as always in the browser.
        if self.in_place {
            if let Err(e) = self.begin_in_place_read() {
                self.error = Some(e.clone());
                self.error_said = false;
                return Err(e);
            }
            #[cfg(not(target_arch = "wasm32"))]
            if wait {
                return self.wait_for_read(Instant::now() + rustgis_data::raster::IN_PLACE_WAIT);
            }
            #[cfg(target_arch = "wasm32")]
            let _ = wait;
            return Ok(());
        }
        if self.embedded.is_none()
            && let Some(path) = self.path.as_deref().filter(|p| {
                rustgis_data::login_address::is_wrapper(p)
                    || rustgis_data::s3_address::is_wrapper(p)
                    || p.get(..5).is_some_and(|s| s.eq_ignore_ascii_case("s3://"))
            })
        {
            let name = rustgis_data::login_address::inner(rustgis_data::s3_address::inner(path))
                .split(['?', '#'])
                .next()
                .unwrap_or("raster.tif")
                .to_string();
            let placement = self.placement;
            // A PDF's page, read again from the download.
            let page = self.subdataset.clone();
            let slot = Arc::new(Mutex::new(None));
            let land = slot.clone();
            let still = self.read_still.clone();
            self.data = None;
            self.opening = Some(slot);
            rustgis_data::source::remote::fetch_whole(
                path,
                Box::new(move |result| {
                    let result = remote::check_still(&still)
                        .and(result.map_err(|e| match e {
                            rustgis_data::source::remote::Failed::Other(message) => message,
                            _ => "The raster could not be downloaded.".to_string(),
                        }))
                        .and_then(|bytes| {
                            rustgis_data::raster::read_shared_page(&name, Arc::new(bytes), page.as_deref(), placement.is_none())
                                .and_then(|r| placed(r, placement))
                                .map(Arc::new)
                                .map_err(|e| e.to_string())
                        });
                    *land.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
                    crate::source::wake();
                }),
            );
            return Ok(());
        }
        // A scan goes back where it was laid (a GeoTIFF opened `as` a scan is one again).
        let r = if let Some(bytes) = &self.embedded {
            rustgis_data::raster::read_shared_page(
                self.file_name.as_deref().unwrap_or("raster.tif"),
                bytes.clone(),
                self.subdataset.as_deref(),
                self.placement.is_none(),
            )
            .and_then(|r| placed(r, self.placement))
        } else if let Some(p) = &self.path {
            self.read_placed(p, self.subdataset.as_deref(), true)
        } else {
            return Err("the raster has no source".into());
        };
        match r {
            Ok(r) => {
                self.forget_ignored_options();
                self.style.repair(&r);
                self.data = Some(Arc::new(r));
                self.pixels_said = false;
                self.lost_said = false;
                Ok(())
            }
            Err(e) => {
                self.error = Some(e.to_string());
                self.data = None;
                Err(e.to_string())
            }
        }
    }

    /// Waits (natively, until `deadline`) for a read under way, then installs it.
    #[cfg(not(target_arch = "wasm32"))]
    fn wait_for_read(&mut self, deadline: Instant) -> Result<(), String> {
        let Some(slot) = self.opening.clone() else { return Ok(()) };
        loop {
            if let Err(e) = rustgis_data::source::remote::check_still(&self.read_still) {
                self.opening = None;
                self.fail(e.clone());
                return Err(e);
            }
            if let Some(result) = slot.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                self.opening = None;
                let error = result.as_ref().err().cloned();
                self.land(result);
                return error.map_or(Ok(()), Err);
            }
            if Instant::now() >= deadline {
                let address = self.path.clone().unwrap_or_default();
                let e = format!("{address}: no answer in two minutes");
                self.opening = None;
                self.land(Err(e.clone()));
                return Err(e);
            }
            std::thread::sleep(web_time::Duration::from_millis(10));
        }
    }

    /// Its pixels, the first time they are asked for after they landed (added with them, or read
    /// in place later: in the browser, a project, an import), so what reading them lost (a colour
    /// table missing, an overview held) is said once for them.
    pub fn pixels_unsaid(&mut self) -> Option<Arc<Raster>> {
        if self.pixels_said {
            return None;
        }
        let d = self.data.clone()?;
        self.pixels_said = true;
        Some(d)
    }

    /// The layer, opened from a tile layer whose `dropped` settings (`"attribution"`, `"legend"`,
    /// `"zoom range"`, `"bounds"`, `"overlay"`) a raster layer hasn't, so they aren't kept: said
    /// once, by [`RasterLayer::dropped_unsaid`].
    pub fn from_tiles_dropping(mut self, dropped: Vec<&'static str>) -> RasterLayer {
        self.dropped = dropped;
        self
    }

    /// The tile layer settings dropped when this layer was opened from one, the first time they
    /// are asked for.
    pub fn dropped_unsaid(&mut self) -> Vec<&'static str> {
        std::mem::take(&mut self.dropped)
    }

    /// What reading its pixels left out (`Raster::lost`), the first time it is asked for after
    /// they landed, so it is said once: as the layer is added, as its project opens, or when they
    /// land later (the browser).
    pub fn lost_unsaid(&mut self) -> Vec<String> {
        if self.lost_said {
            // A look applied after the pixels' losses were said: what it asked for that the file
            // hasn't, on its own, not the pixels' losses again.
            return self.look_lost.take().into_iter().collect();
        }
        let mut lost = self.data.as_ref().map(|d| d.lost.clone()).unwrap_or_default();
        lost.extend(self.look_lost.take());
        self.lost_said = self.data.is_some();
        lost
    }

    /// Why a layer read in place couldn't be read, the first time it is asked for after the read
    /// failed, so it is said once (a project, an import or the browser read it after adding it).
    pub fn error_unsaid(&mut self) -> Option<String> {
        if !self.in_place || self.error_said {
            return None;
        }
        let e = self.error.clone()?;
        self.error_said = true;
        Some(e)
    }

    /// Installs a completed remote read; each render copy can observe the same shared result.
    pub(crate) fn poll_reload(&mut self) -> bool {
        let Some(slot) = self.opening.as_ref() else { return false };
        if let Err(e) = rustgis_data::source::remote::check_still(&self.read_still) {
            self.opening = None;
            self.fail(e);
            crate::source::another_frame();
            return false;
        }
        let Some(result) = slot.lock().unwrap_or_else(|e| e.into_inner()).clone() else { return true };
        self.opening = None;
        self.land(result);
        // Installed while the map draws: a frame after it says what reading them lost.
        crate::source::another_frame();
        false
    }

    /// Installs pixels read (or why they couldn't be): in the file's own look for a layer added
    /// before they arrived, else in its style, repaired for them.
    fn land(&mut self, result: Result<Arc<Raster>, String>) {
        match result {
            Ok(raster) => {
                self.pixels_said = false;
                self.lost_said = false;
                if std::mem::take(&mut self.restyle) {
                    let look = self.look.take();
                    self.look_lost = look.as_ref().and_then(|look| RasterStyle::look_lost(&raster, look));
                    let look = look.and_then(|look| RasterStyle::for_look(&raster, &look));
                    self.style = look.unwrap_or_else(|| RasterStyle::default_for(&raster));
                } else {
                    self.style.repair(&raster);
                }
                self.data = Some(raster);
                self.error = None;
                self.failed_at = None;
                self.failures = 0;
            }
            Err(error) => {
                // A read made again that fails as before (see `read_again`) isn't said again.
                self.error_said = self.error_said && self.error.as_deref() == Some(error.as_str());
                self.error = Some(error);
                self.failed_at = Some(Instant::now());
                self.failures = self.failures.saturating_add(1);
            }
        }
    }

    /// Statistics of the renderer's bands (one per band, in its order) without the pixels the
    /// style's mask hides, for the stretch; `None` without a mask. Worked out once (from a sample
    /// of a large raster) and kept until the mask, the bands or the pixels change.
    pub fn masked_stats(&self) -> Option<Arc<Vec<BandStats>>> {
        let r = self.data.as_ref()?;
        let mask = &self.style.mask;
        if mask.is_empty() {
            return None;
        }
        let bands = self.style.renderer.bands();
        if let Some((m, b, w, stats)) = self.masked.0.lock().ok()?.as_ref()
            && m == mask
            && *b == bands
            && Weak::ptr_eq(w, &Arc::downgrade(r))
        {
            return Some(stats.clone());
        }
        // Worked out without holding the lock.
        let stats = Arc::new(bands.iter().map(|b| mask.stats(r, *b)).collect::<Vec<_>>());
        if let Ok(mut cache) = self.masked.0.lock() {
            *cache = Some((mask.clone(), bands, Arc::downgrade(r), stats.clone()));
        }
        Some(stats)
    }

    /// What a frame draws it from ([`crate::look`]): its settings and its pixels' allocation, which
    /// doesn't keep them. `None` when drawing reads more than it holds: read in place (the part in
    /// view is read as the map draws), still being read, or lit by cast shadows or sky worked out
    /// in the background.
    pub fn look(&self) -> Option<RasterLook> {
        let RasterLayer {
            path,
            in_place,
            // Read into `data`, compared itself.
            file_name: _,
            embedded: _,
            subdataset,
            crs,
            time_zarr,
            placement,
            style,
            data,
            // What it says, not what it draws.
            error: _,
            // Worked out from the pixels and the style.
            masked: _,
            cast: _,
            opening,
            read_still,
            read_generation: _,
            restyle,
            look,
            pixels_said: _,
            error_said: _,
            failed_at: _,
            failures: _,
            lost_said: _,
            look_lost: _,
            dropped: _,
        } = self;
        if *in_place || opening.is_some() || read_still.is_some() || self.casts_shadows() {
            return None;
        }
        Some(RasterLook {
            path: path.clone(),
            subdataset: subdataset.clone(),
            crs: crs.clone(),
            time_zarr: time_zarr.clone(),
            placement: *placement,
            style: style.clone(),
            restyle: *restyle,
            look: look.clone(),
            data: Arc::downgrade(data.as_ref()?),
        })
    }

    /// Whether it is drawn with cast shadows or sky visibility (Shaded Relief with a shadow or a
    /// sky above 0): its frames are drawn again as they land (see [`RasterLayer::cast_shadows`]).
    pub fn casts_shadows(&self) -> bool {
        matches!(self.style.renderer, RasterRenderer::ShadedRelief { shadow, sky, .. } if shadow > 0.0 || sky > 0.0)
    }

    /// How many wait for its cast shadows or sky now (exports), its copies' too: for tests of what
    /// an export waits for.
    #[doc(hidden)]
    pub fn cast_shadow_waiters(&self) -> usize {
        let Ok(jobs) = self.cast.0.lock() else { return 0 };
        jobs.iter().map(|j| j.work.waiting.load(std::sync::atomic::Ordering::SeqCst)).sum()
    }

    /// How many light jobs it and its copies keep, including finished ones: for tests of whether
    /// drawing started work, without starting any work through the inspection itself.
    #[doc(hidden)]
    pub fn cast_shadow_jobs(&self) -> usize {
        self.cast.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// The light its cast shadows leave on each cell (see [`crate::shadow::light`]), when it is
    /// drawn as Shaded Relief with shadows. Worked out once per pixels, band, sun, Z factor and
    /// mask and kept: each owner's latest, and a few of suns left, for going back.
    /// `owner` is who draws it (its map's and its layer's live identities, see
    /// [`crate::Layer::instance_id`]; a layer's id repeats from map to map): a new sun from the
    /// same owner stops the work
    /// for its last one, unless something waits for it. Natively it is worked out on a thread of
    /// its own, then `landed` is called (the map asks to be drawn again); with `wait`, as an
    /// export does, this waits for it. The browser, which has one thread, works it out here, for
    /// at most [`crate::shadow::MAX_BROWSER_CELLS`] cells.
    pub fn cast_shadows(&self, wait: bool, owner: (u64, u64), landed: impl FnOnce() + Send + 'static) -> CastShadows {
        let RasterRenderer::ShadedRelief { azimuth, altitude, shadow, .. } = self.style.renderer else {
            return CastShadows::None;
        };
        let finite = azimuth.is_finite() && altitude.is_finite();
        self.light_grid(Grid::Shadows { azimuth, altitude }, shadow > 0.0 && finite, wait, owner, landed)
    }

    /// The share of the sky each cell sees (see [`crate::shadow::sky`]), when it is drawn as
    /// Shaded Relief with a sky above 0: worked out once per pixels, band, Z factor and mask, then
    /// kept, waited for and said as [`RasterLayer::cast_shadows`] has its light (each owner's
    /// latest is kept, whatever its sun).
    pub fn sky_view(&self, wait: bool, owner: (u64, u64), landed: impl FnOnce() + Send + 'static) -> CastShadows {
        let RasterRenderer::ShadedRelief { sky, .. } = self.style.renderer else {
            return CastShadows::None;
        };
        self.light_grid(Grid::Sky, sky > 0.0, wait, owner, landed)
    }

    /// The `grid` of [`RasterLayer::cast_shadows`] or [`RasterLayer::sky_view`], when `on`.
    fn light_grid(&self, grid: Grid, on: bool, wait: bool, owner: (u64, u64), landed: impl FnOnce() + Send + 'static) -> CastShadows {
        let RasterRenderer::ShadedRelief { band, z_factor, .. } = self.style.renderer else {
            return CastShadows::None;
        };
        let Some(r) = self.data.as_ref().filter(|_| on && z_factor.is_finite()) else { return CastShadows::None };
        let not_drawn = grid.not_drawn();
        // Read in place, the layer holds an overview; the part in view is read as the map draws.
        if self.in_place && r.overview.is_some_and(|(w, _)| w > r.width) {
            return CastShadows::Unavailable(format!("{not_drawn}: it is read in place, and only its overview is held"));
        }
        let cells = r.width as usize * r.height as usize;
        if cfg!(target_arch = "wasm32") && cells > crate::shadow::MAX_BROWSER_CELLS {
            return CastShadows::Unavailable(format!(
                "{not_drawn}: in the browser it is worked out for rasters of at most {} cells, and it has {cells}",
                crate::shadow::MAX_BROWSER_CELLS
            ));
        }
        let key = LightKey { band, grid, z_factor, mask: self.style.mask.clone() };
        let mut start = None;
        let work = {
            let Ok(mut jobs) = self.cast.0.lock() else { return CastShadows::None };
            // Grids of pixels no longer held go (their work stops as it is dropped), and so does
            // work stopped part-way (a cancelled run), to be started again.
            jobs.retain(|j| j.pixels.strong_count() > 0 && !j.work.was_stopped(j.key.grid.stopped()));
            let pixels = Arc::downgrade(r);
            let same = |j: &LightJob| j.key == key && Weak::ptr_eq(&j.pixels, &pixels);
            // The same owner's work for another sun, not done and not waited for, is superseded
            // (going back to a sun kept too): it stops, as it is dropped. Grids done are kept, for
            // going back. Its other kind of grid (its sky, or its shadows) is left alone.
            jobs.retain(|j| same(j) || !j.rivals(owner, &key) || j.work.is_done() || j.work.waited());
            match jobs.iter().position(same) {
                Some(i) => {
                    // The latest used last, so the oldest goes first.
                    let mut job = jobs.remove(i);
                    job.owner = owner;
                    let work = job.work.clone();
                    jobs.push(job);
                    // Joining work under way (an export's, another frame's): drawn again when it
                    // lands too.
                    work.on_landing(landed);
                    work
                }
                None => {
                    // Each owner's latest grid is kept: it is what that owner draws, and letting it go
                    // would only have it worked out again, letting another go (a page of more maps,
                    // lit otherwise, than were kept never got its shadows). The grids of suns
                    // owners have left, done and waited for by nothing, are kept for going back: the
                    // oldest goes past a few.
                    let left: Vec<usize> = (0..jobs.len())
                        .filter(|i| {
                            jobs.get(*i).is_some_and(|j| {
                                j.work.is_done() && !j.work.waited() && jobs.iter().skip(i + 1).any(|later| later.rivals(j.owner, &j.key))
                            })
                        })
                        .collect();
                    if left.len() >= KEPT_LIGHTS
                        && let Some(i) = left.first()
                    {
                        jobs.remove(*i);
                    }
                    let work = Arc::new(LightWork::default());
                    work.on_landing(landed);
                    jobs.push(LightJob { key: key.clone(), pixels: Arc::downgrade(r), owner, work: work.clone() });
                    start = Some((r.clone(), key));
                    work
                }
            }
        };
        // Started without holding the lock: a thread the system refuses runs it here.
        if let Some((r, key)) = start {
            start_light(r, key, work.clone());
        }
        work.result(wait)
    }
}

/// [`RasterLayer::cast_shadows`] and [`RasterLayer::sky_view`]: what the map draws its cast
/// shadows, or its sky, with.
#[derive(Clone, Debug, PartialEq)]
pub enum CastShadows {
    /// It casts none (another renderer, or a shadow or sky of 0).
    None,
    /// The light left on each cell, or the sky it sees (0–255), row by row as the pixels are.
    Ready(Arc<Vec<u8>>),
    /// Being worked out: drawn without, and again once it has landed.
    Pending,
    /// Drawn without them, and why.
    Unavailable(String),
}

/// What a light grid is worked out for, besides the pixels.
#[derive(Clone, Debug, PartialEq)]
struct LightKey {
    band: usize,
    grid: Grid,
    z_factor: f64,
    mask: RasterMask,
}

/// Which grid: the light cast shadows leave for a sun, or the sky seen.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Grid {
    Shadows { azimuth: f64, altitude: f64 },
    Sky,
}

impl Grid {
    /// What the relief is drawn without when there is no grid.
    fn not_drawn(self) -> &'static str {
        match self {
            Grid::Shadows { .. } => "its cast shadows aren't drawn",
            Grid::Sky => "its sky visibility isn't drawn",
        }
    }

    /// Why a grid stopped part-way has none: superseded, or its run cancelled.
    fn stopped(self) -> &'static str {
        match self {
            Grid::Shadows { .. } => "its cast shadows were stopped",
            Grid::Sky => "its sky visibility was stopped",
        }
    }
}

/// A light grid's work, shared with the thread doing it.
#[derive(Default)]
struct LightWork {
    stop: std::sync::atomic::AtomicBool,
    /// The grid, or why there is none, once it is done.
    done: Mutex<Option<Result<Arc<Vec<u8>>, String>>>,
    landed: std::sync::Condvar,
    /// How many wait for it (exports): it is neither superseded nor let go meanwhile.
    waiting: std::sync::atomic::AtomicUsize,
    /// Called once it is done: each frame or page drawn while it was to come asks to be drawn
    /// again (at most [`MAX_LANDINGS`] are kept; the first already redraws them all).
    landings: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

/// The most landing calls a light grid's work keeps.
const MAX_LANDINGS: usize = 32;

impl LightWork {
    /// Calls `landed` once the work is done; nothing when it is already (the caller has it).
    fn on_landing(&self, landed: impl FnOnce() + Send + 'static) {
        // Checked under the calls' lock, which `land` takes after the work is done: a call is
        // either kept and made, or not needed.
        let Ok(mut calls) = self.landings.lock() else { return };
        if !self.is_done() && calls.len() < MAX_LANDINGS {
            calls.push(Box::new(landed));
        }
    }

    /// Makes the landing calls, once the work is done.
    fn land(&self) {
        let calls = self.landings.lock().map(|mut c| std::mem::take(&mut *c)).unwrap_or_default();
        for call in calls {
            call();
        }
    }

    fn is_done(&self) -> bool {
        self.done.lock().is_ok_and(|d| d.is_some())
    }

    /// Whether it was stopped part-way, rather than done (`stopped` is why, see [`Grid::stopped`]).
    fn was_stopped(&self, stopped: &str) -> bool {
        self.done.lock().is_ok_and(|d| matches!(d.as_ref(), Some(Err(e)) if e == stopped))
    }

    fn waited(&self) -> bool {
        self.waiting.load(std::sync::atomic::Ordering::SeqCst) > 0
    }

    fn finish(&self, out: Result<Arc<Vec<u8>>, String>) {
        if let Ok(mut done) = self.done.lock() {
            *done = Some(out);
        }
        self.landed.notify_all();
    }

    /// What it has: with `wait`, once it is done, unless the run waiting is cancelled (see
    /// [`rustgis_data::progress`]). A cancelled run that was the only one waiting stops the work
    /// too (the worker has no probe of its own): the map, drawing again, starts it over.
    fn result(&self, wait: bool) -> CastShadows {
        let _waiting = wait.then(|| Waiting::on(&self.waiting));
        let Ok(mut done) = self.done.lock() else { return CastShadows::None };
        while wait && done.is_none() && rustgis_data::progress::check().is_ok() {
            done = match self.landed.wait_timeout(done, std::time::Duration::from_millis(100)) {
                Ok((d, _)) => d,
                Err(_) => return CastShadows::None,
            };
        }
        if wait && done.is_none() && self.waiting.load(std::sync::atomic::Ordering::SeqCst) == 1 {
            self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        match done.as_ref() {
            Some(Ok(grid)) => CastShadows::Ready(grid.clone()),
            Some(Err(why)) => CastShadows::Unavailable(why.clone()),
            None => CastShadows::Pending,
        }
    }
}

/// Counts one waiting for a light grid while it lives.
struct Waiting<'a>(&'a std::sync::atomic::AtomicUsize);

impl<'a> Waiting<'a> {
    fn on(count: &'a std::sync::atomic::AtomicUsize) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Waiting(count)
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// The light grid kept for a layer, and the work for it.
struct LightJob {
    key: LightKey,
    /// Held weakly, so their address isn't reused.
    pixels: Weak<Raster>,
    /// Who drew it last (see [`RasterLayer::cast_shadows`]).
    owner: (u64, u64),
    work: Arc<LightWork>,
}

impl LightJob {
    /// Whether it is `owner`'s grid of the kind `key` is: a sun's light, or the sky.
    fn rivals(&self, owner: (u64, u64), key: &LightKey) -> bool {
        self.owner == owner && std::mem::discriminant(&self.key.grid) == std::mem::discriminant(&key.grid)
    }
}

impl Drop for LightJob {
    /// Replaced, or its layer gone: the work for it stops.
    fn drop(&mut self) {
        self.work.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// The light grids kept for a layer and its copies, the latest used last.
#[derive(Clone, Default)]
struct CastLight(Arc<Mutex<Vec<LightJob>>>);

/// The most grids of suns their owners have left that a layer and its copies keep, besides
/// each owner's latest of each kind.
const KEPT_LIGHTS: usize = 3;

/// Held by [`hold_cast_shadows`]: light work waits for it before it traces.
static LIGHT_HOLD: std::sync::RwLock<()> = std::sync::RwLock::new(());

/// Holds every light grid's work before it traces, until the guard is dropped: for tests of what
/// is drawn, and said, while cast shadows are still to come, whatever the computer's speed.
#[doc(hidden)]
pub fn hold_cast_shadows() -> std::sync::RwLockWriteGuard<'static, ()> {
    LIGHT_HOLD.write().unwrap_or_else(|e| e.into_inner())
}

/// Works out a light grid: natively on a thread of its own (here if the system refuses one),
/// in the browser here.
fn start_light(r: Arc<Raster>, key: LightKey, work: Arc<LightWork>) {
    let run = move || {
        drop(LIGHT_HOLD.read().unwrap_or_else(|e| e.into_inner()));
        let threads = rustgis_data::threads::max();
        let traced = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match key.grid {
            Grid::Shadows { azimuth, altitude } => {
                crate::shadow::light(&r, key.band, &key.mask, azimuth, altitude, key.z_factor, threads, &work.stop)
            }
            Grid::Sky => crate::shadow::sky(&r, key.band, &key.mask, key.z_factor, threads, &work.stop),
        }));
        let not_drawn = key.grid.not_drawn();
        match traced {
            // Stopped (superseded, or its export cancelled): a frame or page drawn while it was
            // to come is drawn again, and asks for what it draws now.
            Ok(Err(crate::shadow::Unlit::Stopped)) => {
                work.finish(Err(key.grid.stopped().into()));
                work.land();
            }
            Ok(out) => {
                work.finish(out.map(Arc::new).map_err(|e| match e {
                    crate::shadow::Unlit::Failed => format!("{not_drawn}: working it out failed"),
                    _ => format!("{not_drawn}: there isn't the memory for it"),
                }));
                work.land();
            }
            // Said, rather than leaving an export waiting.
            Err(_) => {
                work.finish(Err(format!("{not_drawn}: working it out failed")));
                work.land();
            }
        }
    };
    #[cfg(not(target_arch = "wasm32"))]
    {
        let run = std::sync::Arc::new(Mutex::new(Some(run)));
        let on_thread = run.clone();
        let spawned = std::thread::Builder::new().name("relief light".into()).spawn(move || {
            if let Some(f) = on_thread.lock().ok().and_then(|mut f| f.take()) {
                f();
            }
        });
        if spawned.is_err()
            && let Some(f) = run.lock().ok().and_then(|mut f| f.take())
        {
            f();
        }
    }
    #[cfg(target_arch = "wasm32")]
    run();
}

impl RasterLayer {
    /// The layer's file read again (a project reopened, a source repaired), as it was added.
    ///
    /// - A scan (every raster that opened without coordinates, `"as": "scan"` or not, is laid over
    ///   a placement when added) is read without the world file beside it (#2075), its subdataset
    ///   and coordinate system kept, and laid where it was, a GeoTIFF's own coordinates set aside: a
    ///   world file or `.prj` that turns up later doesn't move it (add it again).
    /// - Any other layer is read as [`rustgis_data::raster::read_path_with`] reads it. When
    ///   `reopening` its own file, one that has no coordinates now (the `.prj` or world file that
    ///   placed it gone) still opens, and says so, rather than being drawn at its stand-in near
    ///   0°, 0° without a word. (A source repaired to another file says nothing of the first.)
    fn read_placed(&self, path: &str, subdataset: Option<&str>, reopening: bool) -> rustgis_data::Result<Raster> {
        use rustgis_data::raster::{read_path_unplaced, read_path_with};
        let p = std::path::Path::new(path);
        let crs = self.crs.as_deref();
        if self.placement.is_some() {
            return placed(read_path_unplaced(p, subdataset, crs)?, self.placement);
        }
        let mut r = read_path_with(p, subdataset, crs)?;
        if reopening && !r.georeferenced {
            r.lost.push("it has no coordinates now, though it had them when added: it is drawn at a stand-in place".into());
        }
        Ok(r)
    }

    /// Drops the subdataset and coordinate system of an SRTM tile or DTED file read from its path
    /// (a project saved when these files went through GDAL kept them): reading it said they are
    /// ignored, once, and the project saved again leaves them out, so the next open doesn't say it
    /// again.
    fn forget_ignored_options(&mut self) {
        if self.embedded.is_none() && self.path.as_deref().is_some_and(rustgis_data::raster::is_elevation_name) {
            self.subdataset = None;
            self.crs = None;
        }
    }
}

/// Reapplies a scan's placement to pixels read again, without copying their bands.
fn placed(mut raster: Raster, placement: Option<[f64; 4]>) -> rustgis_data::Result<Raster> {
    if let Some(bbox) = placement {
        if raster.georeferenced {
            raster = raster.into_scan()?;
        }
        raster.place(bbox)?;
    }
    Ok(raster)
}

/// The most steps a Zarr layer following the time slider lists (as the slider itself does).
pub const MAX_ZARR_STEPS: usize = crate::time::MAX_STEPS;

/// A Zarr variable that follows the time slider: the store (a folder or a web address), the
/// variable, the other dimensions' fixed steps, and its time dimension's steps as dates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ZarrTime {
    /// The store's folder or web address.
    pub store: String,
    pub variable: String,
    /// The other dimensions' steps (a pressure level), as `layer.addZarr`'s `index`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub index: Vec<(String, u64)>,
    /// The time dimension's name.
    pub dim: String,
    /// The step of `dim` the first of `dates` is (a period chosen from a longer axis).
    #[serde(skip_serializing_if = "is_zero")]
    pub offset: u64,
    /// The steps' dates, milliseconds since 1970 (UTC), in order.
    pub dates: Vec<f64>,
    /// The coordinate system given for a store that doesn't say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crs: Option<String>,
    /// The window of the map's cells each step reads, `[x0, y0, x1, y1]` as `layer.addZarr`'s
    /// `cells` (the ends not included); `None` for the whole map. A project with one requires
    /// `zarr-window`: an older reader would read the whole map at each step.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<[u64; 4]>,
    /// The step whose slice the layer holds (an index into `dates`); `None` when none is.
    #[serde(skip)]
    pub shown: Option<usize>,
    /// Process-local read generation: explicit reload must not reuse old slices or pending reads.
    #[serde(skip, default = "next_read_generation")]
    pub read_generation: u64,
}

/// A process-local identity for a fresh Zarr opening or independent analysis invocation.
pub fn next_read_generation() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Default for ZarrTime {
    fn default() -> Self {
        Self {
            store: String::new(),
            variable: String::new(),
            index: Vec::new(),
            dim: String::new(),
            offset: 0,
            dates: Vec::new(),
            crs: None,
            window: None,
            shown: None,
            read_generation: next_read_generation(),
        }
    }
}

/// Shaded Relief's rock colour on steep slopes (#1913): below `from` degrees of slope the ramp's
/// colour is kept, above `to` it is `color`, and between them the two are blended on a smooth
/// step. The slope is the ground's own, without the Z factor (see [`slope`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rock {
    pub from: f64,
    pub to: f64,
    pub color: Rgba,
}

impl Rock {
    /// What a rock turned on without settings is: a light warm grey from 25° to 40°, which a 30 m
    /// DEM reaches on its real scarps (it averages a cliff with its foot).
    pub const DEFAULT: Rock = Rock { from: 25.0, to: 40.0, color: Rgba::rgb(0xB2, 0xAC, 0xA4) };

    /// The share of rock (0 to 1) on a slope of `degrees`: 0 up to `from`, 1 from `to` on, and a
    /// smooth step between them, so no hard outline follows a contour of slope.
    pub fn share(&self, degrees: f64) -> f64 {
        if self.to - self.from <= 0.0 {
            return if degrees >= self.to { 1.0 } else { 0.0 };
        }
        let t = ((degrees - self.from) / (self.to - self.from)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }
}

/// The share of the colour a Shaded Relief slope turned fully from the sun keeps: its `ambient`
/// when set, or else 0.25 (the light of old), and 0.5 with the `balanced` light, which keeps about
/// the old light's contrast (#1977).
pub fn relief_ambient(ambient: Option<f64>, balanced: bool) -> f64 {
    ambient.unwrap_or(if balanced { 0.5 } else { 0.25 }).clamp(0.0, 1.0)
}

/// The most a balanced Shaded Relief light multiplies a colour by, on slopes facing the sun.
const BALANCED_MAX: f64 = 1.3;

fn is_no_light(v: &f64) -> bool {
    *v == 0.0
}

fn is_zero(v: &u64) -> bool {
    *v == 0
}

impl ZarrTime {
    /// The step to show for the slider's window `[from, to)`: the last one in it, `None` when
    /// none falls in it. With the slider off, the first step.
    pub fn step_for(&self, window: Option<(f64, f64)>) -> Option<usize> {
        match window {
            None => (!self.dates.is_empty()).then_some(0),
            Some((from, to)) => self.dates.iter().rposition(|d| *d >= from && *d < to),
        }
    }

    /// The selection of step `i` as `zarr::Selection::parse` reads it: `t2m[time=12,level=3]`,
    /// with `cells=x0:y0:x1:y1` after them for a window.
    pub fn selection(&self, i: usize) -> String {
        let mut parts = vec![format!("{}={}", self.dim, self.offset.saturating_add(i as u64))];
        parts.extend(self.index.iter().filter(|(d, _)| *d != self.dim).map(|(d, v)| format!("{d}={v}")));
        parts.extend(self.window.map(|[x0, y0, x1, y1]| format!("cells={x0}:{y0}:{x1}:{y1}")));
        format!("{}[{}]", self.variable, parts.join(","))
    }

    /// Keeps a loaded layer within bounds: dates finite, in order, at most [`MAX_ZARR_STEPS`];
    /// names of sane length.
    pub fn repair(&mut self) {
        self.dates.truncate(MAX_ZARR_STEPS);
        // Date `i` is step `offset + i`: dates out of order, repeated or not finite (a project
        // edited by hand) can't be matched to steps, so none is followed rather than the wrong one.
        if !self.dates.iter().all(|d| d.is_finite()) || !self.dates.windows(2).all(|w| w[0] < w[1]) {
            self.dates.clear();
        }
        self.index.truncate(32);
        // A window is kept as it is: one a read can't take (a project edited by hand) is refused
        // at each step, never widened to the whole map.
        for t in [&mut self.store, &mut self.variable, &mut self.dim] {
            *t = t.chars().take(4096).collect();
        }
        self.shown = None;
    }
}

pub(crate) mod b64 {
    use std::sync::Arc;

    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &Option<Arc<Vec<u8>>>, s: S) -> Result<S::Ok, S::Error> {
        match v {
            Some(b) => s.serialize_some(&base64::engine::general_purpose::STANDARD.encode(b.as_slice())),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Arc<Vec<u8>>>, D::Error> {
        let s: Option<String> = Option::deserialize(d)?;
        // A corrupt embedded file leaves the layer without data rather than failing the project.
        Ok(s.and_then(|s| base64::engine::general_purpose::STANDARD.decode(s.as_bytes()).ok()).map(Arc::new))
    }
}

/// How pixel values map onto the colour range.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Stretch {
    /// Values as they are (0–255 for 8-bit data, otherwise minimum to maximum).
    None,
    MinMax,
    /// Fixed bounds in the data's own units, whatever the layer's statistics. `bands` holds one
    /// `[min, max]` pair per RGB channel when they differ; `min`/`max` are then the red channel's.
    Custom {
        min: f64,
        max: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bands: Option<[[f64; 2]; 3]>,
    },
    /// Clip the lowest and highest percent of pixels.
    PercentClip {
        low: f64,
        high: f64,
    },
    /// Mean ± `n` standard deviations.
    StandardDeviation {
        n: f64,
    },
    /// Spread values evenly over the histogram.
    HistogramEqualize,
}

impl Default for Stretch {
    fn default() -> Self {
        Stretch::PercentClip { low: 0.5, high: 0.5 }
    }
}

impl Stretch {
    /// One pair of bounds for every band.
    pub fn custom(min: f64, max: f64) -> Stretch {
        Stretch::Custom { min, max, bands: None }
    }

    /// A custom stretch with a pair of bounds per RGB channel.
    pub fn custom_bands(bands: [[f64; 2]; 3]) -> Stretch {
        Stretch::Custom { min: bands[0][0], max: bands[0][1], bands: Some(bands) }
    }

    pub const KINDS: [(&'static str, &'static str); 6] = [
        ("none", "None"),
        ("minMax", "Minimum Maximum"),
        ("custom", "Custom Min/Max"),
        ("percentClip", "Percent Clip"),
        ("standardDeviation", "Standard Deviation"),
        ("histogramEqualize", "Histogram Equalize"),
    ];

    pub fn code(&self) -> &'static str {
        match self {
            Stretch::None => "none",
            Stretch::MinMax => "minMax",
            Stretch::Custom { .. } => "custom",
            Stretch::PercentClip { .. } => "percentClip",
            Stretch::StandardDeviation { .. } => "standardDeviation",
            Stretch::HistogramEqualize => "histogramEqualize",
        }
    }

    pub fn label(&self) -> &'static str {
        Stretch::KINDS.iter().find(|(c, _)| *c == self.code()).map(|(_, l)| *l).unwrap_or("Percent Clip")
    }

    /// A stretch by code, with optional parameters (`low`/`high` percent, `n` deviations,
    /// `min`/`max` bounds; a custom stretch missing a bound takes it from `stats`).
    pub fn from_code(
        code: &str,
        low: Option<f64>,
        high: Option<f64>,
        n: Option<f64>,
        min: Option<f64>,
        max: Option<f64>,
        stats: Option<&BandStats>,
    ) -> Option<Stretch> {
        let pct = |v: Option<f64>| v.unwrap_or(0.5).clamp(0.0, 49.0);
        Some(match code.trim().to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
            "none" => Stretch::None,
            "minmax" | "minimummaximum" => Stretch::MinMax,
            "custom" | "custommin/max" => Stretch::custom(min.or(stats.map(|s| s.min))?, max.or(stats.map(|s| s.max))?),
            "percentclip" => Stretch::PercentClip { low: pct(low), high: pct(high) },
            "standarddeviation" | "stddev" => Stretch::StandardDeviation { n: n.unwrap_or(2.0).clamp(0.1, 10.0) },
            "histogramequalize" | "histogramequalization" => Stretch::HistogramEqualize,
            _ => return None,
        })
    }
}

/// How screen pixels pick raster values.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Resampling {
    /// The nearest pixel: crisp cells, right for categories.
    #[default]
    Nearest,
    /// Blend the four nearest pixels: smooth, for continuous data.
    Bilinear,
}

impl Resampling {
    pub const ALL: [Resampling; 2] = [Resampling::Nearest, Resampling::Bilinear];
    pub fn label(self) -> &'static str {
        match self {
            Resampling::Nearest => "Nearest Neighbor",
            Resampling::Bilinear => "Bilinear",
        }
    }
    pub fn from_name(s: &str) -> Option<Resampling> {
        let s = s.trim().to_ascii_lowercase().replace([' ', '_', '-'], "");
        Resampling::ALL.into_iter().find(|r| {
            let l = r.label().to_ascii_lowercase().replace(' ', "");
            l == s || l.starts_with(&s) || format!("{r:?}").eq_ignore_ascii_case(&s)
        })
    }
}

/// What a raster layer draws.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RasterRenderer {
    /// One band through a colour ramp.
    Stretch { band: usize, ramp: Ramp, invert: bool },
    /// Three bands as red, green and blue.
    Rgb { red: usize, green: usize, blue: usize },
    /// Class breaks over one band.
    Classify {
        band: usize,
        method: ClassMethod,
        ramp: Ramp,
        breaks: Vec<f64>,
        /// The classes asked for: there can be fewer breaks, as repeated ones are dropped. 0 (a
        /// project from before it was kept) is as many as there are breaks.
        #[serde(default)]
        classes: usize,
    },
    /// A colour per whole-number value.
    UniqueValues { band: usize, ramp: Ramp, values: Vec<(f64, Rgba)> },
    /// The file's own colour table.
    Colormap,
    /// Hillshade, optionally tinted by elevation. `shadow` (0 to 1) is how dark the shadows the
    /// terrain casts are drawn (see [`crate::shadow`]); 0, the default, casts none. `sky` (0 to 1)
    /// is how much the share of the sky each cell sees scales its ambient light
    /// ([`crate::shadow::sky`]); 0, the default, leaves it whole. `fill` (0 to 1) is a fill light
    /// from the side opposite the sun, 55° up, that lightens only the slopes turned toward it; 0,
    /// the default, has none. `rock` tints steep slopes toward a rock colour before the lights
    /// shade them (see [`Rock`]); none, the default, keeps the ramp. `ambient` (0 to 1) is the
    /// share of the colour a slope turned fully from the sun keeps; none, the default, is
    /// [`relief_ambient`]'s. `balanced` divides the sun's light by its value on flat ground, so flat
    /// ground keeps the ramp's colour and slopes facing the sun brighten (#1977); off, the default,
    /// lights as before.
    ShadedRelief {
        band: usize,
        ramp: Option<Ramp>,
        azimuth: f64,
        altitude: f64,
        z_factor: f64,
        #[serde(default, skip_serializing_if = "is_no_light")]
        shadow: f64,
        #[serde(default, skip_serializing_if = "is_no_light")]
        sky: f64,
        #[serde(default, skip_serializing_if = "is_no_light")]
        fill: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rock: Option<Rock>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ambient: Option<f64>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        balanced: bool,
    },
}

impl RasterRenderer {
    pub const KINDS: [(&'static str, &'static str); 6] = [
        ("stretch", "Stretch"),
        ("rgb", "RGB"),
        ("classify", "Classify"),
        ("unique", "Unique Values"),
        ("colormap", "Colormap"),
        ("shadedRelief", "Shaded Relief"),
    ];

    pub fn code(&self) -> &'static str {
        match self {
            RasterRenderer::Stretch { .. } => "stretch",
            RasterRenderer::Rgb { .. } => "rgb",
            RasterRenderer::Classify { .. } => "classify",
            RasterRenderer::UniqueValues { .. } => "unique",
            RasterRenderer::Colormap => "colormap",
            RasterRenderer::ShadedRelief { .. } => "shadedRelief",
        }
    }

    pub fn label(&self) -> &'static str {
        RasterRenderer::KINDS.iter().find(|(c, _)| *c == self.code()).map(|(_, l)| *l).unwrap_or("Stretch")
    }

    /// Whether it draws classes (a colour table, unique values, class breaks), which a legend lists
    /// as the Layers pane does (#1679), rather than a ramp or bands.
    pub fn draws_classes(&self) -> bool {
        matches!(self, RasterRenderer::Classify { .. } | RasterRenderer::UniqueValues { .. } | RasterRenderer::Colormap)
    }

    /// The bands it reads.
    pub fn bands(&self) -> Vec<usize> {
        match self {
            RasterRenderer::Stretch { band, .. }
            | RasterRenderer::Classify { band, .. }
            | RasterRenderer::UniqueValues { band, .. }
            | RasterRenderer::ShadedRelief { band, .. } => vec![*band],
            RasterRenderer::Rgb { red, green, blue } => vec![*red, *green, *blue],
            RasterRenderer::Colormap => vec![0],
        }
    }

    pub fn ramp(&self) -> Option<Ramp> {
        match self {
            RasterRenderer::Stretch { ramp, .. } | RasterRenderer::Classify { ramp, .. } | RasterRenderer::UniqueValues { ramp, .. } => {
                Some(ramp.clone())
            }
            RasterRenderer::ShadedRelief { ramp, .. } => ramp.clone(),
            _ => None,
        }
    }
}

/// How a newly added single-band raster of continuous values is drawn: a colour ramp stretch with
/// these settings (`raster.setDefaultStyle`). The app keeps them with its preferences, not the
/// project; the default is a black-to-white grey stretch clipping 0.5 % at each end.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RasterDefaults {
    pub ramp: Ramp,
    pub invert: bool,
    /// Never [`Stretch::Custom`]: its bounds are one raster's own units.
    pub stretch: Stretch,
    pub resampling: Resampling,
}

impl Default for RasterDefaults {
    fn default() -> Self {
        RasterDefaults { ramp: Ramp::gray(), invert: false, stretch: Stretch::default(), resampling: Resampling::Nearest }
    }
}

impl RasterDefaults {
    /// Settings read from a file or preference, made safe to use: a custom stretch falls back to
    /// the default one and parameters are clamped to what `raster.stretch` accepts.
    pub fn sanitized(mut self) -> RasterDefaults {
        self.stretch = match self.stretch {
            Stretch::Custom { .. } => Stretch::default(),
            Stretch::PercentClip { low, high } => {
                let pct = |v: f64| if v.is_finite() { v.clamp(0.0, 49.0) } else { 0.5 };
                Stretch::PercentClip { low: pct(low), high: pct(high) }
            }
            Stretch::StandardDeviation { n } => Stretch::StandardDeviation { n: if n.is_finite() { n.clamp(0.1, 10.0) } else { 2.0 } },
            st => st,
        };
        self
    }
}

/// Most values given their own colour by Unique Values.
pub const MAX_UNIQUE_VALUES: usize = 256;

/// A raster layer's symbology.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RasterStyle {
    pub renderer: RasterRenderer,
    #[serde(default)]
    pub stretch: Stretch,
    /// Dynamic range adjustment: stretch statistics from the pixels in view.
    #[serde(default)]
    pub dra: bool,
    #[serde(default)]
    pub resampling: Resampling,
    /// −100 to 100.
    #[serde(default)]
    pub brightness: f64,
    /// −100 to 100.
    #[serde(default)]
    pub contrast: f64,
    /// 0.1 to 10; 1 leaves values unchanged.
    #[serde(default = "one")]
    pub gamma: f64,
    /// Display rotation in degrees clockwise, about the raster's centre.
    #[serde(default)]
    pub rotation: f64,
    /// Pixel values drawn as no data, on top of the file's own.
    #[serde(default, skip_serializing_if = "RasterMask::is_empty")]
    pub mask: RasterMask,
}

/// Pixel values hidden from view without changing the data: values to treat as no data (a
/// background of 0 or −9999 the file doesn't flag, QA flags, land cover classes), and values
/// outside (or inside) a range.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RasterMask {
    /// Sorted, without repeats (a project's list is sorted as it is read).
    #[serde(default, skip_serializing_if = "Vec::is_empty", deserialize_with = "sorted_values")]
    pub values: Vec<f64>,
    /// Values below `min` or above `max` are hidden; with `inside`, those from `min` to `max`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default)]
    pub inside: bool,
}

/// A mask's finite values, sorted and without repeats.
fn sorted_values<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
    let mut v: Vec<f64> = Vec::deserialize(d)?;
    v.retain(|x| x.is_finite());
    v.sort_by(f64::total_cmp);
    v.dedup();
    Ok(v)
}

/// Most pixels a band's masked statistics are worked out from.
const MASK_STATS_SAMPLE: usize = 1 << 22;

impl RasterMask {
    /// Whether nothing is hidden.
    pub fn is_empty(&self) -> bool {
        self.values.is_empty() && self.min.is_none() && self.max.is_none()
    }

    /// Whether the value `v` is hidden. Compared in f32, as the pixels are.
    pub fn hides(&self, v: f32) -> bool {
        // NaN is no data already.
        if self.is_empty() || v.is_nan() {
            return false;
        }
        // The values are sorted, and stay in order in f32 (+ 0.0 makes -0 equal to 0).
        let v = v + 0.0;
        if self.values.binary_search_by(|m| (*m as f32 + 0.0).total_cmp(&v)).is_ok() {
            return true;
        }
        if self.min.is_none() && self.max.is_none() {
            return false;
        }
        let v = v as f64;
        let within = self.min.is_none_or(|lo| v >= lo) && self.max.is_none_or(|hi| v <= hi);
        within == self.inside
    }

    /// Statistics of `band`'s valid pixels that the mask leaves, from a sample of a large band.
    /// A colour band's cells its alpha draws see-through are left out, as the band's own
    /// statistics leave them out (#1810).
    pub fn stats(&self, r: &Raster, band: usize) -> BandStats {
        let Some(b) = r.bands.get(band) else { return BandStats::default() };
        let step = b.data.len().div_ceil(MASK_STATS_SAMPLE).max(1);
        let hidden = r.see_through_cells(band);
        let kept: Vec<f32> = b
            .data
            .iter()
            .enumerate()
            .step_by(step)
            .map(|(i, v)| {
                let shown = r.is_valid(*v) && !self.hides(*v) && !hidden.as_ref().is_some_and(|h| h(i));
                if shown { *v } else { f32::NAN }
            })
            .collect();
        BandStats::from_values(&kept)
    }

    /// The mask's values and range in words, e.g. "0, 255; outside 1 to 100".
    pub fn label(&self) -> String {
        // Exactly, as they were given.
        let num = |v: f64| format!("{v}");
        let mut parts = Vec::new();
        if !self.values.is_empty() {
            parts.push(self.values.iter().map(|v| num(*v)).collect::<Vec<_>>().join(", "));
        }
        let range = match (self.min, self.max) {
            (Some(lo), Some(hi)) => Some(format!("{} {} to {}", if self.inside { "inside" } else { "outside" }, num(lo), num(hi))),
            (Some(lo), None) => Some(format!("{} {}", if self.inside { "≥" } else { "<" }, num(lo))),
            (None, Some(hi)) => Some(format!("{} {}", if self.inside { "≤" } else { ">" }, num(hi))),
            (None, None) => None,
        };
        parts.extend(range);
        parts.join("; ")
    }
}

fn one() -> f64 {
    1.0
}

/// What a frame draws a raster layer from ([`RasterLayer::look`]).
#[derive(Clone, Debug)]
pub struct RasterLook {
    path: Option<String>,
    subdataset: Option<String>,
    crs: Option<String>,
    time_zarr: Option<ZarrTime>,
    placement: Option<[f64; 4]>,
    style: RasterStyle,
    restyle: bool,
    look: Option<AddressLook>,
    /// The pixels, by allocation.
    data: Weak<Raster>,
}

impl RasterLook {
    /// Whether `other` draws as this did.
    pub fn same(&self, other: &RasterLook) -> bool {
        let RasterLook { path, subdataset, crs, time_zarr, placement, style, restyle, look, data } = self;
        *path == other.path
            && *subdataset == other.subdataset
            && *crs == other.crs
            && *time_zarr == other.time_zarr
            && placement.map(|p| p.map(f64::to_bits)) == other.placement.map(|p| p.map(f64::to_bits))
            && *style == other.style
            && *restyle == other.restyle
            && *look == other.look
            && Weak::ptr_eq(data, &other.data)
    }
}

/// The look an address gives a GeoTIFF read in place (`…/scene.tif#bands=5,3,2&range=0,3000`, as
/// tile layers kept it): the bands drawn (from 0; one in grey, or red, green and blue) and a fixed
/// stretch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AddressLook {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bands: Vec<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<(f64, f64)>,
}

impl RasterStyle {
    /// The style `look` gives `r`: its own look (see [`RasterStyle::default_for`]) with the bands
    /// and stretch the look names (bands it hasn't are left as they are); `None` for a look that
    /// names neither. A palette image stretched by a range, or whose address names bands, is
    /// drawn in grey, as its tile layer was.
    pub fn for_look(r: &Raster, look: &AddressLook) -> Option<RasterStyle> {
        if look.bands.is_empty() && look.range.is_none() {
            return None;
        }
        // Bands the file hasn't: its own look (said by `look_lost`); a palette image in grey, as
        // its tile layer draws one whose address names bands (its colours are of one band alone).
        if Self::look_lost(r, look).is_some() {
            let mut style = RasterStyle::default_for(r);
            if matches!(style.renderer, RasterRenderer::Colormap) {
                style.renderer = RasterRenderer::Stretch { band: 0, ramp: Ramp::gray(), invert: false };
                style.stretch = Stretch::default();
            }
            if let Some((lo, hi)) = look.range {
                style.stretch = Stretch::custom(lo, hi);
            }
            return Some(style);
        }
        let mut style = RasterStyle::default_for(r);
        let n = r.band_count();
        match look.bands[..] {
            [b] if b < n => style.renderer = RasterRenderer::Stretch { band: b, ramp: Ramp::gray(), invert: false },
            [red, green, blue] if red < n && green < n && blue < n => style.renderer = RasterRenderer::Rgb { red, green, blue },
            [] if matches!(style.renderer, RasterRenderer::Colormap) => {
                style.renderer = RasterRenderer::Stretch { band: 0, ramp: Ramp::gray(), invert: false };
            }
            _ => {}
        }
        if let Some((lo, hi)) = look.range {
            style.stretch = Stretch::custom(lo, hi);
        }
        Some(style)
    }

    /// What a look asks for that `r` can't draw: bands past its own, or two bands (one is drawn
    /// in grey, three as red, green and blue), so the file's own look is drawn instead; `None`
    /// when the look fits.
    pub fn look_lost(r: &Raster, look: &AddressLook) -> Option<String> {
        let n = r.band_count();
        // A palette image's colours are of one band alone: in grey, as its tile layer drew it.
        let own = if matches!(r.color, ColorHint::Palette(_)) { "drawn in grey" } else { "drawn in its own look" };
        let past: Vec<String> = look.bands.iter().filter(|b| **b >= n).map(|b| b.saturating_add(1).to_string()).collect();
        if !past.is_empty() {
            return Some(format!("its address asks for band(s) {} (from 1), which it hasn't ({n} band(s)): {own}", past.join(", ")));
        }
        // An address written by hand: `map.setTiles` takes one band or three.
        (!matches!(look.bands.len(), 0 | 1 | 3))
            .then(|| format!("its address asks for {} bands; one is drawn in grey, three as red, green and blue: {own}", look.bands.len()))
    }

    /// A style for a layer whose pixels haven't arrived (its own look replaces it when they do).
    pub fn placeholder() -> RasterStyle {
        RasterStyle {
            renderer: RasterRenderer::Stretch { band: 0, ramp: Ramp::gray(), invert: false },
            stretch: Stretch::default(),
            dra: false,
            resampling: Resampling::Nearest,
            brightness: 0.0,
            contrast: 0.0,
            gamma: 1.0,
            rotation: 0.0,
            mask: RasterMask::default(),
        }
    }

    /// The default look, as the file's PhotometricInterpretation says: the file's colours, RGB for
    /// colour images and for grey ones of exactly three bands (most remote sensing images are
    /// grey, whatever their bands: three are taken as red, green and blue), unique values for a
    /// few categories, otherwise a grey stretch of the first band (an image of more bands is
    /// combined as the user picks).
    pub fn default_for(r: &Raster) -> RasterStyle {
        RasterStyle::default_with(r, &RasterDefaults::default())
    }

    /// The default look, with `d` for a single band of continuous values in place of the grey
    /// stretch.
    pub fn default_with(r: &Raster, d: &RasterDefaults) -> RasterStyle {
        let byte_rgb = r.data_type == "UInt8";
        let (renderer, stretch) = match &r.color {
            ColorHint::Palette(_) => (RasterRenderer::Colormap, Stretch::None),
            ColorHint::Rgb { .. } if r.band_count() >= 3 => {
                (RasterRenderer::Rgb { red: 0, green: 1, blue: 2 }, if byte_rgb { Stretch::None } else { Stretch::default() })
            }
            ColorHint::Gray if r.band_count() == 3 => (RasterRenderer::Rgb { red: 0, green: 1, blue: 2 }, Stretch::default()),
            _ => match r.bands.first().and_then(|b| b.stats.categories.as_ref()).filter(|c| c.len() <= 16 && r.data_type.contains("Int")) {
                Some(_) => (unique_values(r, 0, Ramp::Harbour), Stretch::None),
                None => (RasterRenderer::Stretch { band: 0, ramp: d.ramp.clone(), invert: d.invert }, d.stretch),
            },
        };
        let continuous = matches!(renderer, RasterRenderer::Stretch { .. });
        RasterStyle {
            renderer,
            stretch,
            dra: false,
            resampling: if continuous { d.resampling } else { Resampling::Nearest },
            brightness: 0.0,
            contrast: 0.0,
            gamma: 1.0,
            rotation: 0.0,
            mask: RasterMask::default(),
        }
    }

    /// Flips a Stretch's colour scheme end for end (#1355). Returns false for other renderers.
    pub fn invert_ramp(&mut self) -> bool {
        match &mut self.renderer {
            RasterRenderer::Stretch { invert, .. } => {
                *invert = !*invert;
                true
            }
            _ => false,
        }
    }

    /// Recolours with a colour scheme.
    pub fn set_ramp(&mut self, ramp: Ramp) {
        match &mut self.renderer {
            RasterRenderer::Stretch { ramp: r, .. } | RasterRenderer::Classify { ramp: r, .. } => *r = ramp,
            RasterRenderer::ShadedRelief { ramp: r, .. } => *r = Some(ramp),
            RasterRenderer::UniqueValues { ramp: r, values, .. } => {
                let n = values.len();
                for (i, (_, c)) in values.iter_mut().enumerate() {
                    *c = ramp.pick(i, n);
                }
                *r = ramp;
            }
            RasterRenderer::Rgb { .. } | RasterRenderer::Colormap => {}
        }
    }

    /// Keeps band numbers inside the raster (after it changes on disk).
    pub fn repair(&mut self, r: &Raster) {
        let last = r.band_count().saturating_sub(1);
        match &mut self.renderer {
            RasterRenderer::Stretch { band, .. }
            | RasterRenderer::Classify { band, .. }
            | RasterRenderer::UniqueValues { band, .. }
            | RasterRenderer::ShadedRelief { band, .. } => *band = (*band).min(last),
            RasterRenderer::Rgb { red, green, blue } => {
                for b in [red, green, blue] {
                    *b = (*b).min(last);
                }
            }
            RasterRenderer::Colormap => {
                if !matches!(r.color, ColorHint::Palette(_)) {
                    *self = RasterStyle::default_for(r);
                }
            }
        }
    }
}

fn sorted_sample_masked(r: &Raster, band: usize, max: usize, mask: &RasterMask) -> Vec<f64> {
    let Some(b) = r.bands.get(band) else { return Vec::new() };
    let step = (b.data.len() / max.max(1)).max(1);
    let mut v: Vec<f64> = b.data.iter().step_by(step).filter(|v| r.is_valid(**v) && !mask.hides(**v)).map(|v| *v as f64).collect();
    v.sort_by(|a, b| a.total_cmp(b));
    v
}

/// A Classify renderer with `classes` breaks.
pub fn classify(r: &Raster, band: usize, classes: usize, method: ClassMethod, ramp: Ramp) -> RasterRenderer {
    classify_masked(r, band, classes, method, ramp, &RasterMask::default())
}

/// [`classify`] over the pixels `mask` leaves.
pub fn classify_masked(r: &Raster, band: usize, classes: usize, method: ClassMethod, ramp: Ramp, mask: &RasterMask) -> RasterRenderer {
    let sorted = sorted_sample_masked(r, band, 50_000, mask);
    let classes = classes.clamp(1, 32);
    RasterRenderer::Classify { band, method, ramp, breaks: class_breaks(&sorted, classes, method), classes }
}

/// A Unique Values renderer over a band's distinct whole numbers (at most [`MAX_UNIQUE_VALUES`]).
pub fn unique_values(r: &Raster, band: usize, ramp: Ramp) -> RasterRenderer {
    let cats = r.bands.get(band).and_then(|b| b.stats.categories.clone()).unwrap_or_default();
    let n = cats.len().min(MAX_UNIQUE_VALUES);
    let values = cats.into_iter().take(n).enumerate().map(|(i, v)| (v, ramp.pick(i, n))).collect();
    RasterRenderer::UniqueValues { band, ramp, values }
}

/// Maps a band's values to 0–1 for a stretch.
#[derive(Clone, Debug)]
pub struct Normalizer {
    pub lo: f64,
    pub hi: f64,
    /// Cumulative histogram (0–1) for histogram equalisation, over `lo`..`hi`.
    cdf: Option<Vec<f64>>,
}

impl Normalizer {
    pub fn new(stretch: Stretch, s: &BandStats, data_type: &str) -> Normalizer {
        Normalizer::for_channel(stretch, 0, s, data_type)
    }

    /// The normaliser of renderer channel `channel` (red 0, green 1, blue 2): the same as
    /// [`Normalizer::new`], but a custom stretch with bounds per channel uses that channel's.
    pub fn for_channel(stretch: Stretch, channel: usize, s: &BandStats, data_type: &str) -> Normalizer {
        let (lo, hi) = match stretch {
            Stretch::None if data_type == "UInt8" => (0.0, 255.0),
            Stretch::None | Stretch::MinMax | Stretch::HistogramEqualize => (s.min, s.max),
            // Fixed bounds ignore the statistics, so DRA's view statistics don't move them.
            Stretch::Custom { min, max, bands } => match bands.as_ref().and_then(|b| b.get(channel)) {
                Some([lo, hi]) => (*lo, *hi),
                None => (min, max),
            },
            Stretch::PercentClip { low, high } => (s.percentile(low), s.percentile(100.0 - high)),
            Stretch::StandardDeviation { n } => ((s.mean - n * s.std_dev).max(s.min), (s.mean + n * s.std_dev).min(s.max)),
        };
        let cdf = (stretch == Stretch::HistogramEqualize && !s.histogram.is_empty()).then(|| {
            let total: u64 = s.histogram.iter().sum::<u64>().max(1);
            let mut acc = 0u64;
            s.histogram
                .iter()
                .map(|c| {
                    acc += c;
                    acc as f64 / total as f64
                })
                .collect()
        });
        let (lo, hi) = if lo.is_finite() && hi.is_finite() { (lo, hi) } else { (0.0, 1.0) };
        Normalizer { lo, hi, cdf }
    }

    /// `v` scaled to 0–1 (clamped).
    pub fn t(&self, v: f64) -> f64 {
        let span = self.hi - self.lo;
        let x = if span > 0.0 { (v - self.lo) / span } else { 0.5 };
        let x = x.clamp(0.0, 1.0);
        match &self.cdf {
            Some(cdf) if !cdf.is_empty() => {
                let i = (x * (cdf.len() - 1) as f64).round() as usize;
                cdf.get(i.min(cdf.len() - 1)).copied().unwrap_or(x)
            }
            _ => x,
        }
    }
}

/// Brightness, contrast and gamma as a lookup table on 0–255 channel values.
pub fn enhancement_lut(brightness: f64, contrast: f64, gamma: f64) -> [u8; 256] {
    let mut lut = [0u8; 256];
    let b = brightness.clamp(-100.0, 100.0) / 100.0 * 255.0;
    let c = contrast.clamp(-100.0, 100.0) / 100.0;
    let k = if c >= 0.0 { 1.0 + c * 3.0 } else { 1.0 + c };
    let g = if gamma.is_finite() { gamma.clamp(0.1, 10.0) } else { 1.0 };
    for (i, out) in lut.iter_mut().enumerate() {
        let x = ((i as f64 - 127.5) * k + 127.5 + b).clamp(0.0, 255.0) / 255.0;
        *out = (x.powf(1.0 / g) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    lut
}

/// Colours pixel values for one draw of a raster layer.
pub struct Colorizer<'a> {
    style: &'a RasterStyle,
    norms: Vec<Normalizer>,
    /// Each band's byte for the whole values 0–255 (8-bit images), looked up instead of computed.
    bytes: Vec<[u8; 256]>,
    lut: [u8; 256],
    plain: bool,
    ramp_lut: Vec<[u8; 4]>,
    palette: &'a [[u8; 4]],
}

impl<'a> Colorizer<'a> {
    /// `view_stats` are statistics of the pixels in view, one per renderer band, for DRA.
    pub fn new(style: &'a RasterStyle, r: &'a Raster, view_stats: Option<&[BandStats]>) -> Colorizer<'a> {
        let bands = style.renderer.bands();
        let norms: Vec<Normalizer> = bands
            .iter()
            .enumerate()
            .map(|(i, b)| {
                let s = view_stats.and_then(|v| v.get(i)).or_else(|| r.bands.get(*b).map(|b| &b.stats));
                s.map(|s| Normalizer::for_channel(style.stretch, i, s, &r.data_type)).unwrap_or(Normalizer { lo: 0.0, hi: 1.0, cdf: None })
            })
            .collect();
        let ramp_lut = match &style.renderer {
            RasterRenderer::Stretch { ramp, invert, .. } => {
                (0..256).map(|i| ramp.at(if *invert { 1.0 - i as f64 / 255.0 } else { i as f64 / 255.0 }).0).collect()
            }
            RasterRenderer::ShadedRelief { ramp: Some(ramp), .. } => (0..256).map(|i| ramp.at(i as f64 / 255.0).0).collect(),
            _ => Vec::new(),
        };
        let palette: &[[u8; 4]] = match &r.color {
            ColorHint::Palette(p) => p,
            _ => &[],
        };
        let plain = style.brightness == 0.0 && style.contrast == 0.0 && (style.gamma - 1.0).abs() < 1e-9;
        let bytes = norms.iter().map(|n| std::array::from_fn(|i| Self::normalize(n, i as f32))).collect();
        Colorizer { style, norms, bytes, lut: enhancement_lut(style.brightness, style.contrast, style.gamma), plain, ramp_lut, palette }
    }

    fn normalize(n: &Normalizer, v: f32) -> u8 {
        (n.t(v as f64) * 255.0).round() as u8
    }

    fn byte(&self, band: usize, v: f32) -> u8 {
        if (0.0..=255.0).contains(&v)
            && v.fract() == 0.0
            && let Some(b) = self.bytes.get(band).and_then(|l| l.get(v as usize))
        {
            return *b;
        }
        self.norms.get(band).map(|n| Self::normalize(n, v)).unwrap_or(0)
    }

    fn enhance(&self, c: [u8; 4]) -> [u8; 4] {
        if self.plain {
            return c;
        }
        let l = |x: u8| self.lut[x as usize];
        [l(c[0]), l(c[1]), l(c[2]), c[3]]
    }

    /// The colour of a pixel from its renderer band values (`NaN` = no data), or `None` for
    /// transparent. `hillshade` (0–255) is used by Shaded Relief.
    pub fn color(&self, vals: &[f32], hillshade: Option<u8>) -> Option<[u8; 4]> {
        self.color_lit(vals, hillshade, None, None, None)
    }

    /// [`Colorizer::color`], with Shaded Relief's ambient light scaled by `sky` (0–255, the
    /// share of it the sky a cell sees leaves, see [`RasterRenderer::ShadedRelief`]'s `sky`),
    /// `fill` (0–255, a share of the colour) added for its fill light (its `fill`), and its colour
    /// moved `rock` (0–255) of the way to its rock colour (its `rock`, see [`Rock::share`]).
    pub fn color_lit(&self, vals: &[f32], hillshade: Option<u8>, sky: Option<u8>, fill: Option<u8>, rock: Option<u8>) -> Option<[u8; 4]> {
        // The renderer masks values as it samples them (they arrive as NaN); this is for other callers.
        let mask = &self.style.mask;
        if mask.is_empty() || !vals.iter().any(|v| mask.hides(*v)) {
            return self.paint(vals, hillshade, sky, fill, rock);
        }
        // Masked values are no data. The renderer bands are at most three.
        let mut kept = [f32::NAN; 3];
        for (k, v) in kept.iter_mut().zip(vals) {
            if !mask.hides(*v) {
                *k = *v;
            }
        }
        self.paint(kept.get(..vals.len().min(3)).unwrap_or(&kept), hillshade, sky, fill, rock)
    }

    fn paint(&self, vals: &[f32], hillshade: Option<u8>, sky: Option<u8>, fill: Option<u8>, rock: Option<u8>) -> Option<[u8; 4]> {
        let v0 = *vals.first()?;
        let c = match &self.style.renderer {
            RasterRenderer::Rgb { .. } => {
                if vals.iter().take(3).all(|v| v.is_nan()) {
                    return None;
                }
                let g = |i: usize| vals.get(i).filter(|v| !v.is_nan()).map(|v| self.byte(i, *v)).unwrap_or(0);
                [g(0), g(1), g(2), 255]
            }
            _ if v0.is_nan() => return None,
            RasterRenderer::Stretch { .. } => *self.ramp_lut.get(self.byte(0, v0) as usize)?,
            RasterRenderer::Classify { ramp, breaks, .. } => {
                let k = breaks.iter().position(|b| v0 as f64 <= *b).unwrap_or(breaks.len().saturating_sub(1));
                ramp.pick(k, breaks.len()).0
            }
            RasterRenderer::UniqueValues { values, .. } => {
                let i = values.binary_search_by(|(v, _)| v.total_cmp(&(v0 as f64))).ok()?;
                values.get(i)?.1.0
            }
            RasterRenderer::Colormap => {
                let i = v0 as i64;
                if i < 0 {
                    return None;
                }
                *self.palette.get(i as usize)?
            }
            RasterRenderer::ShadedRelief { ramp, rock: steep, altitude, ambient: setting, balanced, .. } => {
                let h = hillshade.unwrap_or(180) as f64 / 255.0;
                let base = if ramp.is_some() { *self.ramp_lut.get(self.byte(0, v0) as usize)? } else { [255, 255, 255, 255] };
                // Steep ground first takes on the rock colour; the light then shades either.
                let base = match (steep, rock) {
                    (Some(steep), Some(share)) if share > 0 => Rgba(base).lerp(steep.color.with_alpha(255), f64::from(share) / 255.0).0,
                    _ => base,
                };
                // Multiply the tint by the hillshade, keeping some ambient light (less of it where
                // the terrain hides part of the sky), and adding the fill light on its own.
                let share = relief_ambient(*setting, *balanced);
                let a = share * sky.map_or(1.0, |s| f64::from(s) / 255.0);
                let f = fill.map_or(0.0, |f| f64::from(f) / 255.0);
                // Flat ground's hillshade, rounded as the hillshade is: 0 for a sun on the horizon,
                // which lights no flat ground to balance against, so that is lit as unbalanced.
                let flat = (255.0 * altitude.clamp(0.0, 90.0).to_radians().sin()).round();
                let lit = if *balanced && flat > 0.0 {
                    // The sun's light over its value on flat ground: flat ground keeps the ramp's
                    // colour, slopes facing the sun brighten.
                    (a + (1.0 - share) * h * 255.0 / flat).min(BALANCED_MAX)
                } else {
                    a + (1.0 - share) * h
                };
                let m = |c: u8| (c as f64 * (lit + f)).round().clamp(0.0, 255.0) as u8;
                [m(base[0]), m(base[1]), m(base[2]), 255]
            }
        };
        (c[3] > 0).then(|| self.enhance(c))
    }
}

/// The slope (degrees) of a 3×3 window of elevations (row by row), from the gradient
/// [`hillshade`] uses: the ground's own, with no Z factor.
pub fn slope(z: &[f64; 9], cell_x: f64, cell_y: f64) -> f64 {
    let (dzdx, dzdy) = horn(z, cell_x, cell_y);
    dzdx.hypot(dzdy).atan().to_degrees()
}

/// The east and south gradients of a 3×3 window of elevations, Horn's method.
fn horn(z: &[f64; 9], cell_x: f64, cell_y: f64) -> (f64, f64) {
    let [a, b, c, d, _, f, g, h, i] = *z;
    let dzdx = ((c + 2.0 * f + i) - (a + 2.0 * d + g)) / (8.0 * cell_x.max(1e-9));
    let dzdy = ((g + 2.0 * h + i) - (a + 2.0 * b + c)) / (8.0 * cell_y.max(1e-9));
    (dzdx, dzdy)
}

/// Hillshade (0–255) from a 3×3 window of elevations (row by row), Horn's method.
pub fn hillshade(z: &[f64; 9], cell_x: f64, cell_y: f64, azimuth: f64, altitude: f64, z_factor: f64) -> u8 {
    let (dzdx, dzdy) = horn(z, cell_x, cell_y);
    let tilt = (z_factor * (dzdx * dzdx + dzdy * dzdy).sqrt()).atan();
    let aspect = if dzdx != 0.0 || dzdy != 0.0 { dzdy.atan2(-dzdx) } else { 0.0 };
    let zenith = (90.0 - altitude.clamp(0.0, 90.0)).to_radians();
    let az = (360.0 - azimuth + 90.0).rem_euclid(360.0).to_radians();
    let v = zenith.cos() * tilt.cos() + zenith.sin() * tilt.sin() * (az - aspect).cos();
    (v.max(0.0) * 255.0).round().clamp(0.0, 255.0) as u8
}

/// One legend row for a raster layer.
#[derive(Clone, Debug, PartialEq)]
pub enum RasterLegend {
    /// A continuous ramp from `low` to `high`.
    Ramp { ramp: Ramp, invert: bool, high: String, low: String },
    /// A swatch and label.
    Class { color: Rgba, label: String },
    /// A band shown as a primary colour.
    Channel { color: Rgba, label: String },
}

impl RasterLayer {
    /// Legend rows for the Layers pane.
    pub fn legend(&self) -> Vec<RasterLegend> {
        let Some(r) = &self.data else { return Vec::new() };
        let band_name = |b: usize| r.bands.get(b).map(|b| b.name.clone()).unwrap_or_default();
        // A class's name in the file (GDAL's category names), or else its value.
        let class_label = |b: usize, v: f64| r.bands.get(b).and_then(|b| b.class_name(v)).map_or_else(|| fmt_num(v), str::to_owned);
        // The first renderer band's statistics, without the masked pixels.
        let masked = self.masked_stats();
        let stats = |band: usize| masked.as_ref().and_then(|m| m.first()).or_else(|| r.bands.get(band).map(|b| &b.stats));
        let mask = &self.style.mask;
        match &self.style.renderer {
            RasterRenderer::Stretch { band, ramp, invert } => {
                let n = stats(*band).map(|s| Normalizer::new(self.style.stretch, s, &r.data_type));
                let (lo, hi) = n.map(|n| (n.lo, n.hi)).unwrap_or((0.0, 1.0));
                vec![RasterLegend::Ramp { ramp: ramp.clone(), invert: *invert, high: fmt_num(hi), low: fmt_num(lo) }]
            }
            RasterRenderer::ShadedRelief { ramp: Some(ramp), band, .. } => {
                let s = stats(*band).map(|s| (s.min, s.max)).unwrap_or((0.0, 1.0));
                vec![RasterLegend::Ramp { ramp: ramp.clone(), invert: false, high: fmt_num(s.1), low: fmt_num(s.0) }]
            }
            RasterRenderer::ShadedRelief { ramp: None, .. } => {
                vec![RasterLegend::Ramp { ramp: Ramp::Greys, invert: true, high: "Lit".into(), low: "Shaded".into() }]
            }
            RasterRenderer::Rgb { red, green, blue } => vec![
                RasterLegend::Channel { color: Rgba::rgb(0xE5, 0x39, 0x35), label: band_name(*red) },
                RasterLegend::Channel { color: Rgba::rgb(0x43, 0xA0, 0x47), label: band_name(*green) },
                RasterLegend::Channel { color: Rgba::rgb(0x1E, 0x88, 0xE5), label: band_name(*blue) },
            ],
            RasterRenderer::Classify { ramp, breaks, band, .. } => {
                let mut lo = stats(*band).map(|s| s.min).unwrap_or(0.0);
                breaks
                    .iter()
                    .enumerate()
                    .map(|(i, b)| {
                        let label = format!("{} – {}", fmt_num(lo), fmt_num(*b));
                        lo = *b;
                        RasterLegend::Class { color: ramp.pick(i, breaks.len()), label }
                    })
                    .collect()
            }
            RasterRenderer::UniqueValues { band, values, .. } => values
                .iter()
                .filter(|(v, _)| !mask.hides(*v as f32))
                .map(|(v, c)| RasterLegend::Class { color: *c, label: class_label(*band, *v) })
                .collect(),
            RasterRenderer::Colormap => {
                let ColorHint::Palette(p) = &r.color else { return Vec::new() };
                let cats = r.bands.first().and_then(|b| b.stats.categories.clone()).unwrap_or_default();
                // A negative value has no entry in the table, and the map draws none for it, nor
                // for an entry that is fully transparent (a background value).
                cats.iter()
                    .filter(|v| **v >= 0.0 && !mask.hides(**v as f32))
                    .filter_map(|v| {
                        p.get(*v as usize).filter(|c| c[3] > 0).map(|c| RasterLegend::Class { color: Rgba(*c), label: class_label(0, *v) })
                    })
                    .collect()
            }
        }
    }

    /// Whether it is drawn by its colour table over values its statistics don't list (over 256
    /// distinct values, or values that aren't whole): [`RasterLayer::legend_classes`] then has no
    /// class to give, and a colorbar none either (#1679).
    pub fn classes_unknown(&self) -> bool {
        let Some(r) = self.data.as_ref().filter(|_| matches!(self.style.renderer, RasterRenderer::Colormap)) else { return false };
        // Under a mask, the values it leaves may be few enough to list.
        let masked = self.masked_stats();
        let stats = masked.as_ref().and_then(|m| m.first()).or_else(|| r.bands.first().map(|b| &b.stats));
        stats.is_some_and(|s| s.count > 0 && s.categories.is_none())
    }

    /// Whether its mask hides every pixel its masked statistics read: a legend then lists no class
    /// the raster holds, so none can be missing from a sample or an overview (#1679).
    fn mask_hides_all(&self) -> bool {
        self.masked_stats().is_some_and(|m| m.first().is_some_and(|s| s.count == 0))
    }

    /// Unique Values without an enumerated statistics list shows every configured value, just
    /// as an explicit request for all classes does. None of those classes can be missed.
    fn lists_every_class(&self, all_classes: bool) -> bool {
        match &self.style.renderer {
            RasterRenderer::UniqueValues { band, .. } => {
                if all_classes {
                    return true;
                }
                let masked = self.masked_stats();
                let stats = masked.as_ref().and_then(|m| m.first()).or_else(|| self.data.as_ref()?.bands.get(*band).map(|b| &b.stats));
                stats.is_none_or(|s| s.categories.is_none())
            }
            RasterRenderer::Classify { .. } => all_classes,
            _ => false,
        }
    }

    /// Whether the classes a legend lists for it ([`RasterLayer::legend_classes`]) come from a
    /// sample of its pixels: under a mask, a band of more than [`MASK_STATS_SAMPLE`] cells has its
    /// statistics read from every few cells ([`RasterMask::stats`]), so a class too rare for the
    /// sample can be missing (#1679). Not with `all_classes` for Unique Values or Classify, nor when
    /// no class can be listed (a mask hiding every pixel included). Unique Values without an
    /// enumerated statistics list shows every class. A colour table's come from the sample only
    /// when its own statistics, read whole, can't list its values.
    pub fn classes_from_sample(&self, all_classes: bool) -> bool {
        let Some(r) = &self.data else { return false };
        if !self.style.renderer.draws_classes() || self.style.mask.is_empty() || self.classes_unknown() || self.mask_hides_all() {
            return false;
        }
        let band = self.style.renderer.bands().first().copied().unwrap_or(0);
        let sampled = r.bands.get(band).is_some_and(|b| b.data.len() > MASK_STATS_SAMPLE);
        let from_sample = match &self.style.renderer {
            RasterRenderer::Colormap => r.bands.first().is_some_and(|b| b.stats.categories.is_none()),
            _ => !self.lists_every_class(all_classes),
        };
        sampled && from_sample
    }

    /// Whether the classes a legend lists for it ([`RasterLayer::legend_classes`]) come from the
    /// statistics of an overview (a cloud-optimised GeoTIFF read in place, or pixels read reduced)
    /// while the map draws the full resolution: a class too rare for the overview can be missing
    /// (#1679). Not with `all_classes` for Unique Values or Classify, whose every class is listed
    /// whatever the statistics, nor when no class can be listed ([`RasterLayer::classes_unknown`],
    /// or a mask hiding every pixel), or when Unique Values lists every configured class because
    /// its statistics have no enumerated values.
    pub fn classes_from_overview(&self, all_classes: bool) -> bool {
        let Some(r) = &self.data else { return false };
        let every = self.lists_every_class(all_classes);
        self.style.renderer.draws_classes()
            && (self.in_place || r.overview.is_some())
            && !every
            && !self.classes_unknown()
            && !self.mask_hides_all()
    }

    /// The classes a layout's legend lists for this raster (#1679): the Layers pane's class rows
    /// ([`RasterLayer::legend`]), a colour and a label each, in the renderer's order. `None` for a
    /// renderer that draws no classes (a stretch, a hillshade, RGB bands), which a colorbar keys.
    /// Without `all_classes`, the classes the raster holds: those of the values in its statistics
    /// (without the masked pixels). A raster with too many values to list (over 256, or not whole)
    /// gives every Unique Values class, for Classify the classes from its lowest value's to its
    /// highest's, and for a colour table those a mask leaves, when they are few enough, else none
    /// ([`RasterLayer::classes_unknown`]). With `all_classes`, every class of a Unique Values or
    /// Classify renderer. A colour table's 256 entries have no meaningful "all": it lists the
    /// values the raster holds either way, as the Layers pane does.
    pub fn legend_classes(&self, all_classes: bool) -> Option<Vec<(Rgba, String)>> {
        let classes = || {
            self.legend().into_iter().filter_map(|row| match row {
                RasterLegend::Class { color, label } => Some((color, label)),
                _ => None,
            })
        };
        if !self.style.renderer.draws_classes() {
            return None;
        }
        // Not read (yet): no class known.
        let Some(r) = self.data.as_ref() else { return Some(Vec::new()) };
        let masked = self.masked_stats();
        let stats = |band: usize| masked.as_ref().and_then(|m| m.first()).or_else(|| r.bands.get(band).map(|b| &b.stats));
        match &self.style.renderer {
            RasterRenderer::Colormap => {
                // The pane lists the values in the band's own statistics. With too many there, a
                // mask may leave few enough: those, coloured and named as the pane does.
                let own = r.bands.first().is_some_and(|b| b.stats.categories.is_some());
                match (&r.color, stats(0).and_then(|s| s.categories.as_ref())) {
                    (ColorHint::Palette(p), Some(left)) if !own => {
                        let label = |v: f64| r.bands.first().and_then(|b| b.class_name(v)).map_or_else(|| fmt_num(v), str::to_owned);
                        let shown = left.iter().filter(|v| **v >= 0.0 && !self.style.mask.hides(**v as f32));
                        Some(shown.filter_map(|v| p.get(*v as usize).filter(|c| c[3] > 0).map(|c| (Rgba(*c), label(*v)))).collect())
                    }
                    _ => Some(classes().collect()),
                }
            }
            RasterRenderer::UniqueValues { band, values, .. } => {
                // A mask that hides every pixel leaves nothing drawn, so nothing to list.
                if !all_classes && stats(*band).is_some_and(|s| s.count == 0) {
                    return Some(Vec::new());
                }
                let held = stats(*band).and_then(|s| s.categories.as_ref()).filter(|_| !all_classes);
                // The pane's rows are the values its mask doesn't hide, one each, in order.
                let shown = values.iter().filter(|(v, _)| !self.style.mask.hides(*v as f32));
                Some(shown.zip(classes()).filter(|((v, _), _)| held.is_none_or(|held| held.contains(v))).map(|(_, class)| class).collect())
            }
            RasterRenderer::Classify { band, breaks, .. } => {
                // The class a value is drawn in, as the map draws it: the first break at or above
                // it, else the last class.
                let class_of = |v: f64| breaks.iter().position(|b| v <= *b).unwrap_or(breaks.len().saturating_sub(1));
                let held: Option<Vec<usize>> = match stats(*band) {
                    _ if all_classes => None,
                    Some(s) if s.count == 0 => Some(Vec::new()),
                    Some(s) => Some(match &s.categories {
                        Some(values) => values.iter().map(|v| class_of(*v)).collect(),
                        None => (class_of(s.min)..=class_of(s.max)).collect(),
                    }),
                    None => None,
                };
                // A flag per class, so a file with very many breaks costs one pass, not one per row.
                let keep = held.map(|held| {
                    let mut keep = vec![false; breaks.len()];
                    for i in held {
                        if let Some(k) = keep.get_mut(i) {
                            *k = true;
                        }
                    }
                    keep
                });
                let kept = |i: usize| keep.as_ref().is_none_or(|keep| keep.get(i).copied().unwrap_or(false));
                Some(classes().enumerate().filter(|(i, _)| kept(*i)).map(|(_, c)| c).collect())
            }
            RasterRenderer::Stretch { .. } | RasterRenderer::Rgb { .. } | RasterRenderer::ShadedRelief { .. } => None,
        }
    }
}

impl RasterLayer {
    /// Only owning address/path strings are offered; embedded bytes and render state stay opaque.
    pub(crate) fn output_slots(&self) -> Vec<(crate::LayerRole, &str, bool, bool)> {
        let RasterLayer {
            path,
            file_name: _,
            embedded,
            subdataset: _,
            crs: _,
            time_zarr,
            placement: _,
            style: _,
            data: _,
            error: _,
            masked: _,
            cast: _,
            opening: _,
            read_still: _,
            read_generation: _,
            in_place: _,
            restyle: _,
            look: _,
            pixels_said: _,
            error_said: _,
            failed_at: _,
            failures: _,
            lost_said: _,
            look_lost: _,
            dropped: _,
        } = self;
        let mut out = Vec::new();
        if let Some(p) = path {
            out.push((crate::LayerRole::Raster, p.as_str(), true, embedded.is_none() && crate::paths::is_local(p)));
        }
        if let Some(ZarrTime {
            store,
            variable: _,
            index: _,
            dim: _,
            offset: _,
            dates: _,
            crs: _,
            window: _,
            shown: _,
            read_generation: _,
        }) = time_zarr
        {
            out.push((crate::LayerRole::ZarrStore, store.as_str(), true, crate::paths::is_local(store)));
        }
        out
    }

    /// Looks up a prepared address/path field without allocation.
    pub(crate) fn output_slot_mut(&mut self, role: crate::LayerRole) -> Option<&mut String> {
        match role {
            crate::LayerRole::Raster => self.path.as_mut(),
            crate::LayerRole::ZarrStore => Some(&mut self.time_zarr.as_mut()?.store),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    /// A completed read can wait for a render copy or the blocking consumer. Revocation
    /// between callback and application must refuse both consumers of the shared slot.
    #[test]
    fn permission_policy_completed_raster_checks_lifetime_at_application() {
        use rustgis_data::source::remote;
        use std::sync::atomic::{AtomicBool, Ordering::SeqCst};
        let previous = in_place_reader().lock().unwrap().take();
        set_in_place_reader(|_, done| done(Ok(dem())));
        let mut failures = Vec::new();
        for revoke in [false, true] {
            let live = Arc::new(AtomicBool::new(true));
            let flag = live.clone();
            let still: remote::Still = Arc::new(move || flag.load(SeqCst));
            let mut layer = remote::with_still(still, || RasterLayer::opening_in_place("https://raster-landing.invalid/scene.tif"));
            assert!(layer.data.is_none() && layer.opening.as_ref().unwrap().lock().unwrap().is_some(), "callback completed before poll");
            let mut render_copy = layer.clone();
            live.store(!revoke, SeqCst);
            assert!(!layer.poll_reload());
            if layer.data.is_some() == revoke {
                failures.push(format!("render application ignored revoke={revoke}"));
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let r = render_copy.wait_for_read(Instant::now() + web_time::Duration::from_secs(1));
                if r.is_ok() == revoke || render_copy.data.is_some() == revoke {
                    failures.push(format!("blocking application ignored revoke={revoke}"));
                }
            }
        }
        *in_place_reader().lock().unwrap() = previous;
        assert!(failures.is_empty(), "{failures:?}");
    }

    use rustgis_data::raster::{Crs, GeoTransform};

    use super::*;

    /// A Shaded Relief without a ramp or rock, lit from `azimuth` and `altitude`.
    fn grey_relief(azimuth: f64, altitude: f64, z_factor: f64, shadow: f64, sky: f64) -> RasterRenderer {
        RasterRenderer::ShadedRelief {
            band: 0,
            ramp: None,
            azimuth,
            altitude,
            z_factor,
            shadow,
            sky,
            fill: 0.0,
            rock: None,
            ambient: None,
            balanced: false,
        }
    }

    /// The legend names a class as the file does (#889), and gives the value of one it doesn't.
    #[test]
    fn legend_labels_classes_with_their_names() {
        let gt = GeoTransform { origin_x: 10.0, origin_y: 50.0, pixel_width: 0.5, pixel_height: -0.5 };
        let mut palette = vec![[0, 0, 0, 0]; 256];
        palette[1..4].copy_from_slice(&[[239, 235, 221, 255], [255, 231, 160, 255], [253, 190, 110, 255]]);
        let mut r =
            Raster::new("slope", 3, 1, vec![vec![1.0, 2.0, 3.0]], gt, Crs::LonLat, None, ColorHint::Palette(palette), "UInt8").unwrap();
        r.bands[0].classes = vec!["no data".into(), "0-5 %".into(), String::new(), "15-30 %".into()];
        let labels = |rl: &RasterLayer| -> Vec<String> {
            rl.legend()
                .into_iter()
                .map(|e| match e {
                    RasterLegend::Class { label, .. } => label,
                    other => panic!("{other:?}"),
                })
                .collect()
        };
        let mut rl = RasterLayer::from_path(r.clone(), "slope.tif");
        rl.style.renderer = RasterRenderer::Colormap;
        assert_eq!(labels(&rl), ["0-5 %", "2", "15-30 %"]);
        rl.style.renderer = unique_values(&r, 0, Ramp::Harbour);
        assert_eq!(labels(&rl), ["0-5 %", "2", "15-30 %"]);
        r.bands[0].classes.clear();
        rl.data = Some(Arc::new(r));
        assert_eq!(labels(&rl), ["1", "2", "3"], "no names: the values, as before");
    }

    /// A layout's legend lists a classified raster's classes as the Layers pane draws them (#1679):
    /// those the raster holds, or every class of a Unique Values or Classify renderer with
    /// `all_classes`; a colour table the values it holds either way; a ramp or bands none.
    #[test]
    fn legend_classes_are_the_panes_rows_the_raster_holds() {
        let gt = GeoTransform { origin_x: 10.0, origin_y: 50.0, pixel_width: 0.5, pixel_height: -0.5 };
        let mut palette = vec![[0, 0, 0, 0]; 256];
        for v in 1..=7u8 {
            palette[usize::from(v)] = [v * 30, 100, 200 - v * 20, 255];
        }
        // Five classes held (1 to 5), two named in the file.
        let values: Vec<f32> = (1..=5).map(|v| v as f32).collect();
        let mut r = Raster::new("lc", 5, 1, vec![values], gt, Crs::LonLat, None, ColorHint::Palette(palette), "UInt8").unwrap();
        r.bands[0].classes = vec![String::new(), "Forest".into(), String::new(), "Pasture".into()];
        let mut rl = RasterLayer::from_path(r, "lc.tif");
        let labels = |rl: &RasterLayer, all: bool| -> Vec<String> { rl.legend_classes(all).unwrap().into_iter().map(|(_, l)| l).collect() };
        let pane = |rl: &RasterLayer| -> Vec<(Rgba, String)> {
            rl.legend()
                .into_iter()
                .filter_map(|e| match e {
                    RasterLegend::Class { color, label } => Some((color, label)),
                    _ => None,
                })
                .collect()
        };
        rl.style.renderer = RasterRenderer::Colormap;
        assert_eq!(labels(&rl, false), ["Forest", "2", "Pasture", "4", "5"]);
        assert_eq!(rl.legend_classes(true), rl.legend_classes(false), "a colour table: the values held either way");
        assert_eq!(rl.legend_classes(false).unwrap(), pane(&rl), "the pane's colours and labels");
        // Unique values for 1 to 7: the raster holds five.
        let unique = (1..=7u8).map(|v| (f64::from(v), Rgba::rgb(v * 30, 0, 0))).collect();
        rl.style.renderer = RasterRenderer::UniqueValues { band: 0, ramp: Ramp::Harbour, values: unique };
        assert_eq!(labels(&rl, false), ["Forest", "2", "Pasture", "4", "5"]);
        assert_eq!(labels(&rl, true), ["Forest", "2", "Pasture", "4", "5", "6", "7"]);
        assert_eq!(rl.legend_classes(true).unwrap(), pane(&rl), "every class: the pane's rows");
        assert_eq!(rl.legend_classes(false).unwrap()[0].0, Rgba::rgb(30, 0, 0));
        // A value its mask hides is left out, held or not.
        rl.style.mask = RasterMask { values: vec![2.0, 6.0], ..RasterMask::default() };
        assert_eq!(labels(&rl, false), ["Forest", "Pasture", "4", "5"]);
        assert_eq!(labels(&rl, true), ["Forest", "Pasture", "4", "5", "7"]);
        rl.style.mask = RasterMask::default();
        // Class breaks: values 1 to 5 fall in the first three of four classes.
        let breaks = vec![2.0, 4.0, 10.0, 20.0];
        rl.style.renderer =
            RasterRenderer::Classify { band: 0, method: ClassMethod::EqualInterval, ramp: Ramp::Harbour, breaks, classes: 4 };
        assert_eq!(labels(&rl, false), ["1 – 2", "2 – 4", "4 – 10"]);
        assert_eq!(labels(&rl, true), ["1 – 2", "2 – 4", "4 – 10", "10 – 20"]);
        assert!(!rl.classes_unknown(), "breaks, not a colour table");
        // Read whole: its statistics are its own. Read in place: an overview's, said unless every
        // class of its breaks is listed.
        assert!(!rl.classes_from_overview(false));
        rl.in_place = true;
        assert!(rl.classes_from_overview(false) && !rl.classes_from_overview(true));
        rl.style.renderer = RasterRenderer::Colormap;
        assert!(rl.classes_from_overview(true), "a colour table lists what the overview holds either way");
        rl.in_place = false;
        assert!(!rl.classes_unknown(), "its values are known");
        // A ramp and bands have no classes: a colorbar keys them, or nothing does.
        rl.style.renderer = RasterRenderer::Stretch { band: 0, ramp: Ramp::Harbour, invert: false };
        assert_eq!(rl.legend_classes(false), None);
        rl.style.renderer = RasterRenderer::Rgb { red: 0, green: 0, blue: 0 };
        assert_eq!(rl.legend_classes(true), None);
    }

    /// Without an enumerated statistics list, Unique Values lists all configured classes; neither
    /// an overview nor masked sampling can then hide a class from the legend.
    #[test]
    fn unique_values_listing_every_class_has_no_missing_class_warning() {
        let side = 2049u32;
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.01, pixel_height: -0.01 };
        let cells = (0..side * side).map(|i| (i % 1000) as f32).collect();
        let raster = Raster::new("many", side, side, vec![cells], gt, Crs::LonLat, None, ColorHint::Gray, "UInt16").unwrap();
        assert!(raster.bands[0].stats.categories.is_none());
        let mut layer = RasterLayer::from_path(raster, "many.tif");
        layer.style.renderer = RasterRenderer::UniqueValues {
            band: 0,
            ramp: Ramp::Harbour,
            values: (0..1000).map(|v| (f64::from(v), Rgba::rgb(30, 100, 200))).collect(),
        };
        layer.in_place = true;
        assert_eq!(layer.legend_classes(false).unwrap().len(), 1000);
        assert!(!layer.classes_from_overview(false));
        layer.style.mask = RasterMask { values: vec![999.0], ..RasterMask::default() };
        assert_eq!(layer.legend_classes(false).unwrap().len(), 999);
        assert!(!layer.classes_from_sample(false) && !layer.classes_from_overview(false));
        // Once a sampled subset supplies the legend, the rare-class warning applies again.
        layer.style.mask = RasterMask { min: Some(0.0), max: Some(3.0), ..RasterMask::default() };
        // Sampling every second cell sees 0 and 2, missing the configured odd values.
        assert_eq!(layer.legend_classes(false).unwrap().len(), 2);
        assert!(layer.classes_from_sample(false) && layer.classes_from_overview(false));
        assert!(!layer.classes_from_sample(true) && !layer.classes_from_overview(true));
    }

    /// A colour table has no entry for a negative value, and the map draws none: neither the
    /// Layers pane nor a legend lists it (#1679), where `as usize` would have taken entry 0.
    #[test]
    fn a_colour_table_lists_no_negative_value() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.01, pixel_height: -0.01 };
        let mut palette = vec![[0, 0, 0, 0]; 256];
        palette[0] = [10, 20, 30, 255];
        palette[1] = [200, 0, 0, 255];
        palette[2] = [0, 200, 0, 255];
        let values = vec![-1.0, 1.0, 2.0];
        let r = Raster::new("change", 3, 1, vec![values], gt, Crs::LonLat, None, ColorHint::Palette(palette), "Int16").unwrap();
        assert_eq!(r.bands[0].stats.categories.as_deref(), Some(&[-1.0, 1.0, 2.0][..]));
        let mut rl = RasterLayer::from_path(r, "change.tif");
        rl.style.renderer = RasterRenderer::Colormap;
        let labels: Vec<String> = rl.legend_classes(false).unwrap().into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["1", "2"]);
        assert_eq!(rl.legend().len(), 2, "the Layers pane alike");
    }

    /// A fully transparent entry of a colour table (a background value) draws nothing on the map:
    /// neither the Layers pane nor a legend lists it, as a colorbar leaves it out (#1679).
    #[test]
    fn a_colour_table_lists_no_transparent_entry() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.01, pixel_height: -0.01 };
        let mut palette = vec![[0, 0, 0, 0]; 256];
        palette[1] = [200, 0, 0, 255];
        palette[2] = [0, 200, 0, 255];
        let values = vec![0.0, 1.0, 2.0];
        let r = Raster::new("clip", 3, 1, vec![values], gt, Crs::LonLat, None, ColorHint::Palette(palette), "UInt8").unwrap();
        let mut rl = RasterLayer::from_path(r, "clip.tif");
        rl.style.renderer = RasterRenderer::Colormap;
        let labels: Vec<String> = rl.legend_classes(false).unwrap().into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["1", "2"], "0 is transparent: not a class the map draws");
        assert_eq!(rl.legend().len(), 2, "the Layers pane alike");
    }

    /// A mask decides what a legend lists (#1679): one hiding every pixel leaves no Unique Values
    /// class, unless every class is asked for; a colour table over 300 values that a mask brings
    /// down to five lists those five; a mask over more cells than its statistics read is a sample.
    #[test]
    fn legend_classes_follow_the_mask() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.01, pixel_height: -0.01 };
        let r = Raster::new("lc", 3, 1, vec![vec![1.0, 2.0, 3.0]], gt, Crs::LonLat, None, ColorHint::Gray, "UInt8").unwrap();
        let mut rl = RasterLayer::from_path(r, "lc.tif");
        let unique = (1..=4u8).map(|v| (f64::from(v), Rgba::rgb(v * 30, 0, 0))).collect();
        rl.style.renderer = RasterRenderer::UniqueValues { band: 0, ramp: Ramp::Harbour, values: unique };
        // Every pixel hidden; the class for 4, which the raster doesn't hold, isn't.
        rl.style.mask = RasterMask { values: vec![1.0, 2.0, 3.0], ..RasterMask::default() };
        assert_eq!(rl.legend_classes(false), Some(Vec::new()), "nothing drawn, nothing listed");
        assert_eq!(rl.legend_classes(true).map(|c| c.len()), Some(1), "every class asked for: those the mask doesn't hide");
        assert!(!rl.classes_from_sample(false), "three cells, read whole");
        // A colour table over 300 values: none listed, unless a mask leaves few.
        let mut palette = vec![[0, 0, 0, 0]; 256];
        for (i, c) in palette.iter_mut().enumerate() {
            *c = [i as u8, 100, 255 - i as u8, 255];
        }
        let values: Vec<f32> = (0..300).map(|v| v as f32).collect();
        let mut r = Raster::new("many", 300, 1, vec![values], gt, Crs::LonLat, None, ColorHint::Palette(palette), "UInt16").unwrap();
        r.bands[0].classes = vec![String::new(), "Forest".into()];
        let mut rl = RasterLayer::from_path(r, "many.tif");
        rl.style.renderer = RasterRenderer::Colormap;
        assert!(rl.classes_unknown() && rl.legend_classes(false) == Some(Vec::new()));
        rl.style.mask = RasterMask { min: Some(1.0), max: Some(5.0), ..RasterMask::default() };
        let labels: Vec<String> = rl.legend_classes(false).unwrap().into_iter().map(|(_, l)| l).collect();
        assert_eq!(labels, ["Forest", "2", "3", "4", "5"]);
        assert!(!rl.classes_unknown());
        assert_eq!(rl.legend_classes(false).unwrap()[0].0, Rgba([1, 100, 254, 255]), "its colour in the table");
        // Past the cells a mask's statistics read: a sample, said unless every class is listed.
        let side = 2049u32;
        let cells: Vec<f32> = (0..side * side).map(|i| (i % 4 + 1) as f32).collect();
        let r = Raster::new("big", side, side, vec![cells], gt, Crs::LonLat, None, ColorHint::Gray, "UInt8").unwrap();
        let mut rl = RasterLayer::from_path(r, "big.tif");
        let unique = (1..=4u8).map(|v| (f64::from(v), Rgba::rgb(v * 30, 0, 0))).collect();
        rl.style.renderer = RasterRenderer::UniqueValues { band: 0, ramp: Ramp::Harbour, values: unique };
        assert!(!rl.classes_from_sample(false), "no mask: the band's own statistics");
        rl.style.mask = RasterMask { values: vec![4.0], ..RasterMask::default() };
        assert!(rl.classes_from_sample(false) && !rl.classes_from_sample(true));
        // A mask hiding every pixel: no class listed, so none missing from a sample or an overview.
        rl.in_place = true;
        assert!(rl.classes_from_overview(false), "read in place, a class can be missing");
        rl.style.mask = RasterMask { values: vec![1.0, 2.0, 3.0, 4.0], ..RasterMask::default() };
        assert_eq!(rl.legend_classes(false), Some(Vec::new()));
        assert!(!rl.classes_from_sample(false) && !rl.classes_from_overview(false));
    }

    /// Class breaks over a raster with too many values to list them: the classes from its lowest
    /// value's to its highest's (#1679).
    #[test]
    fn legend_classes_of_breaks_over_many_values_follow_the_range() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 0.01, pixel_height: -0.01 };
        // 1,000 distinct values from 100 to 199.9: no categories.
        let values: Vec<f32> = (0..1000).map(|i| 100.0 + i as f32 / 10.0).collect();
        let r = Raster::new("dem", 1000, 1, vec![values], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap();
        assert!(r.bands[0].stats.categories.is_none());
        let mut rl = RasterLayer::from_path(r, "dem.tif");
        let breaks = vec![50.0, 120.0, 150.0, 200.0, 300.0];
        rl.style.renderer =
            RasterRenderer::Classify { band: 0, method: ClassMethod::EqualInterval, ramp: Ramp::Harbour, breaks, classes: 5 };
        let n = |all| rl.legend_classes(all).unwrap().len();
        assert_eq!((n(false), n(true)), (3, 5), "120, 150 and 200: the classes 100 to 199.9 fall in");
        // By a colour table, over values it can't list: no class, said by `classes_unknown`.
        rl.style.renderer = RasterRenderer::Colormap;
        assert_eq!(rl.legend_classes(false), Some(Vec::new()));
        assert!(rl.classes_unknown());
    }

    /// A following layer's dates are its steps from `offset` on, one each: repair keeps them as
    /// they are when they are in order, and follows none rather than the wrong step otherwise.
    #[test]
    fn zarr_dates_keep_their_steps() {
        let zt = |dates: Vec<f64>| {
            let mut z = ZarrTime {
                store: "s".into(),
                variable: "t".into(),
                index: Vec::new(),
                dim: "time".into(),
                offset: 3,
                dates,
                crs: None,
                window: None,
                shown: Some(1),
                read_generation: 0,
            };
            z.repair();
            z
        };
        let ok = zt(vec![1.0, 2.0, 5.0]);
        assert_eq!((ok.dates, ok.shown), (vec![1.0, 2.0, 5.0], None));
        for bad in [vec![2.0, 1.0], vec![1.0, 1.0, 2.0], vec![1.0, f64::NAN]] {
            assert!(zt(bad.clone()).dates.is_empty(), "{bad:?}");
        }
    }

    /// A following layer's window is part of each step's selection, as `zarr::Selection::parse`
    /// reads it; repair keeps one a read refuses (a project edited by hand), never dropping it to
    /// the whole map.
    #[test]
    fn a_zarr_window_is_in_each_steps_selection() {
        let mut z = ZarrTime {
            variable: "t".into(),
            index: vec![("level".into(), 2)],
            dim: "time".into(),
            offset: 3,
            dates: vec![1.0, 2.0],
            window: Some([4, 1, 9, 3]),
            ..Default::default()
        };
        z.repair();
        assert_eq!(z.selection(1), "t[time=4,level=2,cells=4:1:9:3]");
        z.window = Some([4, 1, 5, 3]);
        z.repair();
        assert_eq!((z.window, z.selection(0)), (Some([4, 1, 5, 3]), "t[time=3,level=2,cells=4:1:5:3]".to_string()));
    }

    fn dem() -> Raster {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 1.0, pixel_height: -1.0 };
        let data: Vec<f32> = (0..16).map(|i| (i % 4 + i / 4) as f32).collect();
        Raster::new("dem", 4, 4, vec![data], gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap()
    }

    /// What reading the pixels lost is said once; a look applied after that (bands the file
    /// hasn't) says what it lost on its own, not the pixels' losses again.
    #[test]
    fn a_look_applied_after_the_losses_were_said_says_its_own_alone() {
        let mut r = dem();
        r.lost.push("its colour table is left out".into());
        let mut rl = RasterLayer::in_place(r, "test://look-later.tif");
        assert_eq!(rl.lost_unsaid(), vec!["its colour table is left out".to_string()]);
        assert!(rl.lost_unsaid().is_empty());
        let mut rl = rl.with_look(AddressLook { bands: vec![9], range: None });
        let said = rl.lost_unsaid();
        assert_eq!(said.len(), 1, "{said:?}");
        assert!(!said[0].contains("colour table"), "{said:?}");
        assert!(rl.lost_unsaid().is_empty());
        // A look applied before they are said is said with them, once.
        let mut r = dem();
        r.lost.push("its colour table is left out".into());
        let mut rl = RasterLayer::in_place(r, "test://look-first.tif").with_look(AddressLook { bands: vec![9], range: None });
        assert_eq!(rl.lost_unsaid().len(), 2);
        assert!(rl.lost_unsaid().is_empty());
    }

    /// A project file may ask for any band number: the one past the last `usize` is said by
    /// number, not overflowed.
    #[test]
    fn a_look_asking_for_the_band_past_the_last_usize_is_said() {
        let lost = RasterStyle::look_lost(&dem(), &AddressLook { bands: vec![usize::MAX], range: None }).unwrap();
        assert!(lost.contains(&format!("band(s) {} (from 1)", usize::MAX)), "{lost}");
    }

    #[test]
    fn defaults_and_colours() {
        let r = dem();
        let s = RasterStyle::default_for(&r);
        assert_eq!(s.renderer.code(), "stretch");
        let c = Colorizer::new(&s, &r, None);
        let lo = c.color(&[0.0], None).unwrap();
        let hi = c.color(&[6.0], None).unwrap();
        assert!(lo[0] < hi[0], "grey: low is dark ({lo:?} vs {hi:?})");
        assert_eq!(c.color(&[f32::NAN], None), None);
        assert_eq!(c.color(&[], None), None);

        let u = RasterStyle { renderer: unique_values(&r, 0, Ramp::Harbour), ..s.clone() };
        let c = Colorizer::new(&u, &r, None);
        assert!(c.color(&[3.0], None).is_some());
        assert_eq!(c.color(&[3.5], None), None);
        let k = RasterStyle { renderer: classify(&r, 0, 3, ClassMethod::EqualInterval, Ramp::Ember), ..s };
        assert!(Colorizer::new(&k, &r, None).color(&[5.0], None).is_some());
    }

    /// A style mask's statistics leave out the cells a colour band's alpha draws see-through, as
    /// the band's own leave them out (#1810).
    #[test]
    fn a_masks_statistics_leave_out_see_through_cells() {
        use rustgis_data::raster::{ColorHint, Crs, GeoTransform};
        let gt = GeoTransform { origin_x: 0.0, origin_y: 4.0, pixel_width: 1.0, pixel_height: -1.0 };
        // The bottom row see-through, holding −3.4e38; 200 elsewhere, and a black first cell.
        let colour: Vec<f32> = (0..16)
            .map(|i| {
                if i >= 12 {
                    -3.4e38
                } else if i == 0 {
                    0.0
                } else {
                    200.0
                }
            })
            .collect();
        let alpha: Vec<f32> = (0..16).map(|i| if i >= 12 { 0.0 } else { 1.0 }).collect();
        let bands = vec![colour.clone(), colour.clone(), colour, alpha];
        let r = Raster::new("rgba", 4, 4, bands, gt, Crs::LonLat, None, ColorHint::Rgb { alpha: true }, "Float32").unwrap();
        let mask = RasterMask { values: vec![200.0], ..RasterMask::default() };
        // 200 hidden by the mask, the bottom row by the alpha: the black cell alone.
        let st = mask.stats(&r, 0);
        assert_eq!((st.count, st.min, st.max), (1, 0.0, 0.0));
        // The alpha band itself: every cell the mask leaves.
        assert_eq!(mask.stats(&r, 3).count, 16);
    }

    /// #348: values a mask lists, or outside (or inside) its range, are no data.
    #[test]
    fn masks_hide_values_and_their_statistics() {
        let r = dem();
        let m = RasterMask { values: vec![0.0, 6.0], ..RasterMask::default() };
        assert!(m.hides(0.0) && m.hides(6.0) && !m.hides(3.0));
        // Compared in f32, as the pixels are.
        assert!(RasterMask { values: vec![0.1], ..RasterMask::default() }.hides(0.1f32));
        let range = RasterMask { min: Some(2.0), max: Some(4.0), ..RasterMask::default() };
        assert!(range.hides(1.0) && !range.hides(2.0) && !range.hides(4.0) && range.hides(5.0));
        let inside = RasterMask { inside: true, ..range.clone() };
        assert!(!inside.hides(1.0) && inside.hides(3.0) && !inside.hides(5.0));
        assert!(RasterMask { max: Some(2.0), ..RasterMask::default() }.hides(3.0), "one bound");
        assert!(!RasterMask::default().hides(f32::NAN) && RasterMask::default().is_empty());
        assert!(!range.hides(f32::NAN), "no data isn't masked again");
        assert!(RasterMask { values: vec![0.0], ..RasterMask::default() }.hides(-0.0));
        // A hand-edited project's values are put in order as they are read.
        let read: RasterMask = serde_json::from_str(r#"{"values": [255, 0, 254, 0]}"#).unwrap();
        assert_eq!(read.values, vec![0.0, 254.0, 255.0]);
        assert!(read.hides(255.0) && read.hides(0.0) && !read.hides(1.0));
        let st = m.stats(&r, 0);
        assert_eq!((st.min, st.max, st.count), (1.0, 5.0, 14));
        assert_eq!(m.stats(&r, 9), BandStats::default(), "no such band");
        assert_eq!(range.label(), "outside 2 to 4");

        let mut rl = RasterLayer::from_path(r.clone(), "dem.tif");
        assert!(rl.masked_stats().is_none(), "nothing masked");
        rl.style.mask = m.clone();
        let c = Colorizer::new(&rl.style, &r, None);
        assert_eq!(c.color(&[0.0], None), None);
        assert!(c.color(&[3.0], None).is_some());
        let first = rl.masked_stats().unwrap();
        assert_eq!(first[0].min, 1.0);
        assert!(Arc::ptr_eq(&first, &rl.masked_stats().unwrap()), "kept between draws");
        rl.style.mask.values = vec![1.0];
        assert_eq!(rl.masked_stats().unwrap()[0].min, 0.0, "worked out again for a new mask");
        let again = rl.masked_stats().unwrap();
        rl.data = Some(Arc::new(r.clone()));
        assert!(!Arc::ptr_eq(&again, &rl.masked_stats().unwrap()), "and for new pixels");
        // The legend leaves out masked classes and stretches over the values shown.
        rl.style.renderer = unique_values(&r, 0, Ramp::Harbour);
        let n = rl.legend().len();
        rl.style.mask.values = vec![1.0, 2.0];
        assert_eq!(rl.legend().len(), n - 1);
        rl.style.renderer = classify_masked(&r, 0, 3, ClassMethod::EqualInterval, Ramp::Ember, &rl.style.mask);
        let RasterRenderer::Classify { breaks, .. } = &rl.style.renderer else { panic!() };
        assert_eq!(breaks.last(), Some(&6.0));
        let json = serde_json::to_value(&rl.style).unwrap();
        assert_eq!(json["mask"]["values"], serde_json::json!([1.0, 2.0]));
        assert_eq!(serde_json::from_value::<RasterStyle>(json).unwrap(), rl.style);
        let plain = serde_json::to_value(RasterStyle::default_for(&r)).unwrap();
        assert!(plain.get("mask").is_none(), "no mask, nothing saved");
    }

    #[test]
    fn stretches_and_enhancement() {
        let r = dem();
        let st = &r.bands[0].stats;
        for code in ["none", "minMax", "percentClip", "standardDeviation", "histogramEqualize"] {
            let n = Normalizer::new(Stretch::from_code(code, None, None, None, None, None, Some(st)).unwrap(), st, "Float32");
            assert!(n.t(-1e9) <= n.t(3.0) && n.t(3.0) <= n.t(1e9), "{code}");
            assert!((0.0..=1.0).contains(&n.t(f64::NAN).max(0.0)));
        }
        // Bounds read from a hand-edited project can be reversed, equal or not numbers.
        for (min, max) in [(5.0, 1.0), (2.0, 2.0), (f64::NAN, 1.0), (0.0, f64::INFINITY)] {
            let n = Normalizer::new(Stretch::custom(min, max), st, "Float32");
            assert!((0.0..=1.0).contains(&n.t(3.0)) && (0.0..=1.0).contains(&n.t(f64::NAN).max(0.0)), "{min} {max}");
        }
        assert_eq!(Stretch::from_code("custom", None, None, None, None, None, None), None);
        assert_eq!(Stretch::from_code("bogus", None, None, None, None, None, Some(st)), None);
        let flat = enhancement_lut(0.0, 0.0, 1.0);
        assert!(flat.iter().enumerate().all(|(i, v)| (*v as i32 - i as i32).abs() <= 1));
        let bright = enhancement_lut(50.0, 0.0, 1.0);
        assert!(bright[100] > flat[100]);
        let _ = enhancement_lut(f64::NAN, f64::INFINITY, f64::NAN);
    }

    fn rgb() -> Raster {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
        let bands = vec![vec![0.0, 100.0], vec![0.0, 200.0], vec![0.0, 50.0]];
        Raster::new("rgb", 2, 1, bands, gt, Crs::LonLat, None, ColorHint::Gray, "Float32").unwrap()
    }

    #[test]
    fn custom_stretch_bounds_per_channel() {
        let r = rgb();
        let mut style = RasterStyle::default_for(&r);
        style.renderer = RasterRenderer::Rgb { red: 0, green: 1, blue: 2 };
        // One pair for every channel: the same value draws differently only by band, never by bounds.
        style.stretch = Stretch::custom(0.0, 100.0);
        let shared = Colorizer::new(&style, &r, None).color(&[50.0, 50.0, 50.0], None).unwrap();
        assert_eq!(shared[..3], [128, 128, 128]);
        // A pair per channel: each channel scales by its own bounds.
        style.stretch = Stretch::custom_bands([[0.0, 100.0], [0.0, 200.0], [0.0, 50.0]]);
        let c = Colorizer::new(&style, &r, None).color(&[50.0, 50.0, 50.0], None).unwrap();
        assert_eq!(c[..3], [128, 64, 255]);
        // The red channel's bounds are the plain pair, so a single band reads them.
        assert_eq!(style.stretch, Stretch::Custom { min: 0.0, max: 100.0, bands: style_bands(&style) });
        let n = Normalizer::for_channel(style.stretch, 1, &r.bands[1].stats, "Float32");
        assert_eq!((n.lo, n.hi), (0.0, 200.0));
        let n = Normalizer::new(style.stretch, &r.bands[1].stats, "Float32");
        assert_eq!((n.lo, n.hi), (0.0, 100.0));
    }

    fn style_bands(style: &RasterStyle) -> Option<[[f64; 2]; 3]> {
        match style.stretch {
            Stretch::Custom { bands, .. } => bands,
            _ => None,
        }
    }

    #[test]
    fn custom_stretch_files_stay_compatible() {
        // Saved by a version that had one pair of bounds.
        let old: Stretch = serde_json::from_str(r#"{"Custom":{"min":1.0,"max":2.0}}"#).unwrap();
        assert_eq!(old, Stretch::custom(1.0, 2.0));
        assert_eq!(serde_json::to_string(&old).unwrap(), r#"{"Custom":{"min":1.0,"max":2.0}}"#);
        let per = Stretch::custom_bands([[0.0, 1.0], [2.0, 3.0], [4.0, 5.0]]);
        let back: Stretch = serde_json::from_str(&serde_json::to_string(&per).unwrap()).unwrap();
        assert_eq!(back, per);
        // Read as one pair (an older version ignoring `bands`) it is the red channel's.
        let json: serde_json::Value = serde_json::to_value(per).unwrap();
        assert_eq!(json["Custom"]["min"], 0.0);
        assert_eq!(json["Custom"]["max"], 1.0);
    }

    #[test]
    fn hostile_channel_bounds_never_panic() {
        let r = rgb();
        let st = &r.bands[0].stats;
        let bad = [5.0, 1.0];
        for pair in [bad, [2.0, 2.0], [f64::NAN, 1.0], [0.0, f64::INFINITY]] {
            for ch in 0..5 {
                let n = Normalizer::for_channel(Stretch::custom_bands([pair, pair, pair]), ch, st, "Float32");
                assert!((0.0..=1.0).contains(&n.t(3.0)) && (0.0..=1.0).contains(&n.t(f64::NAN).max(0.0)), "{pair:?} {ch}");
            }
        }
    }

    #[test]
    fn hillshade_faces_the_light() {
        // Ground rising to the east faces west: light from the west brightens it. Flat ground is
        // lit evenly at cos(45°).
        let east = [0.0, 1.0, 2.0, 0.0, 1.0, 2.0, 0.0, 1.0, 2.0];
        let lit_w = hillshade(&east, 1.0, 1.0, 270.0, 45.0, 1.0);
        let lit_e = hillshade(&east, 1.0, 1.0, 90.0, 45.0, 1.0);
        assert!(lit_w > lit_e, "{lit_w} vs {lit_e}");
        let flat = hillshade(&[5.0; 9], 1.0, 1.0, 315.0, 45.0, 1.0);
        assert!((175..=185).contains(&flat), "{flat}");
    }

    #[test]
    fn project_json_round_trip_embeds_uploads() {
        let r = dem();
        let bytes = rustgis_data::raster::geotiff::write(&r, 0).unwrap();
        let rl = RasterLayer::from_bytes(r, "dem.tif", bytes);
        let json = serde_json::to_string(&rl).unwrap();
        let mut back: RasterLayer = serde_json::from_str(&json).unwrap();
        assert!(back.data.is_none());
        back.reload().unwrap();
        assert_eq!(back.data.as_ref().map(|d| d.width), Some(4));
        let mut missing = RasterLayer { path: Some("/nonexistent/x.tif".into()), embedded: None, ..back };
        assert!(missing.reload().is_err());
        assert!(missing.data.is_none());
    }

    /// A plain web address is read as it was before logins: never downloaded whole on reload.
    #[test]
    fn reload_downloads_only_s3_and_login_wrapped_addresses() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 1.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r = Raster::new("web", 1, 1, vec![vec![1.0]], gt, Crs::LonLat, None, ColorHint::Gray, "UInt8").unwrap();
        let mut plain = RasterLayer::from_path(r, "https://anon757.invalid/x.tif");
        plain.data = None;
        assert!(plain.reload().is_err());
        assert!(plain.opening.is_none(), "an anonymous web raster is not fetched whole");
    }

    /// Rock on steep slopes (#1913): the ramp's colour up to `from`, the rock's from `to` on, and a
    /// smooth step between them; the slope is the ground's own, which no Z factor changes; and the
    /// rock is saved only when set.
    #[test]
    fn rock_tints_steep_slopes_by_the_grounds_own_slope() {
        let rock = Rock { from: 20.0, to: 40.0, color: Rgba::rgb(200, 100, 50) };
        assert_eq!((rock.share(0.0), rock.share(20.0), rock.share(40.0), rock.share(70.0)), (0.0, 0.0, 1.0, 1.0));
        assert!((rock.share(30.0) - 0.5).abs() < 1e-12);
        let (a, b) = (rock.share(22.0), rock.share(25.0));
        assert!(0.0 < a && a < b && b < 0.5, "a smooth step, slow at its ends: {a}, {b}");
        // A plane rising one metre a metre eastward is 45 degrees, whatever the light's Z factor.
        let z = [0.0, 10.0, 20.0, 0.0, 10.0, 20.0, 0.0, 10.0, 20.0];
        assert!((slope(&z, 10.0, 10.0) - 45.0).abs() < 1e-9);
        assert_ne!(hillshade(&z, 10.0, 10.0, 270.0, 45.0, 1.0), hillshade(&z, 10.0, 10.0, 270.0, 45.0, 3.0));

        let gt = GeoTransform { origin_x: 0.0, origin_y: 10.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r =
            Raster::new("dem", 10, 10, vec![(0..100).map(|v| v as f32).collect()], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32")
                .unwrap();
        let relief = |rock| RasterRenderer::ShadedRelief {
            band: 0,
            ramp: Some(Ramp::Terrain),
            azimuth: 315.0,
            altitude: 45.0,
            z_factor: 1.0,
            shadow: 0.0,
            sky: 0.0,
            fill: 0.0,
            rock,
            ambient: None,
            balanced: false,
        };
        let (mut rocky, mut plain) = (RasterStyle::default_for(&r), RasterStyle::default_for(&r));
        (rocky.renderer, plain.renderer) = (relief(Some(rock)), relief(None));
        let (with, without) = (Colorizer::new(&rocky, &r, None), Colorizer::new(&plain, &r, None));
        for (v, h) in [(10.0, 0), (50.0, 128), (90.0, 255)] {
            let plain = without.color_lit(&[v], Some(h), None, None, None);
            assert_eq!(with.color_lit(&[v], Some(h), None, None, None), plain, "no share of rock: the ramp");
            assert_eq!(with.color_lit(&[v], Some(h), None, None, Some(0)), plain, "flat ground: the ramp");
            assert_eq!(without.color_lit(&[v], Some(h), None, None, Some(255)), plain, "no rock set: the ramp");
            // All rock: the rock colour, lit as the ramp is (a quarter ambient, the rest the sun).
            let lit = |c: u8| (f64::from(c) * (0.25 + 0.75 * f64::from(h) / 255.0)).round() as u8;
            assert_eq!(with.color_lit(&[v], Some(h), None, None, Some(255)), Some([lit(200), lit(100), lit(50), 255]));
            // Half way: half way between the two colours.
            let half = with.color_lit(&[v], Some(255), None, None, Some(128)).unwrap();
            let ramp = without.color_lit(&[v], Some(255), None, None, None).unwrap();
            for (k, end) in [200u8, 100, 50].into_iter().enumerate() {
                let mid = (f64::from(ramp[k]) + f64::from(end)) / 2.0;
                assert!((f64::from(half[k]) - mid).abs() <= 1.5, "{v}: channel {k} {} not near {mid}", half[k]);
            }
            // With the sky and the fill light (#1912) too: the rock colour first, then every light
            // shades it, as they shade the ramp.
            let all = |c: u8| (f64::from(c) * (0.25 * 128.0 / 255.0 + 0.75 * f64::from(h) / 255.0 + 40.0 / 255.0)).round() as u8;
            assert_eq!(with.color_lit(&[v], Some(h), Some(128), Some(40), Some(255)), Some([all(200), all(100), all(50), 255]));
            let filled = without.color_lit(&[v], Some(h), Some(128), Some(40), None);
            assert_eq!(with.color_lit(&[v], Some(h), Some(128), Some(40), Some(0)), filled, "flat ground: the filled ramp");
        }

        // Saved only when set, and read back as it was.
        let saved = serde_json::to_string(&relief(None)).unwrap();
        assert!(!saved.contains("rock"), "{saved}");
        let saved = serde_json::to_string(&relief(Some(rock))).unwrap();
        assert_eq!(serde_json::from_str::<RasterRenderer>(&saved).unwrap(), relief(Some(rock)));
    }

    /// Ambient and the balanced light (#1977): unset, the relief is lit as before; `ambient` is the
    /// share a slope turned from the sun keeps; balanced, flat ground keeps the ramp's colour,
    /// slopes facing the sun brighten up to 1.3, and every other light still shades it. Each is
    /// saved only when set.
    #[test]
    fn balanced_light_keeps_flat_ground_at_the_ramps_colour() {
        assert_eq!((relief_ambient(None, false), relief_ambient(None, true), relief_ambient(Some(2.0), true)), (0.25, 0.5, 1.0));
        let gt = GeoTransform { origin_x: 0.0, origin_y: 10.0, pixel_width: 1.0, pixel_height: -1.0 };
        let r = Raster::new("dem", 10, 10, vec![vec![5.0; 100]], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let tinted_relief = |ramp, altitude, ambient, balanced| RasterRenderer::ShadedRelief {
            band: 0,
            ramp,
            azimuth: 315.0,
            altitude,
            z_factor: 1.0,
            shadow: 0.0,
            sky: 0.0,
            fill: 0.0,
            rock: None,
            ambient,
            balanced,
        };
        let relief = |altitude, ambient, balanced| tinted_relief(None, altitude, ambient, balanced);
        // Grey relief: white shaded, so a channel is 255 times the light.
        let paint = |renderer: RasterRenderer, h: u8, sky: Option<u8>, fill: Option<u8>| {
            let mut style = RasterStyle::default_for(&r);
            style.renderer = renderer;
            Colorizer::new(&style, &r, None).color_lit(&[5.0], Some(h), sky, fill, None).map(|c| c[0])
        };
        let grey = |f: f64| Some((255.0 * f).round().clamp(0.0, 255.0) as u8);
        for h in [0u8, 90, 180, 255] {
            let hf = f64::from(h) / 255.0;
            assert_eq!(paint(relief(45.0, None, false), h, None, None), grey(0.25 + 0.75 * hf), "unset: as before");
            assert_eq!(paint(relief(45.0, Some(0.4), false), h, None, None), grey(0.4 + 0.6 * hf));
        }
        for altitude in [20.0, 45.0, 70.0] {
            // Flat ground's hillshade, as `hillshade` rounds it: the ramp's own colour.
            let flat = hillshade(&[0.0; 9], 1.0, 1.0, 315.0, altitude, 1.0);
            for ambient in [None, Some(0.2), Some(0.8)] {
                assert_eq!(paint(relief(altitude, ambient, true), flat, None, None), Some(255), "{altitude}° {ambient:?}");
            }
            // Turned away, the ambient share; facing the sun, brighter, up to 1.3 times.
            assert_eq!(paint(relief(altitude, None, true), 0, None, None), grey(0.5));
            let lit = (0.5 + 0.5 * 255.0 / f64::from(flat)).min(1.3);
            assert!(lit > 1.0);
            assert_eq!(paint(relief(altitude, None, true), 255, None, None), Some(255), "white: clamped");
            // On a ramp's colour, flat ground is that colour, and a slope facing the sun it times 1.3 at most.
            let mut style = RasterStyle::default_for(&r);
            style.renderer = tinted_relief(Some(Ramp::Terrain), altitude, None, true);
            let tinted = Colorizer::new(&style, &r, None);
            let ramp = tinted.color_lit(&[5.0], Some(flat), None, None, None).unwrap();
            let bright = |c: u8| (f64::from(c) * lit).round().min(255.0) as u8;
            let lit_ramp = tinted.color_lit(&[5.0], Some(255), None, None, None).unwrap();
            assert_eq!(lit_ramp, [bright(ramp[0]), bright(ramp[1]), bright(ramp[2]), 255]);
            assert!(lit_ramp.iter().zip(ramp).take(3).any(|(l, c)| *l > c), "brighter than the ramp: {lit_ramp:?} {ramp:?}");
            // The sky dims the ambient share, the fill light adds to it, as before.
            let sky_fill = 0.5 * 128.0 / 255.0 + 0.5 * f64::from(flat / 2) / f64::from(flat) + 30.0 / 255.0;
            assert_eq!(paint(relief(altitude, None, true), flat / 2, Some(128), Some(30)), grey(sky_fill));
        }

        // A sun on the horizon lights no flat ground (its hillshade rounds to 0): balanced is
        // lit as unbalanced, with its own ambient.
        for h in [0u8, 40, 120] {
            assert_eq!(paint(relief(0.0, None, true), h, None, None), paint(relief(0.0, Some(0.5), false), h, None, None));
        }

        // Saved only when set, and read back as it was.
        let saved = serde_json::to_string(&relief(45.0, None, false)).unwrap();
        assert!(!saved.contains("ambient") && !saved.contains("balanced"), "{saved}");
        let set = relief(45.0, Some(0.35), true);
        let saved = serde_json::to_string(&set).unwrap();
        assert_eq!(serde_json::from_str::<RasterRenderer>(&saved).unwrap(), set);
    }

    /// A tower lit from the east, drawn as Shaded Relief with `shadow`.
    fn tower(shadow: f64) -> RasterLayer {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 30.0, pixel_height: -30.0 };
        let data: Vec<f32> = (0..400).map(|i| if i == 10 * 20 + 10 { 900.0 } else { 0.0 }).collect();
        let r = Raster::new("tower", 20, 20, vec![data], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let mut rl = RasterLayer::from_path(r, "tower.tif");
        rl.style.renderer = grey_relief(90.0, 30.0, 1.0, shadow, 0.0);
        rl
    }

    /// A shadow of 0 casts none and starts no work; one above it is worked out once, kept, waited
    /// for by an export, and worked out again for another sun.
    #[test]
    fn cast_shadows_are_worked_out_once_per_sun() {
        assert_eq!(tower(0.0).cast_shadows(true, (0, 1), || {}), CastShadows::None);
        let mut rl = tower(0.8);
        // Each landing is sent: `landed` is called on the worker just after the grid is kept, so
        // it is waited for, not read at once.
        let (sent, landings) = std::sync::mpsc::channel::<()>();
        let landed = |tx: &std::sync::mpsc::Sender<()>| {
            let tx = tx.clone();
            move || {
                let _ = tx.send(());
            }
        };
        let CastShadows::Ready(grid) = rl.cast_shadows(true, (0, 1), landed(&sent)) else { panic!("waited for") };
        assert!(landings.recv_timeout(std::time::Duration::from_secs(60)).is_ok(), "landed once");
        // West of the tower, in its shadow; east of it, lit.
        assert_eq!(grid[10 * 20 + 9], 0);
        assert_eq!(grid[10 * 20 + 11], 255);
        // Kept: the same grid, nothing worked out again, and nothing more lands.
        assert!(matches!(rl.cast_shadows(false, (0, 1), landed(&sent)), CastShadows::Ready(g) if Arc::ptr_eq(&g, &grid)));
        drop(sent);
        assert!(landings.recv().is_err(), "landed again");
        // The shadow's strength is drawn, not worked out: the grid is kept for another.
        if let RasterRenderer::ShadedRelief { shadow, .. } = &mut rl.style.renderer {
            *shadow = 0.3;
        }
        assert!(matches!(rl.cast_shadows(false, (0, 1), || {}), CastShadows::Ready(g) if Arc::ptr_eq(&g, &grid)));
        // Another sun: worked out again, lit from the west now.
        if let RasterRenderer::ShadedRelief { azimuth, .. } = &mut rl.style.renderer {
            *azimuth = 270.0;
        }
        let CastShadows::Ready(west) = rl.cast_shadows(true, (0, 1), || {}) else { panic!("waited for") };
        assert_eq!((west[10 * 20 + 9], west[10 * 20 + 11]), (255, 0));
        // A copy lit from elsewhere (another map, whose layer has the same id) keeps its own grid
        // beside it: neither stops the other's.
        let mut copy = rl.clone();
        if let RasterRenderer::ShadedRelief { azimuth, .. } = &mut copy.style.renderer {
            *azimuth = 0.0;
        }
        assert!(matches!(copy.cast_shadows(true, (9, 1), || {}), CastShadows::Ready(_)));
        assert!(matches!(rl.cast_shadows(false, (0, 1), || {}), CastShadows::Ready(g) if Arc::ptr_eq(&g, &west)));
    }

    /// A new sun from the same owner stops the work for its last one, unless it is done; another
    /// owner's (a copy on another map) goes on.
    #[test]
    fn a_new_sun_supersedes_the_owners_last_work() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 30.0, pixel_height: -30.0 };
        let data: Vec<f32> = (0..1200 * 1200).map(|i| (((i % 1200) as f32 / 7.0).sin() * 300.0) + (i / 1200) as f32).collect();
        let r = Raster::new("hills", 1200, 1200, vec![data], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let mut rl = RasterLayer::from_path(r, "hills.tif");
        let lit = |rl: &mut RasterLayer, az: f64| {
            rl.style.renderer = grey_relief(az, 10.0, 1.0, 1.0, 0.0);
        };
        let other = |az: f64, rl: &RasterLayer| {
            let mut c = rl.clone();
            lit(&mut c, az);
            c
        };
        let work_of = |rl: &RasterLayer, az: f64| {
            rl.cast
                .0
                .lock()
                .unwrap()
                .iter()
                .find(|j| matches!(j.key.grid, Grid::Shadows { azimuth, .. } if azimuth == az))
                .map(|j| j.work.clone())
        };
        lit(&mut rl, 10.0);
        rl.cast_shadows(false, (0, 1), || {});
        let copy = other(20.0, &rl);
        copy.cast_shadows(false, (9, 1), || {});
        let first = work_of(&rl, 10.0).unwrap();
        lit(&mut rl, 30.0);
        rl.cast_shadows(false, (0, 1), || {});
        // Owner 1's last sun is stopped and let go, unless it was done; owner 2's goes on.
        let stopped = first.stop.load(std::sync::atomic::Ordering::SeqCst);
        assert!(first.is_done() || (stopped && work_of(&rl, 10.0).is_none()));
        let second = work_of(&rl, 20.0).unwrap();
        assert!(!second.stop.load(std::sync::atomic::Ordering::SeqCst));
        assert!(work_of(&rl, 30.0).is_some());
        // Going back to a sun kept stops the one just left, as a new sun would.
        lit(&mut rl, 40.0);
        rl.cast_shadows(false, (0, 1), || {});
        let left = work_of(&rl, 40.0).unwrap();
        lit(&mut rl, 30.0);
        rl.cast_shadows(false, (0, 1), || {});
        assert!(left.is_done() || (left.stop.load(std::sync::atomic::Ordering::SeqCst) && work_of(&rl, 40.0).is_none()));
        assert!(matches!(copy.cast_shadows(true, (9, 1), || {}), CastShadows::Ready(_)));
        // Waited for, the latest is drawn.
        assert!(matches!(rl.cast_shadows(true, (0, 1), || {}), CastShadows::Ready(_)));
    }

    /// An export cancelled while it waits for a grid it alone waits for stops the work (the
    /// worker has no probe of its own), and the grid isn't kept: the next draw starts it over.
    #[test]
    fn a_cancelled_export_stops_the_work_it_waited_for() {
        struct Cancelled;
        impl rustgis_data::progress::Probe for Cancelled {
            fn cancelled(&self) -> bool {
                true
            }
            fn begin(&self, _: u64) {}
            fn advance(&self, _: u64) {}
        }
        // Gentle ground under a sun at 2°: every cell traces to the raster's edge, far longer
        // than the stop takes.
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 30.0, pixel_height: -30.0 };
        let data: Vec<f32> = (0..2000 * 2000).map(|i| ((i % 2000) as f32 * 0.01) + ((i / 2000) as f32 * 0.01)).collect();
        let r = Raster::new("plain", 2000, 2000, vec![data], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let mut rl = RasterLayer::from_path(r, "plain.tif");
        rl.style.renderer = grey_relief(135.0, 2.0, 1.0, 1.0, 0.0);
        let (sent, landings) = std::sync::mpsc::channel::<()>();
        {
            let _cancelled = rustgis_data::progress::install(Some(std::sync::Arc::new(Cancelled)));
            let tx = sent.clone();
            let out = rl.cast_shadows(true, (0, 1), move || {
                let _ = tx.send(());
            });
            assert_eq!(out, CastShadows::Pending, "the cancelled export gives up");
        }
        // The worker stops and says so (the map draws again), with no grid.
        assert!(landings.recv_timeout(std::time::Duration::from_secs(60)).is_ok(), "landed");
        let work = rl.cast.0.lock().unwrap().first().map(|j| j.work.clone()).unwrap();
        assert!(work.was_stopped(Grid::Shadows { azimuth: 0.0, altitude: 0.0 }.stopped()));
        // Asked again, it starts over.
        rl.cast_shadows(false, (0, 1), || {});
        let again = rl.cast.0.lock().unwrap().first().map(|j| j.work.clone()).unwrap();
        assert!(!Arc::ptr_eq(&work, &again));
        again.stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// A frame joining work under way (an export started it) is drawn again when it lands too.
    #[test]
    fn who_joins_work_under_way_is_told_when_it_lands() {
        let gt = GeoTransform { origin_x: 0.0, origin_y: 0.0, pixel_width: 30.0, pixel_height: -30.0 };
        let data: Vec<f32> = (0..800 * 800).map(|i| ((i % 800) as f32 * 0.01) + ((i / 800) as f32 * 0.01)).collect();
        let r = Raster::new("plain", 800, 800, vec![data], gt, Crs::WebMercator, None, ColorHint::Gray, "Float32").unwrap();
        let mut rl = RasterLayer::from_path(r, "plain.tif");
        rl.style.renderer = grey_relief(135.0, 3.0, 1.0, 1.0, 0.0);
        let (sent, landings) = std::sync::mpsc::channel::<&'static str>();
        let told = |who: &'static str| {
            let tx = sent.clone();
            move || {
                let _ = tx.send(who);
            }
        };
        // Joined while under way (held before it traces), both are told.
        let hold = hold_cast_shadows();
        let first = rl.cast_shadows(false, (0, 1), told("starter"));
        let joined = rl.cast_shadows(false, (5, 1), told("joiner"));
        drop(hold);
        assert!(first == CastShadows::Pending && joined == CastShadows::Pending, "joined while under way");
        let mut heard = vec![landings.recv_timeout(std::time::Duration::from_secs(120)).unwrap()];
        heard.push(landings.recv_timeout(std::time::Duration::from_secs(120)).unwrap());
        heard.sort_unstable();
        assert_eq!(heard, ["joiner", "starter"]);
        assert!(matches!(rl.cast_shadows(true, (0, 1), || {}), CastShadows::Ready(_)));
    }

    /// Read in place with only its overview held, it is drawn without cast shadows, and says so;
    /// with the whole image held, it has them.
    #[test]
    fn cast_shadows_of_a_raster_read_in_place_need_the_whole_image() {
        let mut rl = tower(1.0);
        rl.in_place = true;
        assert!(matches!(rl.cast_shadows(true, (0, 1), || {}), CastShadows::Ready(_)));
        let mut r = (**rl.data.as_ref().unwrap()).clone();
        r.overview = Some((80, 80));
        rl.data = Some(Arc::new(r));
        let CastShadows::Unavailable(why) = rl.cast_shadows(true, (0, 1), || {}) else { panic!("said") };
        assert!(why.contains("read in place"), "{why}");
    }

    /// The sky is worked out once whatever the sun, beside the owner's shadows (neither stops the
    /// other's work); read in place with only its overview held, it is drawn without, and said.
    #[test]
    fn the_sky_is_kept_whatever_the_sun() {
        let mut rl = tower(1.0);
        assert_eq!(rl.sky_view(true, (0, 1), || {}), CastShadows::None, "a sky of 0 has none");
        if let RasterRenderer::ShadedRelief { sky, .. } = &mut rl.style.renderer {
            *sky = 1.0;
        }
        let hold = hold_cast_shadows();
        assert_eq!(rl.sky_view(false, (0, 1), || {}), CastShadows::Pending);
        assert_eq!(rl.cast_shadows(false, (0, 1), || {}), CastShadows::Pending);
        {
            let jobs = rl.cast.0.lock().unwrap();
            assert_eq!(jobs.len(), 2, "both kept");
            assert!(jobs.iter().all(|j| !j.work.stop.load(std::sync::atomic::Ordering::SeqCst)), "neither stopped");
        }
        drop(hold);
        let CastShadows::Ready(sky) = rl.sky_view(true, (0, 1), || {}) else { panic!("waited for") };
        assert!(matches!(rl.cast_shadows(true, (0, 1), || {}), CastShadows::Ready(_)));
        // Beside the tower, the directions toward it are hidden (about 3 of 16); its top sees all.
        assert!((160..240).contains(&sky[10 * 20 + 9]), "{}", sky[10 * 20 + 9]);
        assert_eq!(sky[10 * 20 + 10], 255);
        // Another sun, or another strength: the same sky.
        if let RasterRenderer::ShadedRelief { azimuth, sky, .. } = &mut rl.style.renderer {
            (*azimuth, *sky) = (200.0, 0.4);
        }
        assert!(matches!(rl.sky_view(false, (0, 1), || {}), CastShadows::Ready(g) if Arc::ptr_eq(&g, &sky)));
        let mut r = (**rl.data.as_ref().unwrap()).clone();
        r.overview = Some((80, 80));
        rl.in_place = true;
        rl.data = Some(Arc::new(r));
        let CastShadows::Unavailable(why) = rl.sky_view(true, (0, 1), || {}) else { panic!("said") };
        assert!(why.contains("sky visibility") && why.contains("read in place"), "{why}");
    }
}
