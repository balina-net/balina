//! `.baltsv` — the case-data sidecar of a `.balina` project.
//!
//! UTF-8 text holding one TAB-separated section per tab. Lines starting
//! with `#` are structure; every other line is data (fields below are
//! separated by a single TAB, shown as spaces):
//!
//! ```text
//! #BALTSV  1
//! #project  3f9a1c07e2b4d815
//! #section  9c41e0d27a3b5f60  offset=0000000000000431  bytes=0000000000183920  rows=5000  cols=8  origin=learn-cpts  source=asia.csv  created=2026-11-23T10:04:00Z  name=Asia
//! #end-header
//! #====  9c41e0d27a3b5f60  Asia
//! VisitAsia  Smoking  …
//! yes  no  …
//! ```
//!
//! - `offset` (from the start of the file) points at the section's `#====`
//!   marker line; `bytes` spans marker through last row. Both are zero-padded
//!   to 16 digits so the header's size is known before anything is written.
//! - The first line after a marker holds the column names (not counted in
//!   `rows`). Section fields are `key=value`; unknown keys are ignored.
//! - Cells (and header values) escape `\t \n \r \\`; a line whose first
//!   cell starts with `#` writes it as `\#`.
//! - A hand-edited file whose offsets no longer land on their markers is
//!   still readable: [`open`] rebuilds the section table by scanning.
//!
//! A section *body* (column row + data rows, no marker) is the unit the
//! session keeps per tab; the marker is written at save time so tab renames
//! are reflected. Random row access goes through a sparse [`RowIndex`],
//! which is never persisted.

use std::borrow::Cow;
use std::io::{self, BufRead, Read, Seek, SeekFrom, Write};

use crate::error::{CaseError, IoError};
use crate::learn::cases::case_reader;

pub const MAGIC: &str = "#BALTSV";
pub const VERSION: u32 = 1;
/// Data rows between [`RowIndex`] entries.
pub const INDEX_STRIDE: u64 = 256;

const MARKER: &str = "#====";
const END_HEADER: &str = "#end-header";
/// Width of the zero-padded `offset=` / `bytes=` values.
const NUM_WIDTH: usize = 16;

/// Descriptive fields of a section (everything but its position).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SectionMeta {
    pub tab_uid: String,
    pub name: String,
    /// What produced the data: `learn-cpts`, `learn-structure`, `simulate`
    /// or `attach`.
    pub origin: String,
    /// Source file name, if the data came from a file.
    pub source: String,
    /// RFC 3339 UTC timestamp of when the data was attached.
    pub created: String,
}

/// A section located in a file: where its body starts and how long it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub meta: SectionMeta,
    pub body_offset: u64,
    pub body_len: u64,
    /// Data rows as declared by the header (or counted by a rescan).
    pub rows: u64,
    pub cols: usize,
}

/// A parsed `.baltsv` file.
#[derive(Clone, Debug)]
pub struct BaltsvFile {
    pub version: u32,
    pub project_uid: String,
    pub sections: Vec<Section>,
    /// The header did not match the body (hand-edited file); `sections`
    /// came from scanning for markers instead.
    pub rebuilt: bool,
}

/// Sparse row index of one section body: the body-relative byte offset of
/// every [`INDEX_STRIDE`]-th data row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RowIndex {
    pub offsets: Vec<u64>,
    pub rows: u64,
}

/// What [`convert_body`] / [`index_body`] learn about a body.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BodyStats {
    pub columns: Vec<String>,
    pub len: u64,
    pub index: RowIndex,
}

// ---------------------------------------------------------------------------
// Escaping
// ---------------------------------------------------------------------------

fn push_escaped(out: &mut String, cell: &str, line_start: bool) {
    if line_start && cell.starts_with('#') {
        out.push('\\');
    }
    for c in cell.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c => out.push(c),
        }
    }
}

pub fn escape(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    push_escaped(&mut out, cell, false);
    out
}

/// Inverse of the cell escaping; an unknown escape is kept verbatim.
pub fn unescape(cell: &str) -> Cow<'_, str> {
    if !cell.contains('\\') {
        return Cow::Borrowed(cell);
    }
    let mut out = String::with_capacity(cell.len());
    let mut chars = cell.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('#') => out.push('#'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    Cow::Owned(out)
}

/// One escaped, TAB-joined, LF-terminated line.
fn format_line<'a>(buf: &mut String, cells: impl IntoIterator<Item = &'a str>) {
    buf.clear();
    for (i, cell) in cells.into_iter().enumerate() {
        if i > 0 {
            buf.push('\t');
        }
        push_escaped(buf, cell, i == 0);
    }
    buf.push('\n');
}

/// Split a raw data line (terminator included or not) into unescaped cells.
pub fn split_line(line: &str) -> Vec<String> {
    let line = line.strip_suffix('\n').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    line.split('\t').map(|c| unescape(c).into_owned()).collect()
}

// ---------------------------------------------------------------------------
// Bodies
// ---------------------------------------------------------------------------

/// Convert a case file (CSV/TSV, read exactly as learning reads it — see
/// [`case_reader`]) into a section body. `progress` gets the fraction of
/// `src` consumed.
pub fn convert_body<W: Write>(
    src: &[u8],
    delimiter: Option<u8>,
    out: &mut W,
    progress: &mut dyn FnMut(f32),
) -> Result<BodyStats, CaseError> {
    let mut rdr = case_reader(src, delimiter);
    let headers = rdr.headers()?.clone();
    if headers.is_empty() || headers.iter().all(str::is_empty) {
        return Err(CaseError::NoHeader);
    }
    let mut buf = String::new();
    format_line(&mut buf, headers.iter());
    out.write_all(buf.as_bytes())?;
    let mut stats = BodyStats {
        columns: headers.iter().map(str::to_string).collect(),
        len: buf.len() as u64,
        index: RowIndex::default(),
    };
    let total = src.len().max(1) as f32;
    let mut rec = csv::StringRecord::new();
    while rdr.read_record(&mut rec)? {
        if stats.index.rows.is_multiple_of(INDEX_STRIDE) {
            stats.index.offsets.push(stats.len);
            progress(rdr.position().byte() as f32 / total);
        }
        format_line(&mut buf, rec.iter());
        out.write_all(buf.as_bytes())?;
        stats.len += buf.len() as u64;
        stats.index.rows += 1;
    }
    progress(1.0);
    Ok(stats)
}

/// Read one line including its terminator; `Ok(0)` at end of input.
fn read_line<R: BufRead>(r: &mut R, buf: &mut Vec<u8>) -> io::Result<usize> {
    buf.clear();
    r.read_until(b'\n', buf)
}

fn is_structure(line: &[u8]) -> bool {
    line.first() == Some(&b'#')
}

/// Build the column list and row index of a body by reading it once.
/// `body` must start at the body and should be limited to its length.
pub fn index_body<R: BufRead>(mut body: R) -> io::Result<BodyStats> {
    let mut line = Vec::new();
    let n = read_line(&mut body, &mut line)?;
    if n == 0 || is_structure(&line) {
        return Ok(BodyStats::default());
    }
    let mut stats = BodyStats {
        columns: split_line(&String::from_utf8_lossy(&line)),
        len: n as u64,
        index: RowIndex::default(),
    };
    loop {
        let n = read_line(&mut body, &mut line)?;
        if n == 0 || is_structure(&line) {
            break;
        }
        if stats.index.rows.is_multiple_of(INDEX_STRIDE) {
            stats.index.offsets.push(stats.len);
        }
        stats.len += n as u64;
        stats.index.rows += 1;
    }
    Ok(stats)
}

/// Read up to `count` data rows starting at row `start` of the body that
/// begins at `body_offset`.
pub fn read_rows<R: BufRead + Seek>(
    r: &mut R,
    body_offset: u64,
    index: &RowIndex,
    start: u64,
    count: usize,
) -> io::Result<Vec<Vec<String>>> {
    if start >= index.rows || count == 0 {
        return Ok(Vec::new());
    }
    let slot = (start / INDEX_STRIDE) as usize;
    let Some(&rel) = index.offsets.get(slot) else {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "row index out of range"));
    };
    r.seek(SeekFrom::Start(body_offset + rel))?;
    let mut line = Vec::new();
    for _ in 0..(start - slot as u64 * INDEX_STRIDE) {
        if read_line(r, &mut line)? == 0 {
            return Ok(Vec::new());
        }
    }
    let want = count.min((index.rows - start) as usize);
    let mut rows = Vec::with_capacity(want);
    while rows.len() < want {
        if read_line(r, &mut line)? == 0 || is_structure(&line) {
            break;
        }
        rows.push(split_line(&String::from_utf8_lossy(&line)));
    }
    Ok(rows)
}

/// Re-encode a body as plain TSV (csv-crate quoting) so the existing case
/// readers can learn from it with a forced tab delimiter.
pub fn body_to_tsv<R: BufRead>(mut body: R) -> Result<Vec<u8>, CaseError> {
    let mut wtr = csv::WriterBuilder::new().delimiter(b'\t').from_writer(Vec::new());
    let mut line = Vec::new();
    loop {
        let n = read_line(&mut body, &mut line)?;
        if n == 0 || is_structure(&line) {
            break;
        }
        wtr.write_record(split_line(&String::from_utf8_lossy(&line)))?;
    }
    wtr.into_inner().map_err(|e| CaseError::Io(e.into_error()))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// A body to write into a file, with what the header declares about it.
pub struct SectionSource<'a> {
    pub meta: SectionMeta,
    pub rows: u64,
    pub cols: usize,
    pub body_len: u64,
    pub body: &'a mut dyn Read,
}

fn marker_line(meta: &SectionMeta) -> String {
    format!("{MARKER}\t{}\t{}\n", escape(&meta.tab_uid), escape(&meta.name))
}

fn section_line(meta: &SectionMeta, offset: u64, bytes: u64, rows: u64, cols: usize) -> String {
    let mut line = format!(
        "#section\t{}\toffset={offset:0w$}\tbytes={bytes:0w$}\trows={rows}\tcols={cols}",
        escape(&meta.tab_uid),
        w = NUM_WIDTH
    );
    for (key, value) in [("origin", &meta.origin), ("source", &meta.source), ("created", &meta.created)] {
        if !value.is_empty() {
            line.push_str(&format!("\t{key}={}", escape(value)));
        }
    }
    line.push_str(&format!("\tname={}\n", escape(&meta.name)));
    line
}

/// Write a complete file and return where each section's body landed, in
/// input order.
pub fn write<W: Write>(
    out: &mut W,
    project_uid: &str,
    sections: &mut [SectionSource<'_>],
) -> Result<Vec<Section>, IoError> {
    let markers: Vec<String> = sections.iter().map(|s| marker_line(&s.meta)).collect();
    // Offsets are fixed-width, so a dry run with zeros sizes the header.
    let mut header_len = format!("{MAGIC}\t{VERSION}\n#project\t{}\n{END_HEADER}\n", escape(project_uid)).len();
    for s in sections.iter() {
        header_len += section_line(&s.meta, 0, 0, s.rows, s.cols).len();
    }
    let mut placed = Vec::with_capacity(sections.len());
    let mut header = format!("{MAGIC}\t{VERSION}\n#project\t{}\n", escape(project_uid));
    let mut offset = header_len as u64;
    for (s, marker) in sections.iter().zip(&markers) {
        let bytes = marker.len() as u64 + s.body_len;
        header.push_str(&section_line(&s.meta, offset, bytes, s.rows, s.cols));
        placed.push(Section {
            meta: s.meta.clone(),
            body_offset: offset + marker.len() as u64,
            body_len: s.body_len,
            rows: s.rows,
            cols: s.cols,
        });
        offset += bytes;
    }
    header.push_str(END_HEADER);
    header.push('\n');
    debug_assert_eq!(header.len(), header_len);
    out.write_all(header.as_bytes())?;
    for (s, marker) in sections.iter_mut().zip(&markers) {
        out.write_all(marker.as_bytes())?;
        let copied = io::copy(&mut (&mut s.body).take(s.body_len), out)?;
        if copied != s.body_len {
            return Err(IoError::Malformed(format!(
                "data for tab `{}` is truncated ({copied} of {} bytes)",
                s.meta.name, s.body_len
            )));
        }
    }
    out.flush()?;
    Ok(placed)
}

/// Header fields of one `#section` line, before verification.
struct Declared {
    meta: SectionMeta,
    offset: u64,
    bytes: u64,
    rows: u64,
    cols: usize,
}

fn parse_section_line(line: &str) -> Option<Declared> {
    let mut fields = line.split('\t');
    fields.next()?; // "#section"
    let mut d = Declared {
        meta: SectionMeta { tab_uid: unescape(fields.next()?).into_owned(), ..Default::default() },
        offset: u64::MAX,
        bytes: u64::MAX,
        rows: u64::MAX,
        cols: usize::MAX,
    };
    for field in fields {
        let Some((key, value)) = field.split_once('=') else { continue };
        match key {
            "offset" => d.offset = value.parse().ok()?,
            "bytes" => d.bytes = value.parse().ok()?,
            "rows" => d.rows = value.parse().ok()?,
            "cols" => d.cols = value.parse().ok()?,
            "origin" => d.meta.origin = unescape(value).into_owned(),
            "source" => d.meta.source = unescape(value).into_owned(),
            "created" => d.meta.created = unescape(value).into_owned(),
            "name" => d.meta.name = unescape(value).into_owned(),
            _ => {}
        }
    }
    (d.offset != u64::MAX && d.bytes != u64::MAX && d.rows != u64::MAX && d.cols != usize::MAX)
        .then_some(d)
}

fn line_str(line: &[u8]) -> Cow<'_, str> {
    let s = String::from_utf8_lossy(line);
    match s {
        Cow::Borrowed(b) => Cow::Borrowed(b.trim_end_matches(['\n', '\r'])),
        Cow::Owned(o) => Cow::Owned(o.trim_end_matches(['\n', '\r']).to_string()),
    }
}

/// Check the magic line and return the file's version.
fn read_magic<R: BufRead>(r: &mut R, line: &mut Vec<u8>) -> Result<u32, IoError> {
    read_line(r, line)?;
    let text = line_str(line);
    let version = text
        .strip_prefix(MAGIC)
        .and_then(|v| v.strip_prefix('\t'))
        .and_then(|v| v.trim().parse::<u32>().ok())
        .ok_or_else(|| IoError::Malformed("not a Balina data file (.baltsv)".into()))?;
    if version > VERSION {
        return Err(IoError::Malformed(format!(
            "data file version {version} is newer than this Balina supports ({VERSION})"
        )));
    }
    Ok(version)
}

/// Parse the header, then verify every section's marker; fall back to a
/// marker scan if anything does not line up.
pub fn open<R: BufRead + Seek>(r: &mut R) -> Result<BaltsvFile, IoError> {
    let file_len = r.seek(SeekFrom::End(0))?;
    r.seek(SeekFrom::Start(0))?;
    let mut line = Vec::new();
    let version = read_magic(r, &mut line)?;
    let mut project_uid = String::new();
    let mut declared = Vec::new();
    let (mut header_ok, mut malformed) = (false, false);
    loop {
        if read_line(r, &mut line)? == 0 || !is_structure(&line) {
            break;
        }
        let text = line_str(&line);
        if text == END_HEADER {
            header_ok = true;
            break;
        }
        if let Some(uid) = text.strip_prefix("#project\t") {
            project_uid = unescape(uid).into_owned();
        } else if text.starts_with("#section\t") {
            match parse_section_line(&text) {
                Some(d) => declared.push(d),
                None => malformed = true,
            }
        }
    }
    let header_end = r.stream_position()?;
    if header_ok
        && !malformed
        && let Some(sections) = verify(r, &declared, header_end, file_len)?
    {
        return Ok(BaltsvFile { version, project_uid, sections, rebuilt: false });
    }
    let sections = rescan(r, &declared)?;
    Ok(BaltsvFile { version, project_uid, sections, rebuilt: true })
}

/// Locate each declared body by reading its marker line. `None` unless
/// the sections tile the file exactly — each starting where the previous
/// one ended, the last ending at EOF — and every marker is in place; so a
/// hand edit that changed any section's length (even the last one's) is
/// caught instead of cutting rows short.
fn verify<R: BufRead + Seek>(
    r: &mut R,
    declared: &[Declared],
    header_end: u64,
    file_len: u64,
) -> io::Result<Option<Vec<Section>>> {
    let mut line = Vec::new();
    let mut sections = Vec::with_capacity(declared.len());
    let mut expected = header_end;
    for d in declared {
        if d.offset != expected || d.offset.checked_add(d.bytes).is_none_or(|end| end > file_len) {
            return Ok(None);
        }
        expected = d.offset + d.bytes;
        r.seek(SeekFrom::Start(d.offset))?;
        let n = read_line(r, &mut line)? as u64;
        let ok = match line_str(&line).strip_prefix(MARKER).and_then(|t| t.strip_prefix('\t')) {
            Some(rest) => unescape(rest.split('\t').next().unwrap_or("")) == d.meta.tab_uid,
            None => false,
        };
        if !ok || n > d.bytes {
            return Ok(None);
        }
        sections.push(Section {
            meta: d.meta.clone(),
            body_offset: d.offset + n,
            body_len: d.bytes - n,
            rows: d.rows,
            cols: d.cols,
        });
    }
    Ok((expected == file_len).then_some(sections))
}

/// Rebuild the section table from the `#====` markers, keeping whatever
/// metadata the header still offers for each uid.
fn rescan<R: BufRead + Seek>(r: &mut R, declared: &[Declared]) -> io::Result<Vec<Section>> {
    r.seek(SeekFrom::Start(0))?;
    let mut sections: Vec<Section> = Vec::new();
    let mut line = Vec::new();
    let mut pos = 0u64;
    let mut at_columns = false;
    loop {
        let n = read_line(r, &mut line)? as u64;
        if n == 0 {
            break;
        }
        pos += n;
        if is_structure(&line) {
            let text = line_str(&line);
            if let Some(rest) = text.strip_prefix(MARKER).and_then(|t| t.strip_prefix('\t')) {
                let mut parts = rest.split('\t');
                let uid = unescape(parts.next().unwrap_or("")).into_owned();
                let name = unescape(parts.next().unwrap_or("")).into_owned();
                let meta = declared
                    .iter()
                    .find(|d| d.meta.tab_uid == uid)
                    .map(|d| d.meta.clone())
                    .unwrap_or(SectionMeta { tab_uid: uid, name, ..Default::default() });
                sections.push(Section { meta, body_offset: pos, body_len: 0, rows: 0, cols: 0 });
                at_columns = true;
            }
            continue;
        }
        let Some(s) = sections.last_mut() else { continue };
        s.body_len = pos - s.body_offset;
        if at_columns {
            s.cols = split_line(&String::from_utf8_lossy(&line)).len();
            at_columns = false;
        } else {
            s.rows += 1;
        }
    }
    Ok(sections)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn body(csv: &str) -> (Vec<u8>, BodyStats) {
        let mut out = Vec::new();
        let stats = convert_body(csv.as_bytes(), None, &mut out, &mut |_| {}).unwrap();
        (out, stats)
    }

    fn meta(uid: &str, name: &str) -> SectionMeta {
        SectionMeta { tab_uid: uid.into(), name: name.into(), origin: "attach".into(), ..Default::default() }
    }

    fn write_file(bodies: &[(&SectionMeta, &[u8], &BodyStats)]) -> (Vec<u8>, Vec<Section>) {
        let mut readers: Vec<Cursor<&[u8]>> = bodies.iter().map(|(_, b, _)| Cursor::new(*b)).collect();
        let mut sources: Vec<SectionSource> = bodies
            .iter()
            .zip(readers.iter_mut())
            .map(|((m, _, st), r)| SectionSource {
                meta: (*m).clone(),
                rows: st.index.rows,
                cols: st.columns.len(),
                body_len: st.len,
                body: r,
            })
            .collect();
        let mut out = Vec::new();
        let placed = write(&mut out, "p1", &mut sources).unwrap();
        (out, placed)
    }

    #[test]
    fn escape_roundtrip() {
        for s in ["plain", "a\tb", "line\nbreak\r", "back\\slash", "\\t literal", "#hash", "ñandú ✓"] {
            assert_eq!(unescape(&escape(s)), s);
        }
        let mut line = String::new();
        format_line(&mut line, ["#x", "#y"]);
        assert_eq!(line, "\\#x\t#y\n");
        assert_eq!(split_line(&line), vec!["#x", "#y"]);
    }

    #[test]
    fn convert_reads_like_learning_and_escapes() {
        let (out, stats) = body("\u{feff}A;B\n 1 ;\"x\ty\"\n#2;*\n");
        assert_eq!(stats.columns, vec!["A", "B"]);
        assert_eq!(stats.index.rows, 2);
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "A\tB\n1\tx\\ty\n\\#2\t*\n");
        assert_eq!(stats.len, out.len() as u64);
        assert_eq!(index_body(Cursor::new(&out)).unwrap(), stats);
    }

    #[test]
    fn ragged_rows_are_rejected() {
        let mut out = Vec::new();
        assert!(convert_body(b"A,B\n1,2,3\n", None, &mut out, &mut |_| {}).is_err());
    }

    #[test]
    fn file_roundtrip_with_random_access() {
        let mut csv = String::from("n,sq\n");
        for i in 0..1000 {
            csv.push_str(&format!("{i},{}\n", i * i));
        }
        let (b1, s1) = body(&csv);
        let (b2, s2) = body("only\nx\n");
        let (m1, m2) = (meta("aaa", "First"), meta("bbb", "Sec\tond"));
        let (file, placed) = write_file(&[(&m1, &b1, &s1), (&m2, &b2, &s2)]);
        let text = String::from_utf8(file.clone()).unwrap();
        assert!(text.starts_with("#BALTSV\t1\n#project\tp1\n#section\taaa\toffset=0000000000000"));

        let parsed = open(&mut Cursor::new(&file)).unwrap();
        assert!(!parsed.rebuilt);
        assert_eq!(parsed.project_uid, "p1");
        assert_eq!(parsed.sections, placed);
        assert_eq!(parsed.sections[1].meta.name, "Sec\tond");
        assert_eq!(parsed.sections[0].rows, 1000);

        let sec = &parsed.sections[0];
        let idx = index_body(Cursor::new(&file[sec.body_offset as usize..][..sec.body_len as usize])).unwrap();
        assert_eq!(idx, s1);
        let mut cur = Cursor::new(&file);
        for start in [0u64, 1, 255, 256, 257, 511, 998] {
            let rows = read_rows(&mut cur, sec.body_offset, &idx.index, start, 3).unwrap();
            assert_eq!(rows[0], vec![start.to_string(), (start * start).to_string()]);
            assert_eq!(rows.len(), 3.min(1000 - start as usize));
        }
        assert!(read_rows(&mut cur, sec.body_offset, &idx.index, 1000, 3).unwrap().is_empty());
        let tsv = body_to_tsv(Cursor::new(&file[sec.body_offset as usize..])).unwrap();
        assert!(tsv.starts_with(b"n\tsq\n0\t0\n1\t1\n"));
        assert_eq!(tsv.iter().filter(|&&b| b == b'\n').count(), 1001);
    }

    #[test]
    fn hand_edited_file_is_rescanned() {
        let (b1, s1) = body("A\n1\n2\n");
        let (b2, s2) = body("B\n3\n");
        let (m1, m2) = (meta("aaa", "First"), meta("bbb", "Second"));
        let (file, _) = write_file(&[(&m1, &b1, &s1), (&m2, &b2, &s2)]);
        // Insert a row into the first section: the second offset is now off.
        let text = String::from_utf8(file).unwrap().replace("1\n2\n", "1\n2\n9\n");
        let parsed = open(&mut Cursor::new(text.as_bytes())).unwrap();
        assert!(parsed.rebuilt);
        assert_eq!(parsed.sections.len(), 2);
        assert_eq!(parsed.sections[0].rows, 3);
        assert_eq!(parsed.sections[0].meta.origin, "attach");
        let sec = &parsed.sections[1];
        let body = &text.as_bytes()[sec.body_offset as usize..][..sec.body_len as usize];
        assert_eq!(body, b"B\n3\n");
    }

    #[test]
    fn grown_last_section_is_rescanned_not_truncated() {
        let (b1, s1) = body("A\n1\n");
        let (b2, s2) = body("B\nyes\nno\n");
        let (m1, m2) = (meta("aaa", "First"), meta("bbb", "Second"));
        let (file, _) = write_file(&[(&m1, &b1, &s1), (&m2, &b2, &s2)]);
        // Lengthen a cell of the LAST section: every offset still lands.
        let text = String::from_utf8(file).unwrap().replace("B\nyes\n", "B\nmaybe\n");
        let parsed = open(&mut Cursor::new(text.as_bytes())).unwrap();
        assert!(parsed.rebuilt);
        let sec = &parsed.sections[1];
        let body = &text.as_bytes()[sec.body_offset as usize..][..sec.body_len as usize];
        assert_eq!(body, b"B\nmaybe\nno\n");
        assert_eq!(sec.rows, 2);
    }

    #[test]
    fn rejects_foreign_and_newer_files() {
        assert!(open(&mut Cursor::new(b"a,b\n1,2\n")).is_err());
        assert!(open(&mut Cursor::new(b"#BALTSV\t99\n#end-header\n")).is_err());
    }
}
