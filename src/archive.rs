use crate::Result;
use std::collections::BTreeMap;
use std::io::{Cursor, Write};
use std::path::Path;
use zip::{DateTime, ZipWriter, write::SimpleFileOptions};

/// Write a deterministic archive: `first` leads, the remaining entries follow in path order,
/// and every entry uses the same fixed timestamp.
pub(crate) fn write_zip(
    dest: &Path,
    first: (&str, &[u8]),
    entries: BTreeMap<String, Vec<u8>>,
) -> Result<()> {
    let timestamp = DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0)
        .map_err(|_| crate::Error::from("invalid fixed archive timestamp"))?;
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(timestamp)
        .unix_permissions(0o644);
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    zip.start_file(first.0, options)?;
    zip.write_all(first.1)?;
    for (path, bytes) in entries {
        zip.start_file(path, options)?;
        zip.write_all(&bytes)?;
    }
    let bytes = zip.finish()?.into_inner();
    crate::write_atomic(dest, &bytes)
}
