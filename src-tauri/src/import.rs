use crate::types::*;
use rusqlite::{params, Connection, InterruptHandle};
use std::{
    borrow::Cow,
    fs::File,
    io::{self, BufReader, Read},
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
    let raw = path.replace('/', "\\");
    let kind = if raw.ends_with('\\') {
        "directory"
    } else {
        "file"
    }
    .to_owned();
    let trimmed = raw.trim_end_matches('\\');
    let mut key = trimmed.to_lowercase();
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
                format!("{p}\\")
            } else {
                p.to_owned()
            }
        })
    };
    let name = if drive_root || unc_root {
        raw.clone()
    } else {
        trimmed.rsplit('\\').next().unwrap_or(trimmed).to_owned()
    };
    let depth = key.trim_end_matches('\\').matches('\\').count() as i64;
    (key, parent, name, depth, kind)
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
        .from_reader(StrictCsv::new(BufReader::with_capacity(1024 * 1024, file)));
    let mut record = csv::StringRecord::new();
    let mut positions = [None; 17];
    let mut header_width = None;
    let mut description = None;
    let (mut rows, mut files, mut folders) = (0u64, 0u64, 0u64);
    control.check()?;
    conn.execute_batch("BEGIN")?;
    let result = (|| {
        let mut node = conn.prepare("INSERT INTO nodes(path,parent_path,name,depth,basename_key) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(path) DO NOTHING")?;
        let mut entry = conn.prepare("INSERT INTO entries(side,node_id,path,kind,size,allocated,details,files,folders,mft,volume,extension) SELECT ?1,id,?2,?3,?4,?5,?6,?7,?8,?9,?10,?12 FROM nodes WHERE path=?11")?;
        let mut serialized = Vec::with_capacity(1024);
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
            let (key, parent, name, depth, kind) = normalize(original);
            if key.is_empty() {
                return Err(field_error(path, rec, COLUMNS[0], "路径为空"));
            }
            let size = number(1, true)?.unwrap();
            let allocated = number(2, true)?.unwrap();
            let mut values: [Option<Cow<'_, str>>; 14] = std::array::from_fn(|_| None);
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
                            Ok(Cow::Borrowed(if normalized.is_empty() {
                                "0"
                            } else {
                                normalized
                            }))
                        })
                        .transpose()?
                } else if [5, 6, 11, 12, 13, 14, 15, 16].contains(&i) {
                    parsed[i] = number(i, false)?;
                    parsed[i].map(|v| Cow::Owned(v.to_string()))
                } else {
                    get(i).map(Cow::Borrowed)
                };
                values[i - 3] = value;
            }
            serialized.clear();
            serde_json::to_writer(&mut serialized, &values)
                .map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))?;
            let details = std::str::from_utf8(&serialized)
                .map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))?;
            let basename_key = if parent.is_none() {
                key.as_str()
            } else {
                key.rsplit('\\').next().unwrap_or(&key)
            };
            node.execute(params![key, parent, name, depth, basename_key])?;
            entry.execute(params![side, original, kind, size, allocated, details, parsed[5], parsed[6], values[4].as_deref(), volume(&key), key, if kind == "file" { crate::file_extensions::extension(basename_key) } else { "" }]).map_err(|e| {
                if matches!(e, rusqlite::Error::SqliteFailure(ref x, _) if x.code == rusqlite::ErrorCode::ConstraintViolation) {
                    field_error(path, rec, COLUMNS[0], "重复或大小写规范化冲突路径")
                } else { source_error(path, e.into()) }
            })?;
            rows += 1;
            if kind == "directory" {
                folders += 1;
            } else {
                files += 1;
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
        for &byte in &bytes[..len] {
            self.offset += 1;
            if self.offset <= 3 && byte == [0xef, 0xbb, 0xbf][self.offset as usize - 1] {
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
