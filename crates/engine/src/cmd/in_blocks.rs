//! A raster made a block at a time from a large raster file read in place (#2865). A command that
//! works cell by cell, or over a cell's neighbours (an index, a raster function), reads its input
//! a block of tiles at a time, at the file's full size, where reading it whole would read the
//! finest level of it that fits; one over its neighbours reads each block with a margin of the
//! cells around it. A result that fits
//! is held and saved with the project as any other; one too large to hold is written a row of
//! tiles at a time to a tiled GeoTIFF in the project's results folder
//! ([`rustgis_data::raster::cogwrite::TileStream`]), which its layer then reads in place, as a
//! large file added from this computer is read: what is held at once is a few blocks and a few
//! rows of tiles, whatever the raster's size.
//!
//! The results folder is the one project format 2 writes tool results to (#1853, piece 5): beside
//! the saved project, named after it (`Survey.rgis.data` for `Survey.rgis`), or in RustGIS's own
//! folder for a project not saved yet; the result says where its file went. Only the desktop
//! reads files in place, so none of this is built for the browser.

use std::path::{Path, PathBuf};

use rustgis_analysis::rasterops::NODATA;
use rustgis_data::raster::{ColorHint, Part, Raster, cogwrite};
use serde_json::Value;

use super::workflows::{Input, Made};
use crate::Session;

/// Columns of a block: with the tiles' rows ([`cogwrite::TILE`]), 2 million pixels, a few tens of
/// megabytes for an image of a few bands.
const BLOCK_COLUMNS: u32 = if cfg!(test) { 384 } else { 4096 };

/// The columns and rows of a block ([`BLOCK_COLUMNS`], and the tiles' rows).
pub(crate) const BLOCK: (u32, u32) = (BLOCK_COLUMNS, cogwrite::TILE);

/// The most samples, tiles' padding included, of the overview the result's layer holds: what a
/// layer reading a file in place holds of it (`rustgis_tiles::cog::raster::overview_level`), so
/// it holds the same overview once the project is opened again.
const OVERVIEW_SAMPLES: usize = if cfg!(test) { 1 << 16 } else { 2 << 20 };

/// The cells around a block a raster function reads with it (`function`): none for one that works
/// cell by cell, one for one over a cell's eight neighbours. `None` for one that doesn't work in
/// blocks (a resample, onto another grid).
pub(crate) fn function_halo(function: &str) -> Option<u32> {
    match function {
        "ndvi" | "bandMath" | "remap" | "threshold" => Some(0),
        "slope" | "aspect" | "hillshade" => Some(1),
        _ => None,
    }
}

/// The cells around a block a chain of raster functions (`steps`) reads with it: what each step
/// reads, added, as each works on the one before. `None` when a step doesn't work in blocks; when
/// it remaps or thresholds after a step over neighbours, as its ranges would then be tested on
/// a block's margin too, whose cells are made from fewer neighbours than in the whole raster; or
/// when a slope in percent, whose cells past Float32's range are counted where they're kept,
/// isn't its last step.
pub(crate) fn chain_halo(steps: &[Value]) -> Option<u32> {
    let function = |st: &Value| st.get("function").and_then(Value::as_str).unwrap_or_default().to_string();
    let after = steps.iter().skip_while(|st| function_halo(&function(st)) == Some(0));
    if after.into_iter().any(|st| matches!(function(st).as_str(), "remap" | "threshold")) {
        return None;
    }
    let percent = |st: &Value| function(st) == "slope" && st.get("units").and_then(Value::as_str) == Some("percent");
    if steps.iter().rev().skip(1).any(percent) {
        return None;
    }
    steps.iter().try_fold(0u32, |h, st| Some(h + function_halo(&function(st))?))
}

/// Whether command `id` with `params` works in blocks over a large file, and makes one band:
/// Indices, Classification Tools, and Raster Functions and the Function Editor whose functions do
/// ([`function_halo`], [`chain_halo`]).
fn in_blocks(id: &str, params: &Value) -> bool {
    match id {
        "raster.index" | "raster.classifySupervised" => true,
        "raster.function" => params.get("function").and_then(Value::as_str).and_then(function_halo).is_some(),
        "raster.functionChain" => params.get("steps").and_then(Value::as_array).is_some_and(|s| !s.is_empty() && chain_halo(s).is_some()),
        _ => false,
    }
}

/// The plan to make a raster a block at a time ([`plan`]): its input, what the result is called,
/// and, for a result too large to hold, the folder its file goes in.
pub(crate) struct Blocks {
    input: Input,
    /// The input layer's name, and its file.
    layer: String,
    path: String,
    /// The result's name.
    out: String,
    folder: Option<PathBuf>,
    /// The full image's columns and rows.
    full: (u32, u32),
    /// The cells around each block read with it ([`Blocks::with_halo`]).
    halo: u32,
    /// The no-data value its first block was read with, shared with its [`Blocks::reading`].
    nodata: Nodata,
}

/// The no-data value of a run's first block, which every other block it reads must have: a
/// sidecar beside the file changed while it runs would give later blocks another. Shared by a
/// plan and the reading it gives, so a command that reads the file twice (training, then
/// classifying) reads it with one value.
type Nodata = std::sync::Arc<std::sync::OnceLock<Option<u64>>>;

/// What a raster made a block at a time is: its band's name (none: the band's number); whether
/// it is a hillshade's whole numbers 1 to 255, held and written as bytes with 0 for no value
/// ([`rustgis_analysis::rasterops::derive_hillshade`]), rather than floats; whether it holds
/// classes (a remap's, a threshold's), whose overviews take a pixel rather than an average; and,
/// for a function whose values can pass a float's range (a slope in percent), what to say of the
/// cells that do, an error counting them.
#[derive(Default)]
pub(crate) struct Shape {
    pub(crate) band: Option<String>,
    pub(crate) bytes: bool,
    pub(crate) classes: bool,
    pub(crate) overflow: Option<&'static str>,
}

impl Shape {
    /// The value a cell with none takes, and the type its values are held as.
    fn nodata(&self) -> (f32, &'static str) {
        if self.bytes { (0.0, "UInt8") } else { (NODATA, "Float32") }
    }

    /// How its file stores its band.
    fn samples(&self) -> cogwrite::Samples {
        match (self.bytes, self.classes) {
            (true, _) => cogwrite::Samples::Hillshade,
            (false, true) => cogwrite::Samples::Classes,
            (false, false) => cogwrite::Samples::Floats,
        }
    }
}

/// The most samples a cell-by-cell command reads whole of a file, and holds of a result:
/// [`rustgis_data::raster::SAMPLES_FLOOR`], what any desktop reads whole, or under test what
/// [`BLOCKS_PAST`] sets on this thread. Not the computer's own limit, which follows what it has
/// free: the permission check and the run decide the same way however that changes between them.
fn whole_limit() -> usize {
    #[cfg(test)]
    if let Some(max) = BLOCKS_PAST.with(std::cell::Cell::get) {
        return max;
    }
    rustgis_data::raster::SAMPLES_FLOOR
}

#[cfg(test)]
thread_local! {
    /// [`whole_limit`]'s limit on this thread, under test: no small fixture reaches the real one.
    pub(crate) static BLOCKS_PAST: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// The full size of `input`'s file, when it is a layer read in place from a file that a run
/// can't read whole at that size: its samples' bytes as read, 8 a sample (64-bit samples, which
/// the overview held may not say it has), past the 4 a sample of what a run reads whole, as the
/// GeoTIFF reader decides it.
fn too_large(input: &Input) -> Option<(u32, u32)> {
    input.file()?;
    let held = input.held();
    let full = held.overview?;
    let samples = u64::from(full.0).saturating_mul(u64::from(full.1)).saturating_mul(held.band_count().max(1) as u64);
    (samples.saturating_mul(8) > (whole_limit() as u64).saturating_mul(4)).then_some(full)
}

/// Whether a result of `samples` samples is held as any other, rather than written to a file
/// ([`written`]): one made from blocks of a larger file that only works held (a resample's) is
/// made so only then.
pub(crate) fn holds(samples: u64) -> bool {
    samples <= whole_limit() as u64
}

/// Whether a result `full` pixels in size, one band, is too large to hold: it is then written to
/// a file and read in place, as a file that size added from this computer is.
fn written(full: (u32, u32)) -> bool {
    u64::from(full.0).saturating_mul(u64::from(full.1)) > whole_limit() as u64
}

/// Whether command `id` with `params` writes its result to a file in the results folder: a
/// command that works in blocks ([`in_blocks`]) over a layer too large to read whole, whose
/// result is too large to hold. The permission check adds that it writes files.
pub(crate) fn writes_file(context: &impl super::InputSource, id: &str, params: &Value) -> bool {
    if !in_blocks(id, params) {
        return false;
    }
    let Some(name) = params.get("layer").and_then(Value::as_str) else { return false };
    Input::of(context, name).ok().as_ref().and_then(too_large).is_some_and(written)
}

/// The plan to make `out` from `input` a block at a time, when `input` is a layer read in place
/// from a file too large to read whole at its full size ([`too_large`]); `None` otherwise, and
/// the command reads its input as before. A step of a script that nobody can stop while it runs
/// (the app's, which runs on its window's thread) is refused, as is a result too large to hold
/// when this computer gives no folder to write it to.
pub(crate) fn plan(s: &Session, input: &Input, out: &str) -> Result<Option<Blocks>, String> {
    let (Some(full), Some((layer, path))) = (too_large(input), input.file()) else { return Ok(None) };
    if s.steps_running > 0 && s.on_window_thread {
        return Err(format!(
            "{layer}: its full {} × {} pixels are made a block at a time, which a script step can't do without holding the window: run it on its own",
            full.0, full.1
        ));
    }
    let folder = match written(full) {
        true => Some(folder(s).ok_or("this computer gives RustGIS no folder to write the result to: save the project first")?),
        false => None,
    };
    Ok(Some(Blocks {
        input: input.clone(),
        layer: layer.to_string(),
        path: path.to_string(),
        out: out.to_string(),
        folder,
        full,
        halo: 0,
        nodata: Nodata::default(),
    }))
}

/// The full size of `input`'s file when a command reads it a block at a time ([`plan`]), for one
/// whose result is held whatever its size (a resample's, smaller than the file): `None` when the
/// file isn't too large to read whole at its full size. The command that goes on to read it so
/// asks [`Reading::refused_in_script`] first, as [`plan`] does.
pub(crate) fn plan_read(input: &Input) -> Option<Reading> {
    let (full, (layer, path)) = (too_large(input)?, input.file()?);
    Some(Reading { input: input.clone(), layer: layer.to_string(), path: path.to_string(), full, nodata: Nodata::default() })
}

/// Whether the work can plan for `need` bytes more: within what a job may take now
/// ([`rustgis_data::memory::share`], with what RustGIS holds and the computer has free), or, where
/// that isn't known, within the floor's worth of samples.
pub(crate) fn affordable(need: u64) -> bool {
    use rustgis_data::memory;
    #[cfg(test)]
    if let Some(max) = AFFORDS.with(std::cell::Cell::get) {
        return need <= max;
    }
    let budget = memory::share(memory::physical(), memory::held(), memory::available());
    need <= budget.unwrap_or((rustgis_data::raster::SAMPLES_FLOOR as u64).saturating_mul(4))
}

#[cfg(test)]
thread_local! {
    /// What [`affordable`] lets a run plan for on this thread, under test: a computer short of
    /// memory, which no test can make.
    pub(crate) static AFFORDS: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
}

/// A large file read in place, read a block at a time by a command that makes its own result
/// ([`plan_read`]): its layer's input, name and file, and its full size.
pub(crate) struct Reading {
    input: Input,
    layer: String,
    path: String,
    full: (u32, u32),
    nodata: Nodata,
}

impl Reading {
    /// An error for a step of a script on the app's window thread, which reading the file a block
    /// at a time would hold, as [`plan`] refuses one.
    pub(crate) fn refused_in_script(&self, s: &Session) -> Result<(), String> {
        if s.steps_running > 0 && s.on_window_thread {
            return Err(format!(
                "{}: its full {} × {} pixels are read a block at a time, which a script step can't do without holding the window: run it on its own",
                self.layer, self.full.0, self.full.1
            ));
        }
        Ok(())
    }

    /// The file's full columns and rows.
    pub(crate) fn full(&self) -> (u32, u32) {
        self.full
    }

    /// The input layer's name.
    pub(crate) fn layer(&self) -> &str {
        &self.layer
    }

    /// The raster the layer holds: the file's overview, with its bands, extent and system.
    pub(crate) fn held(&self) -> &Raster {
        self.input.held()
    }

    /// The file's full grid, as a read of all of it places it: from a part of one pixel.
    pub(crate) fn grid(&self) -> Result<rustgis_data::raster::GeoTransform, String> {
        let source = Source { input: &self.input, layer: &self.layer, path: &self.path, full: self.full, nodata: &self.nodata };
        Ok(source.block((0, 1), (0, 1))?.0.grid.0)
    }

    /// Reads the blocks of columns `cols` and rows `rows` (half-open, at the full size), each of
    /// `size` columns and rows from their start, in turn, and gives each to `each` with its
    /// columns and rows: at the file's full size, holding all of them, counted as the run's
    /// progress (its own steps not), Cancel looked at between them. Gives how many values were
    /// rounded as they were read.
    pub(crate) fn each_block(
        &self,
        cols: (u32, u32),
        rows: (u32, u32),
        size: (u32, u32),
        mut each: impl FnMut((u32, u32), (u32, u32), &rustgis_data::raster::PartRead) -> Result<(), String>,
    ) -> Result<u64, String> {
        let source = Source { input: &self.input, layer: &self.layer, path: &self.path, full: self.full, nodata: &self.nodata };
        let (bw, bh) = (size.0.max(1), size.1.max(1));
        let (across, down) = (cols.1.saturating_sub(cols.0).div_ceil(bw), rows.1.saturating_sub(rows.0).div_ceil(bh));
        rustgis_data::progress::begin((across as usize).saturating_mul(down as usize));
        let mut rounded = 0u64;
        for by in 0..down {
            let (r0, r1) = (rows.0 + by * bh, (rows.0 + (by + 1) * bh).min(rows.1));
            for bx in 0..across {
                let (c0, c1) = (cols.0 + bx * bw, (cols.0 + (bx + 1) * bw).min(cols.1));
                rustgis_analysis::control::uncounted(|| -> Result<_, String> {
                    let (read, _count) = source.block((c0, c1), (r0, r1))?;
                    rounded = rounded.saturating_add(read.raster.rounded.iter().sum::<u64>());
                    each((c0, c1), (r0, r1), &read)
                })?;
                rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
            }
        }
        Ok(rounded)
    }
}

/// The file name, before its extension, of a result called `name`: its safe form
/// ([`super::items::safe`]), cut at a character's end to 200 bytes so that with a number, the
/// extension and the name it is written under first it stays within the 255 bytes a file name
/// can have.
fn stem(name: &str) -> String {
    let safe = super::items::safe(name);
    let end = safe.char_indices().map(|(i, c)| i + c.len_utf8()).take_while(|end| *end <= 200).last().unwrap_or(0);
    safe.get(..end).unwrap_or_default().to_string()
}

/// Makes the results folder `folder` (and the folders it is in): whether this call made it, rather
/// than finding it there.
fn make_folder(folder: &Path) -> Result<bool, String> {
    if let Some(parent) = folder.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    match std::fs::create_dir(folder) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(format!("{}: {e}", folder.display())),
    }
}

/// The project's results folder: beside the saved project, named after its file
/// (`Survey.rgis.data`), or in RustGIS's own folder for one not saved yet. `None` when the
/// computer gives none.
fn folder(s: &Session) -> Option<PathBuf> {
    match s.path.as_deref() {
        Some(project) => {
            let name = project.file_name().and_then(|n| n.to_str()).unwrap_or("Project");
            Some(project.parent().unwrap_or(Path::new(".")).join(format!("{name}.data")))
        }
        None => rustgis_data::dirs::app_data_dir().map(|d| d.join("results")),
    }
}

impl Blocks {
    /// The input layer's file read a block at a time ([`Reading::each_block`]), as by this plan.
    pub(crate) fn reading(&self) -> Reading {
        Reading {
            input: self.input.clone(),
            layer: self.layer.clone(),
            path: self.path.clone(),
            full: self.full,
            nodata: self.nodata.clone(),
        }
    }

    /// The plan reading `halo` cells around each block with it, for work over a cell's
    /// neighbours: a block's cells are made from the cells around them as from the whole raster's.
    pub(crate) fn with_halo(mut self, halo: u32) -> Blocks {
        self.halo = halo;
        self
    }

    /// Makes the raster: `cells` turns a block of the input (all its bands, at the file's full
    /// size) into the result's values for its cells, row by row, `NODATA` for none. Each block is
    /// read and made in turn, counted as the run's progress, Cancel looked at between them. A
    /// result too large to hold is written a row of tiles at a time beside its place in the
    /// results folder, and put there as the layer is added, which reads it in place; a run
    /// cancelled, failed or never added leaves no file behind, nor the results folder when it made
    /// it (not the folders around it, the project's own when it is saved) while
    /// nothing else is in it (one another run wrote into too can stay, empty, once both their
    /// results are gone).
    /// It is of `shape`: a hillshade's values below 1 have no value, as a whole hillshade's.
    pub(crate) fn run(
        self,
        cells: impl Fn(&Raster, &rustgis_geom::Extent) -> Result<Vec<f32>, String>,
        shape: Shape,
    ) -> Result<Made, String> {
        let Blocks { input, layer, path, out, folder, full, halo, nodata: first } = self;
        let (nodata, kind) = shape.nodata();
        let cells = |r: &Raster, ground: &rustgis_geom::Extent| -> Result<Vec<f32>, String> {
            let mut v = cells(r, ground)?;
            if shape.bytes {
                v.iter_mut().filter(|v| !v.is_finite() || **v < 1.0).for_each(|v| *v = 0.0);
            }
            Ok(v)
        };
        // A margin read with each block reads its cells twice or more: a count of those rounded
        // is at most that.
        let note = |rounded: u64| super::gp::ReadNote::rounded(&layer, rounded).at_most(halo > 0);
        let named = |mut r: Raster| {
            if let (Some(b), Some(name)) = (r.bands.first_mut(), &shape.band) {
                b.name.clone_from(name);
            }
            r
        };
        let (fw, fh) = full;
        let tile = cogwrite::TILE;
        // What a row of blocks holds of the result and of its file's levels as they are written,
        // and the tables of the file's tiles: planned before any of it is set aside.
        let row_bytes = u64::from(fw).saturating_mul(u64::from(tile)).saturating_mul(4);
        let need = match &folder {
            // The row of blocks made, and the file's levels' rows waiting to be written (less
            // than a row each way, two at most at the full size, half that at the next level…),
            // the tables of its tiles, and the overview kept.
            Some(_) => cogwrite::TileStream::<std::io::Cursor<Vec<u8>>>::peak(fw, fh, OVERVIEW_SAMPLES).saturating_add(row_bytes),
            None => u64::from(fw).saturating_mul(u64::from(fh)).saturating_mul(4).saturating_add(row_bytes),
        };
        // Within what any run of rasters plans for ([`rustgis_data::raster::max_samples`]),
        // floor included, as a whole read plans.
        if need > (rustgis_data::raster::max_samples() as u64).saturating_mul(8) {
            return Err(format!(
                "{layer}: making it a block at a time at its full {fw} × {fh} needs {} MB at once, more than this computer can give now: clip it to a smaller area first",
                need >> 20
            ));
        }
        let _held = rustgis_data::memory::Holding::of(need);
        let blocks = Source { input: &input, layer: &layer, path: &path, full, nodata: &first };
        // The kept cells whose values pass a float's range (none for most functions): counted in
        // the rows made, which hold no margin, so a margin's cell, made from too few neighbours,
        // isn't counted.
        let over = std::cell::Cell::new(0u64);
        let count = |rows: &[f32]| {
            if shape.overflow.is_some() {
                over.set(over.get() + rows.iter().filter(|v| v.is_infinite()).count() as u64);
            }
        };
        let overflowed = || match shape.overflow {
            Some(what) if over.get() > 0 => Err(format!("{layer}: {} {what}", over.get())),
            _ => Ok(()),
        };
        let Some(folder) = folder else {
            // A result that fits: held whole, made a block at a time.
            let mut whole = rustgis_analysis::rasterops::new_band(fw as usize, fh as usize, nodata)?;
            let (grid, crs, rounded) = blocks.each_row(&cells, halo, |r0, rows, _| {
                count(rows);
                let at = r0 as usize * fw as usize;
                if let Some(d) = whole.get_mut(at..at + rows.len()) {
                    d.copy_from_slice(rows);
                }
                Ok(())
            })?;
            overflowed()?;
            let r = Raster::new(out.clone(), fw, fh, vec![whole], grid, crs, Some(f64::from(nodata)), ColorHint::Gray, kind)
                .map_err(|e| e.to_string())?;
            let r = named(r);
            let notes = super::gp::tiled_read_notes(&[note(rounded)], "its");
            let then: super::workflows::Then = Box::new(move |s: &mut Session, _: &mut Value| {
                for note in notes {
                    s.notify(note, false);
                }
            });
            return super::workflows::made_then(r, None, Some(then));
        };
        // A folder this run makes goes again with a result never added, while nothing else is in
        // it; one already there stays, whatever is in it.
        let mut made_folder = make_folder(&folder)?;
        let (mut taken, mut i, mut remade) = (None, 1u32, 0);
        while i < 10_000 {
            let stem = stem(&out);
            let dest = folder.join(if i == 1 { format!("{stem}.tif") } else { format!("{stem} {i}.tif") });
            // The file's name is reserved before it is written: two runs never take the same one.
            let wrote = rustgis_data::io::stage_streamed_new(&dest, made_folder.then_some(folder.as_path()), |w| {
                let (mut sink, mut stream) = (Some(w), None);
                let (_, _, rounded) = blocks
                    .each_row(&cells, halo, |_, rows, (grid, crs)| {
                        count(rows);
                        // Placed on the file's full grid, as a read of all of it places it.
                        if stream.is_none() {
                            let t = Raster::new(
                                out.clone(),
                                1,
                                1,
                                vec![vec![nodata]],
                                grid,
                                crs,
                                Some(f64::from(nodata)),
                                ColorHint::Gray,
                                kind,
                            )
                            .map_err(|e| e.to_string())?;
                            let t = named(t);
                            let w = sink.take().ok_or("the result's file was started twice")?;
                            stream = Some(
                                cogwrite::TileStream::new(&t, fw, fh, shape.samples(), w)
                                    .map_err(|e| e.to_string())?
                                    .keeping(OVERVIEW_SAMPLES),
                            );
                        }
                        stream.as_mut().map_or(Ok(()), |s: &mut cogwrite::TileStream<_>| s.put(rows).map_err(|e| e.to_string()))
                    })
                    // A Cancel stays one, as on the path that holds its result.
                    .map_err(rustgis_data::DataError::of_conversion)?;
                let (info, overview) =
                    stream.ok_or_else(|| rustgis_data::DataError::Format(format!("{layer}: it has no pixels")))?.finish()?;
                Ok::<_, rustgis_data::DataError>((info, overview, rounded))
            });
            match wrote {
                Err(rustgis_data::DataError::Io(e)) if e.kind() == std::io::ErrorKind::AlreadyExists => i += 1,
                // Another run's result, never added, took the folder it made with it meanwhile:
                // made again (this run's now, if it makes it), the same name tried again.
                Err(rustgis_data::DataError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound && !folder.exists() && remade < 8 => {
                    made_folder = make_folder(&folder)?;
                    remade += 1;
                }
                Err(e) => {
                    if made_folder {
                        let _ = std::fs::remove_dir(&folder);
                    }
                    return Err(match e {
                        rustgis_data::DataError::Cancelled => e.to_string(),
                        e => format!("{}: {e}", dest.display()),
                    });
                }
                Ok(w) => {
                    taken = Some((dest, w));
                    break;
                }
            }
        }
        let (dest, (staged, (info, overview, rounded))) =
            taken.ok_or_else(|| format!("{}: every name for {out} is taken", folder.display()))?;
        // Refused, its file goes with it.
        overflowed()?;
        rustgis_data::progress::check().map_err(|e| e.to_string())?;
        // The layer holds an overview of the file, as one added from it would.
        let overview = overview.ok_or("the result's overview wasn't kept")?;
        let file = dest.display().to_string();
        let rl = rustgis_map::RasterLayer::tiled_file(overview, file.clone());
        let (w, h) = info.levels.first().copied().unwrap_or_default();
        let mut notes = vec![format!(
            "{out}: made a block at a time from {layer} at its full {w} × {h}, and written to {file}, where the layer reads it from"
        )];
        notes.extend(super::gp::tiled_read_notes(&[note(rounded)], "its"));
        let then: super::workflows::Then = Box::new(move |s: &mut Session, v: &mut Value| {
            for note in notes {
                s.notify(note, false);
            }
            if let Some(o) = v.as_object_mut() {
                o.insert("path".into(), Value::String(file));
            }
        });
        Ok(super::workflows::made_layer(out, rl, None, Some(then)).committing(staged, Vec::new()))
    }
}

/// A file read a block at a time ([`Source::each_row`]): the layer's input, name and file, and
/// the file's full size.
struct Source<'a> {
    input: &'a Input,
    layer: &'a str,
    path: &'a str,
    full: (u32, u32),
    /// The no-data value the run's first block was read with, which every other one must have.
    nodata: &'a Nodata,
}

impl Source<'_> {
    /// `part` of the file, read at its full size, holding all of columns `cols` and rows `rows`,
    /// with where it lies on the full grid, and its count in [`rustgis_data::memory::held`].
    fn block(
        &self,
        cols: (u32, u32),
        rows: (u32, u32),
    ) -> Result<(rustgis_data::raster::PartRead, std::sync::Arc<rustgis_data::memory::Holding>), String> {
        let part = Part::pixels(cols, rows, self.full).ok_or_else(|| format!("{}: a block of no pixels", self.layer))?;
        let (read, count) = super::gp::read_tiled_part(self.layer, self.path, part, 1, self.input.held())?.ok_or_else(|| {
            format!("{}: its file can't be read in blocks: it isn't the file the layer was read from any more (Reload Data)", self.layer)
        })?;
        let r = &read.raster;
        // One no-data value for every block of a run ([`Nodata`]).
        let nodata = r.nodata.map(f64::to_bits);
        if *self.nodata.get_or_init(|| nodata) != nodata {
            return Err(format!("{}: its no-data value changed while it was read (Reload Data)", self.layer));
        }
        let (ax, ay) = read.at;
        if r.overview.is_some() || ax > cols.0 || ay > rows.0 || ax + r.width < cols.1 || ay + r.height < rows.1 {
            return Err(format!("{}: a block of its file couldn't be read at its full size", self.layer));
        }
        Ok((read, count))
    }

    /// Makes the result a row of blocks at a time: `cells` makes each block's values (read with
    /// `halo` cells around it, whose values it makes too, and which are left out), and `row`
    /// is given each full row of blocks (its first row, its rows of the full width, and the
    /// file's full grid and system) as it is made. Each block is counted as the run's progress, its own steps not
    /// ([`rustgis_analysis::control::uncounted`]), and Cancel is looked at between them. Gives the
    /// full grid, its system, and how many values were rounded as they were read.
    fn each_row(
        &self,
        cells: &impl Fn(&Raster, &rustgis_geom::Extent) -> Result<Vec<f32>, String>,
        halo: u32,
        mut row: impl FnMut(u32, &[f32], (rustgis_data::raster::GeoTransform, rustgis_data::raster::Crs)) -> Result<(), String>,
    ) -> Result<(rustgis_data::raster::GeoTransform, rustgis_data::raster::Crs, u64), String> {
        let (fw, fh) = self.full;
        let tile = cogwrite::TILE;
        let (across, down) = (fw.div_ceil(BLOCK_COLUMNS), fh.div_ceil(tile));
        rustgis_data::progress::begin((across as usize).saturating_mul(down as usize));
        let (mut rounded, mut placed) = (0u64, None);
        for by in 0..down {
            let (r0, r1) = (by * tile, ((by + 1) * tile).min(fh));
            let mut rows = rustgis_analysis::rasterops::new_band(fw as usize, (r1 - r0) as usize, NODATA)?;
            for bx in 0..across {
                let (c0, c1) = (bx * BLOCK_COLUMNS, ((bx + 1) * BLOCK_COLUMNS).min(fw));
                // The block's own steps don't count: the run counts blocks.
                // The block's count in what is held lasts until its values are copied into the row.
                let ((read, _count), values) = rustgis_analysis::control::uncounted(|| -> Result<_, String> {
                    // With the cells around it, within the image.
                    let around = |(a, b): (u32, u32), n: u32| (a.saturating_sub(halo), b.saturating_add(halo).min(n));
                    let read = self.block(around((c0, c1), fw), around((r0, r1), fh))?;
                    let values = cells(&read.0.raster, &read.0.bounds)?;
                    Ok((read, values))
                })?;
                let r = &read.raster;
                let (ax, ay) = read.at;
                if values.len() != r.width as usize * r.height as usize {
                    return Err(format!("{}: a block's values aren't its pixels", self.layer));
                }
                placed.get_or_insert((read.grid.0, r.crs));
                rounded = rounded.saturating_add(r.rounded.iter().sum::<u64>());
                let pw = r.width as usize;
                for y in r0..r1 {
                    let src = (y - ay) as usize * pw + (c0 - ax) as usize;
                    let dst = (y - r0) as usize * fw as usize + c0 as usize;
                    let n = (c1 - c0) as usize;
                    let (Some(s), Some(d)) = (values.get(src..src + n), rows.get_mut(dst..dst + n)) else {
                        return Err(format!("{}: a block's values don't fit its place in the result", self.layer));
                    };
                    d.copy_from_slice(s);
                }
                rustgis_data::progress::step(1).map_err(|e| e.to_string())?;
            }
            let placed = placed.ok_or_else(|| format!("{}: it has no pixels", self.layer))?;
            row(r0, &rows, placed)?;
        }
        let (grid, crs) = placed.ok_or_else(|| format!("{}: it has no pixels", self.layer))?;
        Ok((grid, crs, rounded))
    }
}
