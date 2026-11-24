//! LiDAR point clouds (LAS, LAZ, COPC) as a source: an evenly spread sample of the file's points is
//! read once ([`crate::lidar`]), and views are answered from it, so projects save the file's path
//! rather than its points. The original point count is retained so a display sample cannot be
//! materialized as a complete cloud.

use super::{Batch, FeatureSource, Request, SourceInfo};
use crate::Result;
use crate::raster::Crs;
use crate::{Dataset, Feature, Geometry};
use rustgis_geom::{Coord, Extent};
use std::path::Path;

pub struct LidarSource {
    info: SourceInfo,
    features: Vec<Feature>,
    skipped: usize,
    /// The system the points are held in, as the file holds them ([`FeatureSource::coord_crs`]).
    crs: Crs,
    /// Each point's place in longitude/latitude, in the order of `features`, for a read of a box
    /// there to test (NaN for one with none, which no box holds); `None` when the points are held
    /// in longitude/latitude.
    places: Option<Vec<Coord>>,
}

impl LidarSource {
    /// Opens a local cloud for display without presenting its sample as the whole file.
    pub fn open_path(path: &Path, name: &str) -> Result<LidarSource> {
        let f = std::io::BufReader::new(crate::provenance::open(path)?);
        Self::from_read(crate::lidar::read_sample(f, name)?)
    }

    /// Opens an uploaded cloud for display, retaining the original point count.
    pub fn open_bytes(bytes: &[u8], name: &str) -> Result<LidarSource> {
        Self::from_read(crate::lidar::read_sample(std::io::Cursor::new(bytes), name)?)
    }

    /// Keeps the file count separate from its retained display sample.
    fn from_read(cloud: crate::lidar::Cloud) -> Result<LidarSource> {
        let crate::lidar::Cloud { ds, report, count, places } = cloud;
        let crs = ds.coord_crs();
        let Dataset { name, geometry_type, fields, features, .. } = ds;
        // The extent in longitude/latitude, from the places that have one there.
        let extent = match &places {
            Some(places) => places.iter().filter(|c| c.is_finite()).fold(Extent::EMPTY, |mut e, c| {
                e.expand(*c);
                e
            }),
            None => features.iter().filter_map(|f| f.geometry.as_ref()).fold(Extent::EMPTY, |e, g| e.union(&g.extent())),
        };
        // The reader's messages start with the layer's name, which the source's notes add.
        let prefix = format!("{name}: ");
        let notes = report.messages.into_iter().map(|m| m.map_text(|t| t.strip_prefix(&prefix).map(str::to_string).unwrap_or(t))).collect();
        let info = SourceInfo {
            name,
            format: "LiDAR",
            geometry_type,
            fields,
            extent,
            count: Some(count),
            crs: crs.definition_text(),
            indexed: false,
            notes,
            zm_dropped: false,
        };
        Ok(LidarSource { info, features: features.into_vec(), skipped: report.skipped, crs, places })
    }
}

impl FeatureSource for LidarSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// The file's own system, which its points are held in.
    fn coord_crs(&self) -> Crs {
        self.crs
    }

    /// A box in the file's own system is tested against the points as they are.
    fn reads_own_box(&self) -> bool {
        true
    }

    /// Points that couldn't be read were already disclosed at open, rather than omitted by
    /// sampling.
    fn skipped_at_open(&self) -> usize {
        self.skipped
    }

    /// The notes at open count the points kept with no place in longitude/latitude.
    fn says_unplaced_at_open(&self) -> bool {
        true
    }

    /// Answers from retained points; a sample never constitutes a complete source read. A box in
    /// the file's own system ([`Request::own`]) is tested against the points as they are, else
    /// the extent against their places in longitude/latitude, which a point with none is outside
    /// of.
    fn read(&self, req: &Request) -> Result<Batch> {
        let own = req.own.filter(|(crs, _)| self.places.is_some() && *crs == self.crs).map(|(_, e)| e);
        let within = |c: Coord, e: &Extent| c.x >= e.min_x && c.x <= e.max_x && c.y >= e.min_y && c.y <= e.max_y;
        let inside = |(i, f): &(usize, &Feature)| match (own.as_ref().or(req.extent.as_ref()), &f.geometry) {
            (None, _) => true,
            (Some(_), None) => false,
            (Some(e), Some(g)) => match (&self.places, own.is_some(), g) {
                (Some(places), false, _) => places.get(*i).is_some_and(|c| within(*c, e)),
                (_, _, Geometry::Point(c)) => within(*c, e),
                (_, _, g) => e.intersects(&g.extent()),
            },
        };
        // One pass over the points (references, not copies), then every n-th match past the
        // limit, so the view stays covered evenly.
        let hits: Vec<&Feature> = self.features.iter().enumerate().filter(inside).map(|(_, f)| f).collect();
        // A window can only contain points retained in the display sample. Even when every
        // retained point matches, an oversized or damaged source has not been read whole.
        // Known conversion skips are accounted for separately in the whole read's report.
        let count = self.info.count.map(|n| n.saturating_sub(self.skipped as u64));
        let incomplete = count.is_some_and(|n| n > self.features.len() as u64);
        let matched = if req.extent.is_none() { count.unwrap_or(hits.len() as u64) } else { hits.len() as u64 };
        let limit = req.limit.max(1);
        let step = hits.len().div_ceil(limit).max(1);
        let features: Vec<Feature> = hits.into_iter().step_by(step).take(limit).cloned().collect();
        Ok(Batch {
            complete: !incomplete && features.len() as u64 == matched,
            matched,
            features,
            skipped: 0,
            unread_dates: Vec::new(),
            rounded_dates: Vec::new(),
            null_text: Vec::new(),
            notes: Default::default(),
            not_utf8: Vec::new(),
        })
    }
}
