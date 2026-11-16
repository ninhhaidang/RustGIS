//! LiDAR point clouds (LAS, LAZ, COPC) as a source: an evenly spread sample of the file's points is
//! read once ([`crate::lidar`]), and views are answered from it, so projects save the file's path
//! rather than its points. The original point count is retained so a display sample cannot be
//! materialized as a complete cloud.

use super::{Batch, FeatureSource, Request, SourceInfo};
use crate::Result;
use crate::{Dataset, Feature, Geometry};
use rustgis_geom::Extent;
use std::path::Path;

pub struct LidarSource {
    info: SourceInfo,
    features: Vec<Feature>,
    skipped: usize,
    crs_incomplete: bool,
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
    fn from_read((ds, report, count): (Dataset, crate::ReadReport, u64)) -> Result<LidarSource> {
        let Dataset { name, geometry_type, fields, features, crs_incomplete, .. } = ds;
        let extent = features.iter().filter_map(|f| f.geometry.as_ref()).fold(Extent::EMPTY, |e, g| e.union(&g.extent()));
        // The reader's messages start with the layer's name, which the source's notes add.
        let notes = report.messages.iter().map(|m| m.strip_prefix(&format!("{name}: ")).unwrap_or(m).to_string()).collect();
        let info = SourceInfo {
            name,
            format: "LiDAR",
            geometry_type,
            fields,
            extent,
            count: Some(count),
            crs: "EPSG:4326".into(),
            indexed: false,
            notes,
            zm_dropped: false,
        };
        Ok(LidarSource { info, features: features.into_vec(), skipped: report.skipped, crs_incomplete })
    }
}

impl FeatureSource for LidarSource {
    fn info(&self) -> &SourceInfo {
        &self.info
    }

    /// Conversion failures were already disclosed at open, rather than omitted by sampling.
    fn skipped_at_open(&self) -> usize {
        self.skipped
    }

    /// A projection repair must re-read the original file after a transform discarded points.
    fn lost_to_crs(&self) -> bool {
        self.crs_incomplete
    }

    /// Answers from retained points; a sample never constitutes a complete source read.
    fn read(&self, req: &Request) -> Result<Batch> {
        let inside = |f: &&Feature| match (req.extent, &f.geometry) {
            (None, _) => true,
            (Some(e), Some(Geometry::Point(c))) => c.x >= e.min_x && c.x <= e.max_x && c.y >= e.min_y && c.y <= e.max_y,
            (Some(e), Some(g)) => e.intersects(&g.extent()),
            (Some(_), None) => false,
        };
        // One pass over the points (references, not copies), then every n-th match past the
        // limit, so the view stays covered evenly.
        let hits: Vec<&Feature> = self.features.iter().filter(inside).collect();
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
        })
    }
}
