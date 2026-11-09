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
        let r = rustgis_data::raster::read_path_with(std::path::Path::new(path), sub, self.crs.as_deref())
            .and_then(|r| placed(r, self.placement))
            .map_err(|e| format!("{path}: {e}"))?;
        self.validate_replacement(&r, path)?;
        let mut style = self.style.clone();
        style.repair(&r);
        Ok(RasterLayer {
            path: Some(path.into()),
            in_place: false,
            file_name: None,
            embedded: None,
            subdataset: sub.map(String::from),
            crs: self.crs.clone(),
            time_zarr: None,
            placement: self.placement,
            style,
            data: Some(Arc::new(r)),
            error: None,
            masked: MaskedStats::default(),
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
        let style = RasterStyle::default_for(&raster);
        RasterLayer {
            path: None,
            in_place: false,
            file_name: Some(file_name.into()),
            embedded: Some(Arc::new(bytes)),
            subdataset: None,
            crs: None,
            time_zarr: None,
            placement: None,
            style,
            data: Some(Arc::new(raster)),
            error: None,
            masked: MaskedStats::default(),
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
                            rustgis_data::raster::read_bytes(&name, &bytes)
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
        let r = if let Some(bytes) = &self.embedded {
            rustgis_data::raster::read_bytes(self.file_name.as_deref().unwrap_or("raster.tif"), bytes)
        } else if let Some(p) = &self.path {
            rustgis_data::raster::read_path_with(std::path::Path::new(p), self.subdataset.as_deref(), self.crs.as_deref())
        } else {
            return Err("the raster has no source".into());
        };
        // A scan goes back where it was laid (a GeoTIFF opened `as` a scan is one again).
        let r = r.and_then(|r| placed(r, self.placement));
        match r {
            Ok(r) => {
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
            shown: None,
            read_generation: next_read_generation(),
        }
    }
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

    /// The selection of step `i` as `zarr::Selection::parse` reads it: `t2m[time=12,level=3]`.
    pub fn selection(&self, i: usize) -> String {
        let mut parts = vec![format!("{}={}", self.dim, self.offset.saturating_add(i as u64))];
        parts.extend(self.index.iter().filter(|(d, _)| *d != self.dim).map(|(d, v)| format!("{d}={v}")));
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
    /// Hillshade, optionally tinted by elevation.
    ShadedRelief { band: usize, ramp: Option<Ramp>, azimuth: f64, altitude: f64, z_factor: f64 },
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
    pub fn stats(&self, r: &Raster, band: usize) -> BandStats {
        let Some(b) = r.bands.get(band) else { return BandStats::default() };
        let step = b.data.len().div_ceil(MASK_STATS_SAMPLE).max(1);
        let kept: Vec<f32> = b.data.iter().step_by(step).map(|v| if r.is_valid(*v) && !self.hides(*v) { *v } else { f32::NAN }).collect();
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

/// Valid values of a band, sorted, sampled down to about `max` of them.
pub fn sorted_sample(r: &Raster, band: usize, max: usize) -> Vec<f64> {
    sorted_sample_masked(r, band, max, &RasterMask::default())
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
        // The renderer masks values as it samples them (they arrive as NaN); this is for other callers.
        let mask = &self.style.mask;
        if mask.is_empty() || !vals.iter().any(|v| mask.hides(*v)) {
            return self.paint(vals, hillshade);
        }
        // Masked values are no data. The renderer bands are at most three.
        let mut kept = [f32::NAN; 3];
        for (k, v) in kept.iter_mut().zip(vals) {
            if !mask.hides(*v) {
                *k = *v;
            }
        }
        self.paint(kept.get(..vals.len().min(3)).unwrap_or(&kept), hillshade)
    }

    fn paint(&self, vals: &[f32], hillshade: Option<u8>) -> Option<[u8; 4]> {
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
            RasterRenderer::ShadedRelief { ramp, .. } => {
                let h = hillshade.unwrap_or(180) as f64 / 255.0;
                let base = if ramp.is_some() { *self.ramp_lut.get(self.byte(0, v0) as usize)? } else { [255, 255, 255, 255] };
                // Multiply the tint by the hillshade, keeping some ambient light.
                let m = |c: u8| (c as f64 * (0.25 + 0.75 * h)).round().clamp(0.0, 255.0) as u8;
                [m(base[0]), m(base[1]), m(base[2]), 255]
            }
        };
        (c[3] > 0).then(|| self.enhance(c))
    }
}

/// Hillshade (0–255) from a 3×3 window of elevations (row by row), Horn's method.
pub fn hillshade(z: &[f64; 9], cell_x: f64, cell_y: f64, azimuth: f64, altitude: f64, z_factor: f64) -> u8 {
    let [a, b, c, d, _, f, g, h, i] = *z;
    let dzdx = ((c + 2.0 * f + i) - (a + 2.0 * d + g)) / (8.0 * cell_x.max(1e-9));
    let dzdy = ((g + 2.0 * h + i) - (a + 2.0 * b + c)) / (8.0 * cell_y.max(1e-9));
    let slope = (z_factor * (dzdx * dzdx + dzdy * dzdy).sqrt()).atan();
    let aspect = if dzdx != 0.0 || dzdy != 0.0 { dzdy.atan2(-dzdx) } else { 0.0 };
    let zenith = (90.0 - altitude.clamp(0.0, 90.0)).to_radians();
    let az = (360.0 - azimuth + 90.0).rem_euclid(360.0).to_radians();
    let v = zenith.cos() * slope.cos() + zenith.sin() * slope.sin() * (az - aspect).cos();
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
                cats.iter()
                    .filter(|v| !mask.hides(**v as f32))
                    .filter_map(|v| p.get(*v as usize).map(|c| RasterLegend::Class { color: Rgba(*c), label: class_label(0, *v) }))
                    .collect()
            }
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
        if let Some(ZarrTime { store, variable: _, index: _, dim: _, offset: _, dates: _, crs: _, shown: _, read_generation: _ }) =
            time_zarr
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
}
