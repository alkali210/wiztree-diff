use crate::{import::JobControl, store, types::*};
use rusqlite::{params, Connection};

pub const CATEGORIES: [&str; 8] = [
    "code",
    "images",
    "video",
    "documents",
    "archives",
    "text",
    "audio",
    "other",
];

/// Original broad classification uses the actual basename, not extension stats.
pub fn category(name: &str) -> &'static str {
    if name.eq_ignore_ascii_case("README") || name.eq_ignore_ascii_case(".env") {
        return "text";
    }
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "other";
    };
    const GROUPS: [(&str, &[&str]); 7] = [
        (
            "code",
            &[
                "exe", "dll", "sys", "ocx", "pdb", "mui", "rs", "py", "js", "jsx", "ts", "tsx",
                "c", "h", "cpp", "hpp", "cs", "java", "go", "rb", "php", "swift", "kt", "scala",
                "vue", "svelte", "html", "css", "scss", "json", "xml", "yaml", "yml", "toml",
                "sql", "sh", "ps1",
            ],
        ),
        (
            "images",
            &[
                "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "ico", "tif", "tiff", "heic",
                "avif", "raw", "psd",
            ],
        ),
        (
            "video",
            &[
                "mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "mpeg", "mpg", "flv",
            ],
        ),
        (
            "documents",
            &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf",
                "epub",
            ],
        ),
        (
            "archives",
            &[
                "zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "cab", "iso", "tgz",
            ],
        ),
        (
            "text",
            &["txt", "md", "csv", "log", "ini", "cfg", "conf", "rst"],
        ),
        (
            "audio",
            &[
                "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus", "aiff",
            ],
        ),
    ];
    for (category, extensions) in GROUPS {
        if extensions.iter().any(|e| extension.eq_ignore_ascii_case(e)) {
            return category;
        }
    }
    "other"
}

/// One streaming import pass with sixteen fixed counters; UI reads only this table.
pub fn materialize(conn: &Connection, control: &JobControl) -> Result<()> {
    control.check()?;
    *control
        .interrupt
        .lock()
        .map_err(|_| ApiError::new("LOCK_ERROR", "取消锁失效"))? =
        Some(conn.get_interrupt_handle());
    let mut values = [[[0i64; 3]; 8]; 2];
    let mut stmt = conn.prepare("SELECT e.side,n.name,e.size,e.allocated FROM entries e JOIN nodes n ON n.id=e.node_id WHERE e.kind='file'")?;
    let mut rows = stmt.query([])?;
    let mut batch = 0;
    while let Some(row) = rows.next()? {
        if batch == 0 {
            control.check()?;
        }
        batch = (batch + 1) % 1024;
        let side: usize = row.get(0)?;
        let name = store::root_text(row, 1)?;
        let classification = category(name);
        let index = CATEGORIES
            .iter()
            .position(|c| *c == classification)
            .unwrap();
        let v = &mut values[side][index];
        v[0] = store::checked_add(v[0], row.get(2)?, "文件类别大小")?;
        v[1] = store::checked_add(v[1], row.get(3)?, "文件类别分配")?;
        v[2] = store::checked_add(v[2], 1, "文件类别数量")?;
    }
    drop(rows);
    drop(stmt);
    control.check()?;
    conn.execute_batch("CREATE TABLE file_category_stats(side INTEGER NOT NULL,category TEXT NOT NULL,size INTEGER NOT NULL,allocated INTEGER NOT NULL,files INTEGER NOT NULL,PRIMARY KEY(side,category)) WITHOUT ROWID;")?;
    let mut insert = conn.prepare("INSERT INTO file_category_stats VALUES(?1,?2,?3,?4,?5)")?;
    for (side, categories) in values.iter().enumerate() {
        for (index, v) in categories.iter().enumerate() {
            insert.execute(params![side as i64, CATEGORIES[index], v[0], v[1], v[2]])?;
        }
    }
    control.check()
}

fn value(v: [i64; 3]) -> TypeValue {
    TypeValue {
        size: v[0].to_string(),
        allocated: v[1].to_string(),
        files: v[2] as u64,
    }
}

pub fn get_file_categories(conn: &Connection) -> Result<FileCategoriesData> {
    let mut items: Vec<_> = CATEGORIES
        .iter()
        .map(|c| FileCategoryItem {
            category: (*c).into(),
            before: value([0; 3]),
            after: value([0; 3]),
        })
        .collect();
    let mut totals = [[0i64; 3]; 2];
    let mut stmt =
        conn.prepare("SELECT side,category,size,allocated,files FROM file_category_stats")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let side: usize = row.get(0)?;
        let category = store::root_text(row, 1)?;
        let index = CATEGORIES
            .iter()
            .position(|c| *c == category)
            .ok_or_else(|| ApiError::new("CACHE_INVALID", "无效文件类别"))?;
        let v = [row.get(2)?, row.get(3)?, row.get(4)?];
        for i in 0..3 {
            totals[side][i] = store::checked_add(totals[side][i], v[i], "文件类别汇总")?;
        }
        if side == 0 {
            items[index].before = value(v);
        } else {
            items[index].after = value(v);
        }
    }
    Ok(FileCategoriesData {
        items,
        before: value(totals[0]),
        after: value(totals[1]),
        warnings: vec![
            "按实际文件路径汇总；不使用目录导出汇总，不按 MFT 去重；包含缺失父目录的文件".into(),
        ],
    })
}
