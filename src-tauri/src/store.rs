use crate::{
    diff,
    import::{self, JobControl, Progress},
    types::*,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

// Share exact same-node cold metadata without collapsing paths or MFT hardlinks.
// Kind and byte values remain canonical, precise, side-specific membership data.
pub const SCHEMA: &str = "
CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
CREATE TABLE path_prefixes(id INTEGER PRIMARY KEY,path TEXT NOT NULL UNIQUE);
CREATE TABLE node_records(id INTEGER PRIMARY KEY,prefix_id INTEGER NOT NULL,basename_key TEXT NOT NULL,name TEXT NOT NULL,depth INTEGER NOT NULL,parent_id INTEGER,parent_path TEXT,extension TEXT NOT NULL DEFAULT '',UNIQUE(prefix_id,basename_key));
CREATE VIEW nodes AS SELECT n.id,p.path||n.basename_key path,n.parent_path,n.parent_id,n.name,n.depth,n.basename_key FROM node_records n JOIN path_prefixes p ON p.id=n.prefix_id;
CREATE TABLE entry_values(id INTEGER PRIMARY KEY,prefix_id INTEGER NOT NULL,suffix TEXT NOT NULL,details TEXT NOT NULL,files INTEGER,folders INTEGER,mft TEXT,volume TEXT NOT NULL,extension TEXT NOT NULL DEFAULT '');
CREATE TABLE snapshot_entries(side INTEGER NOT NULL,node_id INTEGER NOT NULL,value_id INTEGER NOT NULL,kind INTEGER NOT NULL CHECK(kind IN(0,1)),size INTEGER NOT NULL,allocated INTEGER NOT NULL,PRIMARY KEY(side,node_id)) WITHOUT ROWID;
CREATE VIEW entries AS SELECT s.side,s.node_id,p.path||v.suffix path,CASE s.kind WHEN 0 THEN 'file' ELSE 'directory' END kind,s.size,s.allocated,v.details,v.files,v.folders,v.mft,v.volume,v.extension FROM snapshot_entries s JOIN entry_values v ON v.id=s.value_id JOIN path_prefixes p ON p.id=v.prefix_id;
CREATE TABLE file_category_stats(side INTEGER NOT NULL,category TEXT NOT NULL,size INTEGER NOT NULL,allocated INTEGER NOT NULL,files INTEGER NOT NULL,PRIMARY KEY(side,category)) WITHOUT ROWID;
CREATE TABLE extension_stats(side INTEGER NOT NULL,node_id INTEGER NOT NULL,extension TEXT NOT NULL,
size INTEGER NOT NULL CHECK(typeof(size)='integer' AND size>=0),allocated INTEGER NOT NULL CHECK(typeof(allocated)='integer' AND allocated>=0),
files INTEGER NOT NULL CHECK(typeof(files)='integer' AND files>=0),PRIMARY KEY(side,node_id,extension)) WITHOUT ROWID;";

pub fn open_reader(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.execute_batch("PRAGMA cache_size=-32768; PRAGMA temp_store=FILE; PRAGMA mmap_size=0;")?;
    let state: Option<String> = conn
        .query_row("SELECT value FROM metadata WHERE key='state'", [], |r| {
            r.get(0)
        })
        .optional()?;
    if state.as_deref() != Some("ready") {
        return Err(ApiError::new(
            "CACHE_INVALID",
            "对比索引尚未完成，请重新导入",
        ));
    }
    Ok(conn)
}
pub fn get_summary(conn: &Connection) -> Result<ComparisonSummary> {
    let ready: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM metadata WHERE key='state' AND value='ready')",
        [],
        |r| r.get(0),
    )?;
    if !ready {
        return Err(ApiError::new(
            "CACHE_INVALID",
            "对比索引尚未完成，请重新导入",
        ));
    }
    let summary: String =
        conn.query_row("SELECT value FROM metadata WHERE key='summary'", [], |r| {
            r.get(0)
        })?;
    serde_json::from_str(&summary).map_err(|e| ApiError::new("CACHE_INVALID", e.to_string()))
}
pub fn build_comparison(
    before: &Path,
    after: &Path,
    db_path: &Path,
    comparison_id: &str,
    control: &JobControl,
    progress: &mut dyn FnMut(Progress),
) -> Result<ComparisonSummary> {
    let result = (|| {
        control.check()?;
        let conn = Connection::open(db_path)?;
        *control
            .interrupt
            .lock()
            .map_err(|_| ApiError::new("LOCK_ERROR", "取消锁失效"))? =
            Some(conn.get_interrupt_handle());
        // Bulk writes belong only to this unpublished, disposable database.
        // Flush it before publishing; an interrupted build is discarded.
        conn.execute_batch("PRAGMA page_size=8192; PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA temp_store=FILE; PRAGMA temp.cache_size=-32768; PRAGMA mmap_size=536870912; PRAGMA cache_size=-262144;")?;
        conn.execute_batch(SCHEMA)?;
        let mut before_summary = import::load(&conn, before, 0, control, progress)?;
        let mut after_summary = import::load(&conn, after, 1, control, progress)?;
        progress(Progress {
            phase: "indexing".into(),
            bytes_read: 0,
            total_bytes: 0,
            rows: before_summary.rows + after_summary.rows,
        });
        control.check()?;
        let warnings = finish_import(&conn, &mut before_summary, &mut after_summary, control)?;
        progress(Progress {
            phase: "comparing".into(),
            bytes_read: 0,
            total_bytes: 0,
            rows: before_summary.rows + after_summary.rows,
        });
        crate::file_extensions::materialize(&conn, control)?;
        crate::global_treemap::materialize(&conn, control)?;
        let mut counts = Counts {
            files: StatusCounts::default(),
            folders: StatusCounts::default(),
        };
        let mut stmt = conn.prepare(
            "SELECT expandable,status,count(*) FROM comparison_nodes GROUP BY expandable,status",
        )?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let target = if row.get::<_, bool>(0)? {
                &mut counts.folders
            } else {
                &mut counts.files
            };
            let count = row.get::<_, i64>(2)? as u64;
            match row.get::<_, String>(1)?.as_str() {
                "added" => target.added = count,
                "removed" => target.removed = count,
                "modified" => target.modified = count,
                "typeChanged" => target.type_changed = count,
                _ => target.unchanged = count,
            }
        }
        let summary = ComparisonSummary {
            comparison_id: comparison_id.into(),
            before: before_summary,
            after: after_summary,
            statuses: counts,
            warnings,
        };
        conn.execute(
            "INSERT INTO metadata VALUES('summary',?1)",
            params![serde_json::to_string(&summary)
                .map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))?],
        )?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; INSERT INTO metadata VALUES('state','ready'); PRAGMA wal_checkpoint(TRUNCATE);")?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(db_path)?
            .sync_all()?;
        control.check()?;
        Ok(summary)
    })();
    if let Ok(mut handle) = control.interrupt.lock() {
        *handle = None;
    }
    if result.is_err() {
        let _ = std::fs::remove_file(db_path);
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db_path.display()));
        }
    }
    if control.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
        Err(ApiError::new("CANCELLED", "任务已取消"))
    } else {
        result
    }
}

/// Link actual records only; missing ancestors are never invented.
pub fn finish_import(
    conn: &Connection,
    before: &mut SourceSummary,
    after: &mut SourceSummary,
    control: &JobControl,
) -> Result<Vec<String>> {
    // Spool unresolved links to SQLite, not a Rust path list. Resolve each compact
    // key once before creating the hierarchy indexes; absent parents stay absent.
    let links = conn.unchecked_transaction()?;
    conn.execute_batch("CREATE TEMP TABLE unresolved_parents AS SELECT id,parent_path FROM node_records WHERE parent_path IS NOT NULL;")?;
    {
        let mut pending = conn.prepare("SELECT id,parent_path FROM unresolved_parents")?;
        let mut lookup = conn.prepare("SELECT n.id FROM path_prefixes p JOIN node_records n ON n.prefix_id=p.id WHERE p.path=?1 AND n.basename_key=?2")?;
        let mut save = conn.prepare("UPDATE node_records SET parent_id=?2,parent_path=NULL WHERE id=?1")?;
        let mut rows = pending.query([])?;
        while let Some(row) = rows.next()? {
            control.check()?;
            let (prefix, basename) = import::canonical_parts(root_text(row, 1)?);
            let parent: Option<i64> = lookup.query_row((prefix, basename), |r| r.get(0)).optional()?;
            save.execute((row.get::<_, i64>(0)?, parent))?;
        }
    }
    conn.execute_batch("DROP TABLE unresolved_parents; CREATE INDEX nodes_parent ON node_records(parent_id); CREATE INDEX nodes_depth ON node_records(depth); CREATE INDEX entry_values_mft ON entry_values(volume,mft) WHERE mft IS NOT NULL; CREATE INDEX snapshot_values ON snapshot_entries(value_id,side) WHERE kind=0;")?;
    links.commit()?;
    let mut bad_parent = conn.prepare("SELECT ep.path||e.suffix FROM (SELECT DISTINCT parent_id FROM node_records WHERE parent_id IS NOT NULL) parents
        CROSS JOIN snapshot_entries p ON p.side=?1 AND p.node_id=parents.parent_id
        CROSS JOIN node_records n INDEXED BY nodes_parent ON n.parent_id=p.node_id
        CROSS JOIN snapshot_entries child ON child.side=p.side AND child.node_id=n.id
        CROSS JOIN entry_values e ON e.id=child.value_id
        CROSS JOIN path_prefixes ep ON ep.id=e.prefix_id WHERE p.kind=0 LIMIT 1")?;
    for side in [0, 1] {
        if let Some(path) = bad_parent
            .query_row([side], |r| r.get::<_, String>(0))
            .optional()?
        {
            return Err(ApiError::new(
                "INVALID_HIERARCHY",
                format!("同侧父路径记录是文件：{path}"),
            ));
        }
    }
    let mut warnings = Vec::new();
    conn.execute_batch("CREATE TABLE export_roots(side INTEGER NOT NULL,node_id INTEGER NOT NULL,path TEXT NOT NULL,eligible INTEGER NOT NULL,PRIMARY KEY(side,node_id));")?;
    // Stream all roots in one transaction; cancellation/error rolls back on drop.
    // The completed build checkpoints WAL before publishing the ready manifest.
    let root_transaction = conn.unchecked_transaction()?;
    for (side, summary) in [(0, &mut *before), (1, &mut *after)] {
        let mut roots = conn.prepare("WITH missing(id) AS MATERIALIZED (
            SELECT parents.parent_id FROM (SELECT DISTINCT parent_id FROM node_records WHERE parent_id IS NOT NULL) parents
            LEFT JOIN snapshot_entries p ON p.side=?1 AND p.node_id=parents.parent_id WHERE p.node_id IS NULL)
            SELECT n.id,np.path||n.basename_key,e.path,e.kind,e.size,e.allocated
            FROM node_records n INDEXED BY nodes_parent CROSS JOIN path_prefixes np ON np.id=n.prefix_id CROSS JOIN entries e ON e.side=?1 AND e.node_id=n.id WHERE n.parent_id IS NULL
            UNION ALL SELECT n.id,np.path||n.basename_key,e.path,e.kind,e.size,e.allocated
            FROM missing m CROSS JOIN node_records n INDEXED BY nodes_parent ON n.parent_id=m.id
            CROSS JOIN path_prefixes np ON np.id=n.prefix_id CROSS JOIN entries e ON e.side=?1 AND e.node_id=n.id ORDER BY 2")?;
        let mut ancestor = conn.prepare("SELECT e.kind=0 FROM path_prefixes p JOIN node_records n ON n.prefix_id=p.id JOIN snapshot_entries e ON e.node_id=n.id AND e.side=?1 WHERE p.path=?2 AND n.basename_key=?3")?;
        let mut save_root = conn
            .prepare("INSERT INTO export_roots(side,node_id,path,eligible) VALUES(?1,?2,?3,?4)")?;
        let mut rows = roots.query([side])?;
        let (mut size, mut allocated, mut missing, mut nested_missing) = (0i64, 0i64, 0u64, 0u64);
        let mut examples = Vec::new();
        while let Some(row) = rows.next()? {
            control.check()?;
            let kind = root_text(row, 3)?;
            let path = root_text(row, 2)?;
            if kind == "file" {
                missing += 1;
                if examples.len() < 5 {
                    examples.push(path.to_owned());
                }
            }
            let mut parent = canonical_parent(root_text(row, 1)?);
            let mut nested = false;
            while let Some(p) = parent {
                let (prefix, basename) = import::canonical_parts(p);
                if let Some(parent_is_file) = ancestor
                    .query_row(params![side, prefix, basename], |r| r.get::<_, bool>(0))
                    .optional()?
                {
                    if parent_is_file {
                        return Err(ApiError::new(
                            "INVALID_HIERARCHY",
                            format!("同侧祖先路径记录是文件：{path}"),
                        ));
                    }
                    nested = true;
                    break;
                }
                parent = canonical_parent(p);
            }
            save_root.execute(params![
                side,
                row.get::<_, i64>(0)?,
                root_text(row, 1)?,
                !nested
            ])?;
            summary.root_count += 1;
            let x: i64 = row.get(4)?;
            let y: i64 = row.get(5)?;
            if nested {
                nested_missing += 1;
            } else {
                size = checked_add(size, x, "根大小")?;
                allocated = checked_add(allocated, y, "根分配")?;
            }
            if summary.roots.len() < 200 {
                summary.roots.push(Root {
                    node_id: format!("n{}", row.get::<_, i64>(0)?),
                    path: path.to_owned(),
                    kind: kind.to_owned(),
                    size: x.to_string(),
                    allocated: y.to_string(),
                });
            }
        }
        summary.size = size.to_string();
        summary.allocated = allocated.to_string();
        summary.roots_truncated = summary.root_count > summary.roots.len() as u64;
        if nested_missing > 0 {
            warnings.push(format!("{}快照有 {nested_missing} 个嵌套导出根缺少中间父目录记录；导出范围可能不完整，工作区汇总不重复累加嵌套根", if side == 0 { "之前" } else { "之后" }));
        }
        if missing > 0 {
            warnings.push(format!(
                "{}快照有 {missing} 个文件缺少父目录记录，导出范围可能不完整；示例：{}",
                if side == 0 { "之前" } else { "之后" },
                examples.join("、")
            ));
        }
    }
    root_transaction.commit()?;
    control.check()?;
    conn.execute_batch("CREATE INDEX export_roots_page ON export_roots(side,path,node_id);")?;
    let different_roots: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM export_roots r WHERE NOT EXISTS(SELECT 1 FROM export_roots other WHERE other.side=1-r.side AND other.node_id=r.node_id))",
        [], |r| r.get(0),
    )?;
    if different_roots {
        warnings.push("导出根范围不同；差异仅表示 CSV 记录差异，不断言磁盘删除".into());
    }
    diff::materialize(conn, control)?;
    // Reuse compact comparison rows instead of re-reading both detail tables.
    // Absent/file parents cannot carry that side's directory counts.
    conn.execute_batch("CREATE TABLE export_counts(
        node_id INTEGER PRIMARY KEY,parent_id INTEGER,depth INTEGER,b_dir INTEGER,a_dir INTEGER,
        b_files INTEGER,b_folders INTEGER,a_files INTEGER,a_folders INTEGER);
        WITH direct AS (SELECT parent_id id,sum(before_kind='file') b_files,sum(before_kind='directory') b_folders,
            sum(after_kind='file') a_files,sum(after_kind='directory') a_folders FROM comparison_nodes GROUP BY parent_id)
        INSERT INTO export_counts SELECT c.node_id,c.parent_id,n.depth,coalesce(c.before_kind='directory',0),coalesce(c.after_kind='directory',0),
            CASE WHEN c.before_kind='directory' THEN coalesce(d.b_files,0) ELSE 0 END,
            CASE WHEN c.before_kind='directory' THEN coalesce(d.b_folders,0) ELSE 0 END,
            CASE WHEN c.after_kind='directory' THEN coalesce(d.a_files,0) ELSE 0 END,
            CASE WHEN c.after_kind='directory' THEN coalesce(d.a_folders,0) ELSE 0 END
            FROM comparison_nodes c JOIN node_records n ON n.id=c.node_id LEFT JOIN direct d ON d.id=c.node_id WHERE c.expandable=1;
        CREATE INDEX export_counts_depth ON export_counts(depth);
        CREATE INDEX export_counts_parent ON export_counts(parent_id);")?;
    let depth: i64 = conn.query_row(
        "SELECT coalesce(max(depth),0) FROM export_counts",
        [],
        |r| r.get(0),
    )?;
    let mut propagate = conn.prepare("UPDATE export_counts INDEXED BY export_counts_depth SET (b_files,b_folders,a_files,a_folders)=(
        SELECT export_counts.b_files+export_counts.b_dir*coalesce(sum(c.b_files),0),export_counts.b_folders+export_counts.b_dir*coalesce(sum(c.b_folders),0),
            export_counts.a_files+export_counts.a_dir*coalesce(sum(c.a_files),0),export_counts.a_folders+export_counts.a_dir*coalesce(sum(c.a_folders),0)
        FROM export_counts c INDEXED BY export_counts_parent WHERE c.parent_id=export_counts.node_id) WHERE depth=?1")?;
    for d in (0..=depth).rev() {
        control.check()?;
        propagate.execute([d]).map_err(aggregate_error)?;
    }
    let mismatch: i64 = conn.query_row("SELECT (SELECT count(*) FROM export_counts c CROSS JOIN snapshot_entries s ON s.node_id=c.node_id AND s.side=0 JOIN entry_values e ON e.id=s.value_id
        WHERE c.b_dir AND ((e.files IS NOT NULL AND e.files!=c.b_files) OR (e.folders IS NOT NULL AND e.folders!=c.b_folders)))
        +(SELECT count(*) FROM export_counts c CROSS JOIN snapshot_entries s ON s.node_id=c.node_id AND s.side=1 JOIN entry_values e ON e.id=s.value_id
        WHERE c.a_dir AND ((e.files IS NOT NULL AND e.files!=c.a_files) OR (e.folders IS NOT NULL AND e.folders!=c.a_folders)))", [], |r| r.get(0))?;
    if mismatch > 0 {
        warnings.push(format!(
            "{mismatch} 条目录导出计数与实际后代不符；导出范围可能不一致/不完整"
        ));
    }
    conn.execute_batch("DROP TABLE export_counts")?;
    Ok(warnings)
}
pub(crate) fn root_text<'a>(row: &'a rusqlite::Row<'_>, index: usize) -> rusqlite::Result<&'a str> {
    row.get_ref(index)?.as_str().map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(e))
    })
}
// Borrow canonical ancestors using import::normalize's drive/UNC root boundaries.
fn canonical_parent(path: &str) -> Option<&str> {
    if path.ends_with('\\') {
        return None;
    }
    path.rfind('\\').map(|i| {
        let parent = &path[..i];
        if (parent.len() == 2 && parent.ends_with(':'))
            || (parent.starts_with("\\\\") && parent[2..].split('\\').count() == 2)
        {
            &path[..=i]
        } else {
            parent
        }
    })
}
pub(crate) fn checked_add(a: i64, b: i64, field: &str) -> Result<i64> {
    a.checked_add(b)
        .ok_or_else(|| ApiError::new("AGGREGATE_OVERFLOW", format!("{field} 超出 i64 范围")))
}
pub(crate) fn aggregate_error(error: rusqlite::Error) -> ApiError {
    if error.to_string().contains("integer overflow") {
        ApiError::new("AGGREGATE_OVERFLOW", "聚合超出 i64 范围")
    } else {
        error.into()
    }
}
