use crate::types::*;
use rusqlite::{Connection, InterruptHandle, OptionalExtension};
use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

#[derive(Default)]
pub struct JobControl {
    pub cancelled: AtomicBool,
    pub interrupt: Mutex<Option<InterruptHandle>>,
}
impl JobControl {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        if let Ok(handle) = self.interrupt.lock() {
            if let Some(handle) = handle.as_ref() {
                handle.interrupt();
            }
        }
    }
    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(ApiError::new("CANCELLED", "任务已取消"))
        } else {
            Ok(())
        }
    }
}
pub struct Progress {
    pub phase: String,
    pub bytes_read: u64,
    pub total_bytes: u64,
    pub rows: u64,
}

pub const COLUMNS: [&str; 17] = [
    "文件名称",
    "大小",
    "分配",
    "修改时间",
    "属性",
    "文件",
    "文件夹",
    "MFTRECNO",
    "MFTPARENTRECNO",
    "LASTACCESSDATE",
    "CREATEDDATE",
    "FOLDERSIZE",
    "FOLDERALLOCATED",
    "DRIVECAPACITY",
    "FREESPACE",
    "USEDSPACE",
    "RESERVEDSPACE",
];

/// Match exported Windows paths without consulting the filesystem.
pub fn normalize(path: &str) -> (String, Option<String>, String, i64, String) {
    let mut buffers = PathBuffers::default();
    let (key, parent, name, depth, kind) = buffers.normalize(path);
    (
        key.to_owned(),
        parent.map(str::to_owned),
        name.to_owned(),
        depth,
        kind.to_owned(),
    )
}

#[derive(Default)]
struct PathBuffers {
    raw: String,
    key: String,
    root_parent: String,
}
impl PathBuffers {
    fn normalize<'a>(
        &'a mut self,
        path: &'a str,
    ) -> (&'a str, Option<&'a str>, &'a str, i64, &'static str) {
        let raw = if path.contains('/') {
            self.raw.clear();
            for c in path.chars() {
                self.raw.push(if c == '/' { '\\' } else { c });
            }
            self.raw.as_str()
        } else {
            path
        };
        let kind = if raw.ends_with('\\') {
            "directory"
        } else {
            "file"
        };
        let trimmed = raw.trim_end_matches('\\');
        self.key.clear();
        if trimmed.is_ascii() {
            self.key.push_str(trimmed);
            self.key.make_ascii_lowercase();
        } else {
            // Preserve contextual Unicode lowercase mapping, including Greek sigma.
            self.key.push_str(&trimmed.to_lowercase());
        }
        let key = &mut self.key;
        let drive_root = key.len() == 2 && key.ends_with(':');
        let unc_root = key.starts_with("\\\\") && key[2..].split('\\').count() == 2;
        if drive_root || unc_root {
            key.push('\\');
        }
        let parent = if drive_root || unc_root {
            None
        } else {
            key.rfind('\\').map(|i| {
                let p = &key[..i];
                if (p.len() == 2 && p.ends_with(':'))
                    || (p.starts_with("\\\\") && p[2..].split('\\').count() == 2)
                {
                    self.root_parent.clear();
                    self.root_parent.push_str(p);
                    self.root_parent.push('\\');
                    self.root_parent.as_str()
                } else {
                    p
                }
            })
        };
        let name = if drive_root || unc_root {
            raw
        } else {
            trimmed.rsplit('\\').next().unwrap_or(trimmed)
        };
        let depth = key.trim_end_matches('\\').matches('\\').count() as i64;
        (key.as_str(), parent, name, depth, kind)
    }
}
pub fn volume(key: &str) -> &str {
    if key.starts_with("\\\\") {
        let mut separators = key.match_indices('\\').skip(2);
        separators.next();
        separators.next().map_or(key, |(i, _)| &key[..i])
    } else {
        key.split('\\').next().unwrap_or(key)
    }
}
fn field_error(path: &Path, record: u64, column: &str, message: impl Into<String>) -> ApiError {
    ApiError {
        code: "INVALID_FIELD".into(),
        message: message.into(),
        source: Some(path.display().to_string()),
        record: Some(record),
        column: Some(column.into()),
    }
}

pub fn load(
    conn: &Connection,
    path: &Path,
    side: i64,
    control: &JobControl,
    progress: &mut dyn FnMut(Progress),
) -> Result<SourceSummary> {
    let initial = std::fs::metadata(path).map_err(|e| source_error(path, e.into()))?;
    let file = File::open(path).map_err(|e| source_error(path, e.into()))?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .buffer_capacity(64 * 1024)
        .from_reader(StrictCsv::new(file));
    let mut record = csv::StringRecord::new();
    let mut positions = [None; 17];
    let mut header_width = None;
    let mut description = None;
    let (mut rows, mut files, mut folders) = (0u64, 0u64, 0u64);
    control.check()?;
    conn.execute_batch("BEGIN")?;
    let result = (|| {
        let mut node = conn.prepare("INSERT INTO nodes(path,parent_path,name,depth,basename_key,parent_id) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(path) DO NOTHING")?;
        let mut lookup = conn.prepare("SELECT id FROM nodes WHERE path=?1")?;
        let mut entry = conn.prepare("INSERT INTO entries(side,node_id,path,kind,size,allocated,details,files,folders,mft,volume,extension) VALUES(?1,?11,?2,?3,?4,?5,?6,?7,?8,?9,?10,?12)")?;
        let mut serialized = Vec::with_capacity(1024);
        let mut paths = PathBuffers::default();
        let mut categories = [[0i64; 3]; 8];
        let mut extensions = HashMap::<String, [i64; 3]>::with_capacity(4096);
        let mut parents = HashMap::<String, i64>::with_capacity(1024);
        loop {
            let present = reader.read_record(&mut record).map_err(|e| {
                let mut error = source_error(
                    path,
                    ApiError::new("CSV_ERROR", format!("{e}；请导出 UTF-8 CSV")),
                );
                error.record = Some(match e.kind() {
                    csv::ErrorKind::Io(io) => io
                        .get_ref()
                        .and_then(|e| e.downcast_ref::<LexicalError>())
                        .map_or(reader.position().record() + 1, |e| e.record),
                    _ => e
                        .position()
                        .map_or(reader.position().record() + 1, |p| p.record() + 1),
                });
                error.column = Some("CSV".into());
                error
            })?;
            if !present {
                break;
            }
            let rec = record
                .position()
                .map_or(reader.position().record(), |p| p.record() + 1);
            if header_width.is_none() {
                if record
                    .iter()
                    .any(|s| s.trim_start_matches('\u{feff}') == COLUMNS[0])
                {
                    for (i, column) in COLUMNS.iter().enumerate() {
                        let mut matches = record
                            .iter()
                            .enumerate()
                            .filter(|(_, s)| s.trim_start_matches('\u{feff}') == *column);
                        positions[i] = matches.next().map(|(p, _)| p);
                        if matches.next().is_some() {
                            return Err(field_error(path, rec, column, "重复表头"));
                        }
                    }
                    for i in 0..3 {
                        if positions[i].is_none() {
                            return Err(source_error(
                                path,
                                ApiError::new(
                                    "UNSUPPORTED_HEADER",
                                    format!(
                                        "缺少 {}；请按样本选项导出 UTF-8 WizTree CSV",
                                        COLUMNS[i]
                                    ),
                                ),
                            ));
                        }
                    }
                    header_width = Some(record.len());
                } else if description.is_none() {
                    description = Some(
                        record
                            .iter()
                            .collect::<Vec<_>>()
                            .join(",")
                            .trim_start_matches('\u{feff}')
                            .to_owned(),
                    );
                } else {
                    return Err(source_error(
                        path,
                        ApiError::new("UNSUPPORTED_HEADER", "请按样本选项导出 UTF-8 WizTree CSV"),
                    ));
                }
                continue;
            }
            if record.len() > header_width.unwrap() {
                return Err(field_error(path, rec, "CSV", "记录包含无表头列"));
            }
            let get = |i: usize| {
                positions[i]
                    .and_then(|p| record.get(p))
                    .filter(|v| !v.is_empty())
            };
            let number = |i: usize, required: bool| -> Result<Option<i64>> {
                match get(i) {
                    None if required => Err(field_error(path, rec, COLUMNS[i], "缺少必需值")),
                    None => Ok(None),
                    Some(v) if !v.bytes().all(|b| b.is_ascii_digit()) => {
                        Err(field_error(path, rec, COLUMNS[i], "必须是非负十进制整数"))
                    }
                    Some(v) => v
                        .parse()
                        .map(Some)
                        .map_err(|_| field_error(path, rec, COLUMNS[i], "数值超出 i64 范围")),
                }
            };
            let original = get(0).ok_or_else(|| field_error(path, rec, COLUMNS[0], "路径为空"))?;
            let (key, parent, name, depth, kind) = paths.normalize(original);
            if key.is_empty() {
                return Err(field_error(path, rec, COLUMNS[0], "路径为空"));
            }
            let size = number(1, true)?.unwrap();
            let allocated = number(2, true)?.unwrap();
            let mut values: [Option<&str>; 14] = [None; 14];
            let mut parsed = [None; 17];
            for i in 3..17 {
                let value = if i == 7 || i == 8 {
                    get(i)
                        .map(|v| {
                            if !v.bytes().all(|b| b.is_ascii_digit()) {
                                return Err(field_error(
                                    path,
                                    rec,
                                    COLUMNS[i],
                                    "必须是非负十进制整数",
                                ));
                            }
                            let normalized = v.trim_start_matches('0');
                            Ok(if normalized.is_empty() {
                                "0"
                            } else {
                                normalized
                            })
                        })
                        .transpose()?
                } else if [5, 6, 11, 12, 13, 14, 15, 16].contains(&i) {
                    parsed[i] = number(i, false)?;
                    get(i).map(|v| {
                        let normalized = v.trim_start_matches('0');
                        if normalized.is_empty() {
                            "0"
                        } else {
                            normalized
                        }
                    })
                } else {
                    get(i)
                };
                values[i - 3] = value;
            }
            serialized.clear();
            serde_json::to_writer(&mut serialized, &values)
                .map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))?;
            let details = std::str::from_utf8(&serialized)
                .map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))?;
            let basename_key = if parent.is_none() {
                key
            } else {
                key.rsplit('\\').next().unwrap_or(key)
            };
            let existing = if side == 0 { None } else { lookup.query_row([key], |r| r.get(0)).optional()? };
            let node_id = if let Some(id) = existing {
                id
            } else {
                let parent_id = match parent {
                    None => None,
                    Some(path) => match parents.get(path) {
                        Some(id) => Some(*id),
                        None => lookup.query_row([path], |r| r.get(0)).optional()?,
                    },
                };
                if node.execute((key, parent, name, depth, basename_key, parent_id))? != 0 {
                    conn.last_insert_rowid()
                } else { lookup.query_row([key], |r| r.get(0))? }
            };
            if kind == "directory" {
                if parents.len() == 1024 {
                    parents.clear();
                }
                parents.insert(key.to_owned(), node_id);
            }
            entry.execute((side, original, kind, size, allocated, details, parsed[5], parsed[6], values[4], volume(key), node_id, if kind == "file" { crate::file_extensions::extension(basename_key) } else { "" })).map_err(|e| {
                if matches!(e, rusqlite::Error::SqliteFailure(ref x, _) if x.code == rusqlite::ErrorCode::ConstraintViolation) {
                    field_error(path, rec, COLUMNS[0], "重复或大小写规范化冲突路径")
                } else { source_error(path, e.into()) }
            })?;
            rows += 1;
            if kind == "directory" {
                folders += 1;
            } else {
                files += 1;
                let classification = crate::file_categories::category(name);
                let index = crate::file_categories::CATEGORIES
                    .iter()
                    .position(|c| *c == classification)
                    .unwrap();
                let values = &mut categories[index];
                values[0] = crate::store::checked_add(values[0], size, "文件类别大小")?;
                values[1] = crate::store::checked_add(values[1], allocated, "文件类别分配")?;
                values[2] = crate::store::checked_add(values[2], 1, "文件类别数量")?;
                let extension = crate::file_extensions::extension(basename_key);
                if let Some(values) = extensions.get_mut(extension) {
                    values[0] = crate::store::checked_add(values[0], size, "扩展名大小")?;
                    values[1] = crate::store::checked_add(values[1], allocated, "扩展名分配")?;
                    values[2] = crate::store::checked_add(values[2], 1, "扩展名数量")?;
                } else {
                    if extensions.len() == 4096 {
                        crate::file_extensions::save_import(conn, side, &mut extensions)?;
                        extensions.clear();
                    }
                    extensions.insert(extension.to_owned(), [size, allocated, 1]);
                }
            }
            if rows % 1000 == 0 {
                control.check()?;
                progress(Progress {
                    phase: if side == 0 { "before" } else { "after" }.into(),
                    bytes_read: reader.position().byte(),
                    total_bytes: initial.len(),
                    rows,
                });
            }
            if rows % 10000 == 0 {
                crate::file_extensions::save_import(conn, side, &mut extensions)?;
                conn.execute_batch("COMMIT; BEGIN")?;
            }
        }
        control.check()?;
        if rows == 0 {
            return Err(source_error(
                path,
                ApiError::new("EMPTY_SNAPSHOT", "CSV 没有数据记录"),
            ));
        }
        let current = std::fs::metadata(path).map_err(|e| source_error(path, e.into()))?;
        if initial.len() != current.len() || initial.modified()? != current.modified()? {
            return Err(source_error(
                path,
                ApiError::new("SOURCE_CHANGED", "导入期间源文件发生变化"),
            ));
        }
        crate::file_extensions::save_import(conn, side, &mut extensions)?;
        crate::file_categories::save_import(conn, side, &categories)?;
        conn.execute_batch("COMMIT")?;
        progress(Progress {
            phase: if side == 0 { "before" } else { "after" }.into(),
            bytes_read: initial.len(),
            total_bytes: initial.len(),
            rows,
        });
        Ok(SourceSummary {
            path: path.display().to_string(),
            description,
            rows,
            files,
            folders,
            roots: Vec::new(),
            root_count: 0,
            roots_truncated: false,
            size: "0".into(),
            allocated: "0".into(),
        })
    })();
    if result.is_err() {
        let _ = conn.execute_batch("ROLLBACK");
    }
    result
}
fn source_error(path: &Path, mut error: ApiError) -> ApiError {
    error.source = Some(path.display().to_string());
    error
}
#[derive(Debug)]
struct LexicalError {
    record: u64,
}
impl std::fmt::Display for LexicalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CSV record {}: malformed quoting", self.record)
    }
}
impl std::error::Error for LexicalError {}
// csv accepts malformed quoting; enforce lexical structure without buffering records.
struct StrictCsv<R> {
    inner: R,
    state: u8,
    offset: u64,
    record: u64,
    cr: bool,
}
impl<R> StrictCsv<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            state: 0,
            offset: 0,
            record: 1,
            cr: false,
        }
    }
}
impl<R: Read> Read for StrictCsv<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let len = self.inner.read(bytes)?;
        let invalid = |record| io::Error::new(io::ErrorKind::InvalidData, LexicalError { record });
        if len == 0 && self.state == 2 {
            return Err(invalid(self.record));
        }
        let start = self.offset;
        self.offset += len as u64;
        let mut i = 0;
        while i < len {
            // Most path/name bytes are ordinary. Check eight at a time instead of
            // running the quote/record state machine for every byte.
            if start + i as u64 >= 3 && (self.state == 1 || self.state == 2) {
                let skipped = ordinary_prefix(&bytes[i..len], self.state == 2);
                if self.state == 1 && skipped != 0 {
                    self.cr = false;
                }
                i += skipped;
                if i == len {
                    break;
                }
            }
            let byte = bytes[i];
            i += 1;
            let offset = start + i as u64;
            if offset <= 3 && byte == [0xef, 0xbb, 0xbf][offset as usize - 1] {
                continue;
            }
            if self.state == 2 {
                if byte == b'"' {
                    self.state = 3;
                }
                continue;
            }
            if self.state == 3 && byte == b'"' {
                self.state = 2;
                continue;
            }
            if byte == b',' {
                self.state = 0;
                self.cr = false;
                continue;
            }
            if byte == b'\r' || byte == b'\n' {
                if byte == b'\r' || !self.cr {
                    self.record += 1;
                }
                self.cr = byte == b'\r';
                self.state = 0;
                continue;
            }
            self.cr = false;
            match (self.state, byte) {
                (0, b'"') => self.state = 2,
                (0, _) => self.state = 1,
                (1, b'"') | (3, _) => return Err(invalid(self.record)),
                _ => {}
            }
        }
        Ok(len)
    }
}

// The zero-byte test may also flag a neighbouring byte after a borrow, but never
// misses a match. Only skip blocks without a match; inspect the candidate block.
fn ordinary_prefix(bytes: &[u8], quoted: bool) -> usize {
    fn contains(word: u64, byte: u8) -> bool {
        let x = word ^ (u64::from(byte) * 0x0101_0101_0101_0101);
        (x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080) != 0
    }
    let mut i = 0;
    while i + 8 <= bytes.len() {
        let word = u64::from_ne_bytes(bytes[i..i + 8].try_into().unwrap());
        if contains(word, b'"')
            || (!quoted && (contains(word, b',') || contains(word, b'\r') || contains(word, b'\n')))
        {
            break;
        }
        i += 8;
    }
    while i < bytes.len() {
        let byte = bytes[i];
        if byte == b'"' || (!quoted && matches!(byte, b',' | b'\r' | b'\n')) {
            break;
        }
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Chunks<'a> {
        bytes: &'a [u8],
        limit: usize,
    }
    impl Read for Chunks<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let len = self.bytes.len().min(self.limit).min(out.len());
            out[..len].copy_from_slice(&self.bytes[..len]);
            self.bytes = &self.bytes[len..];
            Ok(len)
        }
    }

    #[test]
    fn strict_csv_preserves_quotes_crlf_and_bom_across_read_boundaries() {
        let valid = "\u{feff}说明\r\n路径,大小\r\n\"C:\\long ordinary path 中文,\"\"quoted\"\"\r\nname\",12\r\nC:\\plain,0";
        for limit in 1..=32 {
            let mut reader = StrictCsv::new(Chunks {
                bytes: valid.as_bytes(),
                limit,
            });
            let mut result = Vec::new();
            reader.read_to_end(&mut result).unwrap();
            assert_eq!(result, valid.as_bytes());
            assert_eq!(reader.record, 4);
            for (invalid, record) in [
                ("header\r\nplain\"bad,0", 2),
                ("header\r\n\"long ordinary path\"suffix,0", 2),
                ("header\r\n\"long ordinary path\"\"", 2),
                ("header\r\n\"multiline\r\nquoted\",0\r\n\"unclosed", 3),
            ] {
                let mut reader = StrictCsv::new(Chunks {
                    bytes: invalid.as_bytes(),
                    limit,
                });
                let error = reader.read_to_end(&mut Vec::new()).unwrap_err();
                assert_eq!(
                    error
                        .get_ref()
                        .unwrap()
                        .downcast_ref::<LexicalError>()
                        .unwrap()
                        .record,
                    record
                );
            }
        }
    }

    #[test]
    fn block_scanner_matches_scalar_for_every_byte_and_boundary() {
        for quoted in [false, true] {
            for byte in 0u8..=255 {
                for position in 0..40 {
                    let mut bytes = [b'x'; 40];
                    bytes[position] = byte;
                    let expected = bytes
                        .iter()
                        .position(|&b| b == b'"' || (!quoted && matches!(b, b',' | b'\r' | b'\n')))
                        .unwrap_or(bytes.len());
                    assert_eq!(ordinary_prefix(&bytes, quoted), expected);
                }
            }
        }
    }
}
