use crate::{diff, import::JobControl, store, types::*};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// The caller supplies the normalized lowercase basename, not a full path.
pub fn extension(name: &str) -> &str {
    if name.starts_with('.') {
        return "";
    }
    match name.rfind('.') {
        Some(i) if i > 0 && i + 1 < name.len() => &name[i..],
        _ => "",
    }
}
fn aggregate_error(error: rusqlite::Error) -> ApiError {
    if error.to_string().contains("typeof(") {
        ApiError::new("AGGREGATE_OVERFLOW", "扩展名聚合超出 i64 范围")
    } else {
        store::aggregate_error(error)
    }
}

pub fn materialize(conn: &Connection, control: &JobControl) -> Result<()> {
    control.check()?;
    *control
        .interrupt
        .lock()
        .map_err(|_| ApiError::new("LOCK_ERROR", "取消锁失效"))? =
        Some(conn.get_interrupt_handle());
    conn.execute_batch("BEGIN")?;
    let result = (|| {
        conn.execute_batch("CREATE INDEX IF NOT EXISTS nodes_depth ON nodes(depth);
            CREATE TABLE extension_stats(side INTEGER NOT NULL,node_id INTEGER NOT NULL,extension TEXT NOT NULL,
                size INTEGER NOT NULL CHECK(typeof(size)='integer' AND size>=0),
                allocated INTEGER NOT NULL CHECK(typeof(allocated)='integer' AND allocated>=0),
                files INTEGER NOT NULL CHECK(typeof(files)='integer' AND files>=0),PRIMARY KEY(side,node_id,extension)) WITHOUT ROWID;
            INSERT INTO extension_stats SELECT side,0,extension,sum(size),sum(allocated),count(*) FROM entries WHERE kind='file' GROUP BY side,extension;
            INSERT INTO extension_stats SELECT side,node_id,extension,size,allocated,1 FROM entries WHERE kind='file';
            INSERT INTO extension_stats SELECT e.side,n.parent_id,e.extension,sum(e.size),sum(e.allocated),count(*) FROM entries e JOIN nodes n ON n.id=e.node_id JOIN entries p ON p.node_id=n.parent_id AND p.side=e.side AND p.kind='directory' WHERE e.kind='file' GROUP BY e.side,n.parent_id,e.extension;") .map_err(aggregate_error)?;
        let depth: i64 =
            conn.query_row("SELECT coalesce(max(depth),0) FROM nodes", [], |r| r.get(0))?;
        let mut propagate = conn.prepare("INSERT INTO extension_stats SELECT s.side,n.parent_id,s.extension,sum(s.size),sum(s.allocated),sum(s.files) FROM nodes n INDEXED BY nodes_depth JOIN extension_stats s ON s.node_id=n.id JOIN entries e ON e.node_id=n.id AND e.side=s.side AND e.kind='directory' JOIN entries p ON p.node_id=n.parent_id AND p.side=s.side AND p.kind='directory' WHERE n.depth=?1 GROUP BY s.side,n.parent_id,s.extension ON CONFLICT(side,node_id,extension) DO UPDATE SET size=extension_stats.size+excluded.size,allocated=extension_stats.allocated+excluded.allocated,files=extension_stats.files+excluded.files")?;
        for level in (0..=depth).rev() {
            control.check()?;
            propagate.execute([level]).map_err(aggregate_error)?;
        }
        conn.execute_batch("CREATE TABLE extension_totals(side INTEGER NOT NULL,node_id INTEGER NOT NULL,
            extensions INTEGER NOT NULL,size INTEGER NOT NULL CHECK(typeof(size)='integer' AND size>=0),
            allocated INTEGER NOT NULL CHECK(typeof(allocated)='integer' AND allocated>=0),
            files INTEGER NOT NULL CHECK(typeof(files)='integer' AND files>=0),PRIMARY KEY(side,node_id)) WITHOUT ROWID;
            INSERT INTO extension_totals SELECT side,node_id,count(*),sum(size),sum(allocated),sum(files) FROM extension_stats GROUP BY side,node_id;
            CREATE INDEX extension_before_size ON extension_stats(side,node_id,size DESC,extension) WHERE side=0;
            CREATE INDEX extension_after_size ON extension_stats(side,node_id,size DESC,extension) WHERE side=1;
            CREATE INDEX extension_before_allocated ON extension_stats(side,node_id,allocated DESC,extension) WHERE side=0;
            CREATE INDEX extension_after_allocated ON extension_stats(side,node_id,allocated DESC,extension) WHERE side=1;
            CREATE INDEX extension_before_files ON extension_stats(side,node_id,files DESC,extension) WHERE side=0;
            CREATE INDEX extension_after_files ON extension_stats(side,node_id,files DESC,extension) WHERE side=1;") .map_err(aggregate_error)?;
        control.check()
    })();
    match result {
        Ok(()) => {
            conn.execute_batch("COMMIT")?;
            control.check()
        }
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            if control.check().is_err() {
                control.check()
            } else {
                Err(error)
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    comparison: String,
    parent: Option<String>,
    side: SnapshotSide,
    metric: Metric,
    weight: i64,
    extension: String,
}

pub fn list_extensions(
    conn: &Connection,
    comparison_id: &str,
    parent: Option<&str>,
    side: SnapshotSide,
    metric: Metric,
    cursor: Option<&str>,
) -> Result<ExtensionPage> {
    if store::get_summary(conn)?.comparison_id != comparison_id {
        return Err(ApiError::new("STALE_COMPARISON", "对比 ID 已过期"));
    }
    let node = match parent {
        None => 0,
        Some(id) => {
            let node = diff::node_number(id)?;
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1)",
                [node],
                |r| r.get(0),
            )?;
            if !exists {
                return Err(ApiError::new("INVALID_NODE", "节点不存在或已过期"));
            }
            node
        }
    };
    let side_number = match side {
        SnapshotSide::Before => 0,
        SnapshotSide::After => 1,
    };
    let column = match metric {
        Metric::Size => "size",
        Metric::Allocated => "allocated",
    };
    let cursor = cursor.map(|text| {
        if text.len() > 32768 { return Err(ApiError::new("INVALID_CURSOR", "分页游标过长")); }
        let c: Cursor = serde_json::from_str(text).map_err(|_| ApiError::new("INVALID_CURSOR", "无效分页游标"))?;
        let same_metric = matches!((c.metric, metric), (Metric::Size, Metric::Size) | (Metric::Allocated, Metric::Allocated));
        if c.comparison != comparison_id || c.parent.as_deref() != parent || c.side != side || !same_metric || c.weight < 0 {
            return Err(ApiError::new("INVALID_CURSOR", "分页游标范围不匹配"));
        }
        let valid: bool = conn.query_row(&format!("SELECT EXISTS(SELECT 1 FROM extension_stats WHERE side=?1 AND node_id=?2 AND extension=?3 AND {column}=?4)"), params![side_number,node,c.extension,c.weight], |r| r.get(0))?;
        if !valid { return Err(ApiError::new("INVALID_CURSOR", "分页游标排序键无效")); }
        Ok(c)
    }).transpose()?;
    let totals: Option<(i64,i64,i64,i64)> = conn.query_row("SELECT extensions,size,allocated,files FROM extension_totals WHERE side=?1 AND node_id=?2", params![side_number,node], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    let (count, size, allocated, files) = totals.unwrap_or((0, 0, 0, 0));
    let mut items = Vec::with_capacity(200);
    let mut last = None;
    let mut more = false;
    // Split the descending weight / ascending extension seek into two indexed ranges.
    for equal_weight in [true, false] {
        if cursor.is_none() && !equal_weight {
            break;
        }
        let range = if cursor.is_none() {
            String::new()
        } else if equal_weight {
            format!(" AND {column}=?3 AND extension>?4")
        } else {
            format!(" AND {column}<?3")
        };
        let sql = format!("SELECT extension,size,allocated,files,{column} FROM extension_stats WHERE side=?1 AND node_id=?2{range} ORDER BY {column} DESC,extension LIMIT {}",201-items.len());
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = match cursor.as_ref() {
            None => stmt.query(params![side_number, node])?,
            Some(c) if equal_weight => {
                stmt.query(params![side_number, node, c.weight, c.extension])?
            }
            Some(c) => stmt.query(params![side_number, node, c.weight])?,
        };
        while let Some(row) = rows.next()? {
            if items.len() == 200 {
                more = true;
                break;
            }
            let extension: String = row.get(0)?;
            last = Some(Cursor {
                comparison: comparison_id.into(),
                parent: parent.map(str::to_owned),
                side,
                metric,
                weight: row.get(4)?,
                extension: extension.clone(),
            });
            items.push(ExtensionItem {
                extension,
                size: row.get::<_, i64>(1)?.to_string(),
                allocated: row.get::<_, i64>(2)?.to_string(),
                files: row.get::<_, i64>(3)? as u64,
            });
        }
        if more {
            break;
        }
    }
    let next_cursor = if more {
        Some(
            serde_json::to_string(&last.unwrap())
                .map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))?,
        )
    } else {
        None
    };
    Ok(ExtensionPage {
        rows: items,
        next_cursor,
        total_extensions: count as u64,
        total: TypeValue {
            size: size.to_string(),
            allocated: allocated.to_string(),
            files: files as u64,
        },
        warnings: vec![
            "按实际文件路径汇总；不使用目录导出汇总，不按 MFT 去重；缺失父目录会中断目录范围汇总"
                .into(),
        ],
    })
}
