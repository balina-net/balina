//! `.balpck` — a project and its data sidecar in one zip, for sharing.
//!
//! Entries, in order: `mimetype` (stored, `application/x-balina-pack`, as
//! in ODF so the file is identifiable from its first bytes),
//! `project.balina` (the native JSON) and, when any tab has data,
//! `data.baltsv` (both deflated).

use std::io::{self, Read, Seek, Write};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::error::IoError;

pub const MIMETYPE: &str = "application/x-balina-pack";
pub const MIMETYPE_ENTRY: &str = "mimetype";
pub const PROJECT_ENTRY: &str = "project.balina";
pub const DATA_ENTRY: &str = "data.baltsv";

pub(crate) fn not_text() -> IoError {
    IoError::Malformed(".balpck is a binary bundle; open it with the pack reader".into())
}

pub struct PackReader<R: Read + Seek> {
    zip: ZipArchive<R>,
}

impl<R: Read + Seek> PackReader<R> {
    pub fn open(r: R) -> Result<Self, IoError> {
        let mut zip = ZipArchive::new(r)?;
        let mut tag = String::new();
        if let Ok(mut f) = zip.by_name(MIMETYPE_ENTRY) {
            f.read_to_string(&mut tag)?;
        }
        if tag.trim() != MIMETYPE || zip.index_for_name(PROJECT_ENTRY).is_none() {
            return Err(IoError::Malformed("not a Balina pack (.balpck)".into()));
        }
        Ok(PackReader { zip })
    }

    pub fn project_json(&mut self) -> Result<String, IoError> {
        let mut text = String::new();
        self.zip.by_name(PROJECT_ENTRY)?.read_to_string(&mut text)?;
        Ok(text)
    }

    /// Uncompressed size of the data sidecar, if the pack has one.
    pub fn data_len(&mut self) -> Option<u64> {
        self.zip.by_name(DATA_ENTRY).ok().map(|f| f.size())
    }

    /// Stream the decompressed data sidecar into `out`; returns its length.
    pub fn extract_data<W: Write + ?Sized>(&mut self, out: &mut W) -> Result<u64, IoError> {
        Ok(io::copy(&mut self.zip.by_name(DATA_ENTRY)?, out)?)
    }
}

pub struct PackWriter<W: Write + Seek> {
    zip: ZipWriter<W>,
}

impl<W: Write + Seek> PackWriter<W> {
    pub fn new(w: W, project_json: &str) -> Result<Self, IoError> {
        let mut zip = ZipWriter::new(w);
        zip.start_file(
            MIMETYPE_ENTRY,
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
        )?;
        zip.write_all(MIMETYPE.as_bytes())?;
        zip.start_file(PROJECT_ENTRY, deflated(project_json.len() as u64))?;
        zip.write_all(project_json.as_bytes())?;
        Ok(PackWriter { zip })
    }

    /// Start the data entry; write the sidecar into the returned writer.
    /// `len` is its (expected) uncompressed size, which decides zip64.
    pub fn data_writer(&mut self, len: u64) -> Result<&mut impl Write, IoError> {
        self.zip.start_file(DATA_ENTRY, deflated(len))?;
        Ok(&mut self.zip)
    }

    /// Copy another pack's data entry without recompressing it.
    pub fn copy_data_from<R: Read + Seek>(&mut self, src: &mut PackReader<R>) -> Result<(), IoError> {
        let idx = src
            .zip
            .index_for_name(DATA_ENTRY)
            .ok_or_else(|| IoError::Malformed("pack has no data entry".into()))?;
        self.zip.raw_copy_file(src.zip.by_index_raw(idx)?)?;
        Ok(())
    }

    pub fn finish(self) -> Result<W, IoError> {
        Ok(self.zip.finish()?)
    }
}

fn deflated(len: u64) -> SimpleFileOptions {
    SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .large_file(len >= u32::MAX as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn pack(data: Option<&[u8]>) -> Vec<u8> {
        let mut w = PackWriter::new(Cursor::new(Vec::new()), "{\"format\":\"x\"}").unwrap();
        if let Some(d) = data {
            w.data_writer(d.len() as u64).unwrap().write_all(d).unwrap();
        }
        w.finish().unwrap().into_inner()
    }

    #[test]
    fn roundtrip_with_and_without_data() {
        let data = b"#BALTSV\t1\n".repeat(1000);
        let bytes = pack(Some(&data));
        assert!(bytes.len() < data.len() / 4, "data is compressed");
        assert_eq!(&bytes[30..38], b"mimetype");
        let mut r = PackReader::open(Cursor::new(&bytes)).unwrap();
        assert_eq!(r.project_json().unwrap(), "{\"format\":\"x\"}");
        assert_eq!(r.data_len(), Some(data.len() as u64));
        let mut out = Vec::new();
        assert_eq!(r.extract_data(&mut out).unwrap(), data.len() as u64);
        assert_eq!(out, data);

        let mut bare = PackReader::open(Cursor::new(pack(None))).unwrap();
        assert_eq!(bare.data_len(), None);
    }

    #[test]
    fn raw_copy_keeps_data() {
        let data = b"abc\tdef\n".repeat(500);
        let first = pack(Some(&data));
        let mut src = PackReader::open(Cursor::new(&first)).unwrap();
        let mut w = PackWriter::new(Cursor::new(Vec::new()), "{}").unwrap();
        w.copy_data_from(&mut src).unwrap();
        let second = w.finish().unwrap().into_inner();
        let mut r = PackReader::open(Cursor::new(&second)).unwrap();
        assert_eq!(r.project_json().unwrap(), "{}");
        let mut out = Vec::new();
        r.extract_data(&mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn rejects_other_zips() {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file("hello.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"hi").unwrap();
        let bytes = zip.finish().unwrap().into_inner();
        assert!(PackReader::open(Cursor::new(bytes)).is_err());
        assert!(PackReader::open(Cursor::new(b"not a zip".to_vec())).is_err());
    }
}
