//! Validate feature lengths before the dependency resizes its buffer. Prefix errors are kept
//! separately because flatgeobuf treats a failed prefix read as a normal end of iteration.

use flatgeobuf::{FallibleStreamingIterator, FgbReader};
use std::cell::{Cell, RefCell};
use std::io::{self, Read, Seek, SeekFrom};
use std::rc::Rc;

type Failure = Rc<RefCell<Option<io::Error>>>;

/// A header reader whose selections keep prefix failures observable.
pub(super) struct Reader<R> {
    inner: FgbReader<Guard<R>>,
    failure: Failure,
}

impl<R: Read + Seek> Reader<R> {
    /// Reads the verified header, then identifies where size-prefixed features begin.
    pub(super) fn open(mut input: R) -> super::Result<Self> {
        let end = match input.seek(SeekFrom::End(0)) {
            Ok(end) => Some(end),
            Err(e) if e.kind() == io::ErrorKind::Unsupported => None,
            Err(e) => return Err(e.into()),
        };
        input.seek(SeekFrom::Start(0))?;
        let start = Rc::new(Cell::new(u64::MAX));
        let failure = Failure::default();
        let guard = Guard { input, pos: 0, end, start: start.clone(), next: None, failure: failure.clone() };
        let inner = FgbReader::open(guard).map_err(super::err)?;
        let h = inner.header();
        // FlatBuffers verifies the header, but not the index arithmetic it advertises.
        let index = if h.index_node_size() > 0 && h.features_count() > 0 {
            super::level_bounds(h.features_count(), u64::from(h.index_node_size()))
                .and_then(|levels| levels.first().and_then(|(_, end)| (*end).max(2).checked_mul(super::NODE)))
                .filter(|n| usize::try_from(*n).is_ok())
                .ok_or_else(|| super::DataError::Format("FlatGeobuf: an impossible spatial index size".into()))?
        } else {
            0
        };
        start.set(8 + h._tab.buf().len() as u64 + index);
        Ok(Self { inner, failure })
    }

    /// The verified schema and file metadata, without exposing an unchecked selection.
    pub(super) fn header(&self) -> flatgeobuf::Header<'_> {
        self.inner.header()
    }

    /// Selects a whole file, preserving errors from its first size prefix.
    pub(super) fn select_all(self) -> flatgeobuf::Result<Features<R>> {
        Ok(Features { inner: self.inner.select_all()?, failure: self.failure, seen: 0 })
    }

    /// Selects indexed features; seeks reset the prefix boundary to the selected feature.
    pub(super) fn select_bbox(self, min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> flatgeobuf::Result<Features<R>> {
        Ok(Features { inner: self.inner.select_bbox(min_x, min_y, max_x, max_y)?, failure: self.failure, seen: 0 })
    }
}

/// A selection that reports prefix failures rather than silently ending a read.
pub(super) struct Features<R> {
    seen: usize,
    inner: flatgeobuf::FeatureIter<Guard<R>, flatgeobuf::reader_trait::Seekable>,
    failure: Failure,
}

impl<R: Read + Seek> Features<R> {
    /// The dependency's selected feature count, used for stable IDs and sampling.
    pub(super) fn features_count(&self) -> Option<usize> {
        self.inner.features_count()
    }

    /// Checks failures even when selection or advance swallowed them as end-of-file.
    pub(super) fn next(&mut self) -> flatgeobuf::Result<Option<&flatgeobuf::FgbFeature>> {
        if let Some(e) = self.failure.borrow_mut().take() {
            return Err(e.into());
        }
        self.inner.advance()?;
        if let Some(e) = self.failure.borrow_mut().take() {
            return Err(e.into());
        }
        let feature = self.inner.get();
        if feature.is_none() && self.inner.features_count().is_some_and(|count| self.seen < count) {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "a missing feature size prefix").into());
        }
        self.seen += usize::from(feature.is_some());
        Ok(feature)
    }
}

/// Tracks sequential prefixes and index seeks without scanning or fetching skipped features.
struct Guard<R> {
    input: R,
    pos: u64,
    end: Option<u64>,
    start: Rc<Cell<u64>>,
    next: Option<u64>,
    failure: Failure,
}

impl<R: Read> Read for Guard<R> {
    /// Header and index reads only; feature prefixes use the checked `read_exact` below.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(buf)?;
        self.pos = self.pos.saturating_add(n as u64);
        Ok(n)
    }

    /// The dependency reads a four-byte prefix before allocating the feature's body.
    fn read_exact(&mut self, buf: &mut [u8]) -> io::Result<()> {
        if self.pos < self.start.get() || self.pos != self.next.unwrap_or(self.start.get()) {
            self.input.read_exact(buf)?;
            self.pos = self.pos.saturating_add(buf.len() as u64);
            return Ok(());
        }
        let mut filled = 0;
        let result = (|| {
            while filled < buf.len() {
                match self.read(&mut buf[filled..]) {
                    Ok(0) => return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "a truncated feature size prefix")),
                    Ok(n) => filled += n,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            let size = <[u8; 4]>::try_from(&*buf)
                .map(u32::from_le_bytes)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "an invalid feature size prefix"))?;
            if size == 0 {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "a zero-size feature"));
            }
            let size = u64::from(size);
            let remaining = self.end.map(|end| end.saturating_sub(self.pos)).unwrap_or(u64::MAX).min(super::remote::READ_BUDGET);
            if size > remaining || usize::try_from(size).ok().and_then(|n| n.checked_add(4)).is_none() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("feature size {size} exceeds the remaining file or read budget ({remaining} bytes)"),
                ));
            }
            self.next = self.pos.checked_add(size);
            Ok(())
        })();
        if let Err(e) = &result {
            // A zero-byte EOF is normal for a stream with an unspecified feature count.
            if filled > 0 || e.kind() != io::ErrorKind::UnexpectedEof {
                *self.failure.borrow_mut() = Some(io::Error::new(e.kind(), e.to_string()));
            }
        }
        result
    }
}

impl<R: Seek> Seek for Guard<R> {
    /// Index selections seek straight to a size prefix; ordinary position queries preserve it.
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let pos = self.input.seek(to)?;
        if pos != self.pos {
            self.next = (pos >= self.start.get()).then_some(pos);
        }
        self.pos = pos;
        Ok(pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_length_still_caps_feature_allocation() {
        // A sparse or large file can have enough remaining bytes for a hostile prefix.
        // Exercise the exact read intercepted before the dependency allocates its buffer.
        let size = u32::try_from(super::super::remote::READ_BUDGET + 1).unwrap();
        let mut guard = Guard {
            input: io::Cursor::new(size.to_le_bytes()),
            pos: 0,
            end: Some(u64::from(size) + 4),
            start: Rc::new(Cell::new(0)),
            next: None,
            failure: Failure::default(),
        };
        let error = guard.read_exact(&mut [0; 4]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("read budget"));
        assert!(guard.failure.borrow().is_some());
    }

    #[test]
    fn a_zero_size_prefix_is_named() {
        let mut guard = Guard {
            input: io::Cursor::new(0u32.to_le_bytes()),
            pos: 0,
            end: Some(4),
            start: Rc::new(Cell::new(0)),
            next: None,
            failure: Failure::default(),
        };
        let error = guard.read_exact(&mut [0; 4]).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "a zero-size feature");
        assert!(guard.failure.borrow().is_some());
    }
}
