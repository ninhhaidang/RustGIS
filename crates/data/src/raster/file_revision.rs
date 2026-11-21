//! Runtime identity of the local GeoTIFF used to decode a displayed raster.
//! Metadata comparison, not a content hash: rewrites preserving every compared field are not detected.
//! Windows compares length, modification and creation times; Unix also compares device, inode and ctime.
use std::{fs::Metadata, path::Path};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct FileRevision {
    len: u64,
    modified: std::time::SystemTime,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
    #[cfg(windows)]
    created: u64,
}

impl FileRevision {
    /// The path a revision was read from is not part of it: the callers check the path they hold,
    /// so a changed working directory never looks like a changed file.
    pub(super) fn capture(metadata: &Metadata) -> std::io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        #[cfg(windows)]
        use std::os::windows::fs::MetadataExt;
        Ok(Self {
            len: metadata.len(),
            modified: metadata.modified()?,
            #[cfg(unix)]
            identity: (metadata.dev(), metadata.ino(), metadata.ctime(), metadata.ctime_nsec()),
            #[cfg(windows)]
            created: metadata.creation_time(),
        })
    }

    fn check_metadata(&self, metadata: &Metadata) -> std::result::Result<(), String> {
        let current = Self::capture(metadata).map_err(|e| format!("Could not verify the current image file metadata: {e}"))?;
        if *self == current {
            Ok(())
        } else {
            Err("The image file changed since its pixels were loaded. Use Reload Data, then retry the operation.".into())
        }
    }
}

impl super::Raster {
    fn original_revision(&self) -> std::result::Result<&FileRevision, String> {
        match &self.source_file {
            Some(Ok(original)) => Ok(original),
            Some(Err(error)) => Err(format!(
                "The original image file revision could not be captured ({error}). Check filesystem metadata support/access, or export the image as a GeoTIFF to another location and add it."
            )),
            None => Err("The displayed image has no verified original file revision. Use Reload Data, then retry the operation.".into()),
        }
    }

    /// Checks fresh metadata at the live path on the reader's worker, never per frame.
    pub fn check_original_file(&self, path: &Path) -> std::result::Result<(), String> {
        let original = self.original_revision()?;
        let metadata = std::fs::metadata(path).map_err(|e| format!("Could not read the current image file metadata: {e}"))?;
        original.check_metadata(&metadata)
    }

    /// Checks fresh metadata from this open handle against the original source.
    /// This does not verify that the live path still names the handle; check_original_file does.
    pub fn check_original_handle(&self, file: &std::fs::File) -> std::result::Result<(), String> {
        let original = self.original_revision()?;
        let metadata = file.metadata().map_err(|e| format!("Could not read the open image file metadata: {e}"))?;
        original.check_metadata(&metadata)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn annotation_grid_revision_detects_replacement_with_the_same_length_and_modification_time() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("image.tif");
        let bytes = super::super::geotiff::write(&super::super::tests::small(), 0).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let raster = super::super::geotiff::read_file(&path, "image").unwrap();
        let original = std::fs::File::open(&path).unwrap();
        let metadata = original.metadata().unwrap();
        raster.check_original_file(&path).unwrap();
        raster.check_original_handle(&original).unwrap();
        let replacement = folder.path().join("replacement.tif");
        std::fs::write(&replacement, &bytes).unwrap();
        let file = std::fs::File::options().write(true).open(&replacement).unwrap();
        file.set_times(std::fs::FileTimes::new().set_modified(metadata.modified().unwrap())).unwrap();
        drop(file);
        std::fs::remove_file(&path).unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let current = std::fs::metadata(&path).unwrap();
        assert_eq!(metadata.len(), current.len());
        assert_eq!(metadata.modified().unwrap(), current.modified().unwrap());
        // Retaining old metadata cannot make the live-path API accept the replacement.
        assert!(raster.check_original_file(&path).unwrap_err().contains("Reload Data"));
        let replacement_handle = std::fs::File::open(&path).unwrap();
        assert!(raster.check_original_handle(&replacement_handle).is_err());
    }

    #[test]
    fn annotation_grid_handle_validation_rejects_a_different_file() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("image.tif");
        let other = folder.path().join("other.tif");
        let bytes = super::super::geotiff::write(&super::super::tests::small(), 0).unwrap();
        std::fs::write(&path, &bytes).unwrap();
        let mut other_bytes = bytes.clone();
        other_bytes.push(0); // Distinct metadata even on filesystems with coarse timestamps.
        std::fs::write(&other, &other_bytes).unwrap();
        let raster = super::super::geotiff::read_file(&path, "image").unwrap();
        assert!(raster.check_original_handle(&std::fs::File::open(&other).unwrap()).is_err());
        raster.check_original_file(&path).unwrap();
    }
}
