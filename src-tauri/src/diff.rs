use crate::{
    import::JobControl,
    store::{self, aggregate_error},
    types::*,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};

pub fn materialize(conn: &Connection, control: &JobControl) -> Result<()> {
    control.check()?;
    // Rank SQLite's exact BINARY lexical order once. The temporary table holds
    // only integer pairs; display/sort text remains solely in node_records.
    conn.execute_batch("CREATE TEMP TABLE lexical_order(node_id INTEGER PRIMARY KEY,sort_rank INTEGER NOT NULL) WITHOUT ROWID;
        INSERT INTO lexical_order SELECT node_id,sort_rank FROM (SELECT id node_id,dense_rank() OVER(ORDER BY basename_key COLLATE BINARY) sort_rank FROM node_records) ORDER BY node_id;")?;
    control.check()?;
    conn.execute_batch("CREATE TABLE comparison_nodes(
        node_id INTEGER PRIMARY KEY,parent_id INTEGER NOT NULL,sort_rank INTEGER NOT NULL,
        expandable INTEGER NOT NULL,status TEXT NOT NULL,has_changes INTEGER NOT NULL,
        changed_descendant_count INTEGER NOT NULL DEFAULT 0,child_count INTEGER NOT NULL DEFAULT 0,
        before_kind TEXT,after_kind TEXT,before_size INTEGER NOT NULL,after_size INTEGER NOT NULL,
        before_allocated INTEGER NOT NULL,after_allocated INTEGER NOT NULL,
        size_delta INTEGER NOT NULL,allocated_delta INTEGER NOT NULL,extension TEXT NOT NULL);
        INSERT INTO comparison_nodes(node_id,parent_id,sort_rank,expandable,status,has_changes,before_kind,after_kind,before_size,after_size,before_allocated,after_allocated,size_delta,allocated_delta,extension)
        SELECT n.id,coalesce(n.parent_id,0),r.sort_rank,coalesce(b.kind=1,0) OR coalesce(a.kind=1,0),
        CASE WHEN b.node_id IS NULL THEN 'added' WHEN a.node_id IS NULL THEN 'removed' WHEN b.kind!=a.kind THEN 'typeChanged' WHEN b.size!=a.size OR b.allocated!=a.allocated THEN 'modified' ELSE 'unchanged' END,
        b.node_id IS NULL OR a.node_id IS NULL OR b.kind!=a.kind OR b.size!=a.size OR b.allocated!=a.allocated,
        CASE b.kind WHEN 0 THEN 'file' WHEN 1 THEN 'directory' END,CASE a.kind WHEN 0 THEN 'file' WHEN 1 THEN 'directory' END,
        coalesce(b.size,0),coalesce(a.size,0),coalesce(b.allocated,0),coalesce(a.allocated,0),
        coalesce(a.size,0)-coalesce(b.size,0),coalesce(a.allocated,0)-coalesce(b.allocated,0),CASE WHEN b.kind=0 OR a.kind=0 THEN n.extension ELSE '' END
        FROM node_records n NOT INDEXED CROSS JOIN lexical_order r ON r.node_id=n.id
        LEFT JOIN snapshot_entries b ON b.node_id=n.id AND b.side=0 LEFT JOIN snapshot_entries a ON a.node_id=n.id AND a.side=1;
        DROP TABLE lexical_order;
        CREATE INDEX comparison_parent ON comparison_nodes(parent_id);")?;
    let depth: i64 =
        conn.query_row("SELECT coalesce(max(depth),0) FROM node_records", [], |r| {
            r.get(0)
        })?;
    let mut propagate = conn.prepare("UPDATE comparison_nodes SET (changed_descendant_count,has_changes)=(SELECT coalesce(sum(c.changed_descendant_count+(c.status!='unchanged')),0),comparison_nodes.status!='unchanged' OR coalesce(max(c.has_changes),0) FROM comparison_nodes c INDEXED BY comparison_parent WHERE c.parent_id=comparison_nodes.node_id) WHERE node_id IN (SELECT n.id FROM node_records n INDEXED BY nodes_depth CROSS JOIN comparison_nodes c WHERE n.depth=?1 AND c.node_id=n.id AND c.expandable=1)")?;
    for d in (0..=depth).rev() {
        control.check()?;
        propagate.execute([d]).map_err(aggregate_error)?;
    }
    conn.execute_batch("CREATE TABLE comparison_aggregates(
        parent_id INTEGER PRIMARY KEY,child_count INTEGER NOT NULL,changed_children INTEGER NOT NULL,internal_changes INTEGER NOT NULL);
        INSERT INTO comparison_aggregates SELECT parent_id,count(*),sum(has_changes),sum(status='unchanged' AND has_changes AND expandable) FROM comparison_nodes GROUP BY parent_id;
        UPDATE comparison_nodes SET child_count=coalesce((SELECT a.child_count FROM comparison_aggregates a WHERE a.parent_id=comparison_nodes.node_id),0) WHERE expandable=1;
        CREATE INDEX tree_page ON comparison_nodes(parent_id,expandable DESC,sort_rank,node_id);
        CREATE INDEX tree_changes_page ON comparison_nodes(parent_id,expandable DESC,sort_rank,node_id) WHERE has_changes=1;
        DROP INDEX comparison_parent;
        ").map_err(aggregate_error)?;
    control.check()
}

pub(crate) fn node_number(id: &str) -> Result<i64> {
    let value = id
        .strip_prefix('n')
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0);
    value.ok_or_else(|| ApiError::new("INVALID_NODE", "无效节点 ID"))
}
fn parent_number(conn: &Connection, parent: Option<&str>) -> Result<i64> {
    let Some(id) = parent else {
        return Ok(0);
    };
    let number = node_number(id)?;
    let expandable: Option<bool> = conn
        .query_row(
            "SELECT expandable FROM comparison_nodes WHERE node_id=?1",
            [number],
            |r| r.get(0),
        )
        .optional()?;
    match expandable {
        Some(true) => Ok(number),
        Some(false) => Err(ApiError::new("INVALID_PARENT", "文件节点不能展开")),
        None => Err(ApiError::new("INVALID_NODE", "节点不存在或已过期")),
    }
}
fn status(text: String) -> rusqlite::Result<Status> {
    match text.as_str() {
        "added" => Ok(Status::Added),
        "removed" => Ok(Status::Removed),
        "modified" => Ok(Status::Modified),
        "unchanged" => Ok(Status::Unchanged),
        "typeChanged" => Ok(Status::TypeChanged),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}
fn side(
    row: &Row<'_>,
    kind: usize,
    size: usize,
    allocated: usize,
    modified: usize,
    attributes: usize,
) -> rusqlite::Result<Option<SideValue>> {
    let Some(kind) = row.get::<_, Option<String>>(kind)? else {
        return Ok(None);
    };
    Ok(Some(SideValue {
        kind,
        size: row.get::<_, i64>(size)?.to_string(),
        allocated: row.get::<_, i64>(allocated)?.to_string(),
        modified: row.get(modified)?,
        attributes: row.get(attributes)?,
    }))
}
fn child(row: &Row<'_>) -> rusqlite::Result<ChildRow> {
    Ok(ChildRow {
        node_id: format!("n{}", row.get::<_, i64>(0)?),
        name: row.get(1)?,
        expandable: row.get(2)?,
        status: status(row.get(3)?)?,
        has_changes: row.get(4)?,
        changed_descendant_count: row.get::<_, i64>(5)? as u64,
        child_count: row.get::<_, i64>(6)? as u64,
        before: side(row, 7, 9, 11, 16, 17)?,
        after: side(row, 8, 10, 12, 18, 19)?,
        size_delta: row.get::<_, i64>(13)?.to_string(),
        allocated_delta: row.get::<_, i64>(14)?.to_string(),
    })
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootCursor {
    comparison: String,
    side: SnapshotSide,
    path: String,
    node: i64,
}

pub fn list_roots(
    conn: &Connection,
    comparison_id: &str,
    side: SnapshotSide,
    cursor: Option<&str>,
) -> Result<RootPage> {
    let summary = store::get_summary(conn)?;
    if summary.comparison_id != comparison_id {
        return Err(ApiError::new("STALE_COMPARISON", "对比 ID 已过期"));
    }
    let (side_number, total_roots) = match side {
        SnapshotSide::Before => (0, summary.before.root_count),
        SnapshotSide::After => (1, summary.after.root_count),
    };
    let cursor = cursor
        .map(|text| {
            if text.len() > 32768 {
                return Err(ApiError::new("INVALID_CURSOR", "分页游标过长"));
            }
            let c: RootCursor = serde_json::from_str(text)
                .map_err(|_| ApiError::new("INVALID_CURSOR", "无效分页游标"))?;
            if c.comparison != comparison_id || c.side != side || c.node <= 0 {
                return Err(ApiError::new("INVALID_CURSOR", "分页游标范围不匹配"));
            }
            let valid: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM export_roots WHERE side=?1 AND node_id=?2 AND path=?3)",
            params![side_number, c.node, c.path], |r| r.get(0),
        )?;
            if !valid {
                return Err(ApiError::new("INVALID_CURSOR", "分页游标排序键无效"));
            }
            Ok(c)
        })
        .transpose()?;
    // Both ranges seek the disk index; the summary supplies the exact scalar count.
    let sql = if cursor.is_some() {
        "SELECT r.node_id,r.path,e.path,e.kind,e.size,e.allocated FROM export_roots r INDEXED BY export_roots_page CROSS JOIN entries e WHERE r.side=?1 AND (r.path,r.node_id)>(?2,?3) AND e.side=r.side AND e.node_id=r.node_id ORDER BY r.path,r.node_id LIMIT 201"
    } else {
        "SELECT r.node_id,r.path,e.path,e.kind,e.size,e.allocated FROM export_roots r INDEXED BY export_roots_page CROSS JOIN entries e WHERE r.side=?1 AND e.side=r.side AND e.node_id=r.node_id ORDER BY r.path,r.node_id LIMIT 201"
    };
    let mut stmt = conn.prepare(sql)?;
    let mut query = match cursor {
        Some(c) => stmt.query(params![side_number, c.path, c.node])?,
        None => stmt.query([side_number])?,
    };
    let mut rows = Vec::with_capacity(200);
    let mut last_path = String::new();
    let mut last_node = 0;
    let mut more = false;
    while let Some(row) = query.next()? {
        if rows.len() == 200 {
            more = true;
            break;
        }
        last_node = row.get(0)?;
        last_path.clear();
        last_path.push_str(store::root_text(row, 1)?);
        rows.push(Root {
            node_id: format!("n{last_node}"),
            path: row.get(2)?,
            kind: row.get(3)?,
            size: row.get::<_, i64>(4)?.to_string(),
            allocated: row.get::<_, i64>(5)?.to_string(),
        });
    }
    let next_cursor = if more {
        Some(
            serde_json::to_string(&RootCursor {
                comparison: comparison_id.into(),
                side,
                path: last_path,
                node: last_node,
            })
            .map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))?,
        )
    } else {
        None
    };
    Ok(RootPage {
        rows,
        next_cursor,
        total_roots,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    comparison: String,
    parent: Option<String>,
    changes: bool,
    directory: bool,
    rank: i64,
    node: i64,
}

pub fn list_children(
    conn: &Connection,
    comparison_id: &str,
    parent: Option<&str>,
    changes_only: bool,
    cursor: Option<&str>,
) -> Result<ChildPage> {
    // Read the small manifest, never the node set, to reject stale comparison scopes.
    if store::get_summary(conn)?.comparison_id != comparison_id {
        return Err(ApiError::new("STALE_COMPARISON", "对比 ID 已过期"));
    }
    let parent_id = parent_number(conn, parent)?;
    let cursor = cursor.map(|text| {
        if text.len() > 32768 { return Err(ApiError::new("INVALID_CURSOR", "分页游标过长")); }
        let c: Cursor = serde_json::from_str(text).map_err(|_| ApiError::new("INVALID_CURSOR", "无效分页游标"))?;
        if c.comparison != comparison_id || c.parent.as_deref() != parent || c.changes != changes_only || c.node <= 0 {
            return Err(ApiError::new("INVALID_CURSOR", "分页游标范围不匹配"));
        }
        let valid: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM comparison_nodes WHERE node_id=?1 AND parent_id=?2 AND expandable=?3 AND sort_rank=?4 AND (?5=0 OR has_changes=1))", params![c.node,parent_id,c.directory,c.rank,changes_only], |r| r.get(0))?;
        if !valid { return Err(ApiError::new("INVALID_CURSOR", "分页游标排序键无效")); }
        Ok(c)
    }).transpose()?;
    let total: Option<i64> = conn
        .query_row(
            if changes_only {
                "SELECT changed_children FROM comparison_aggregates WHERE parent_id=?1"
            } else {
                "SELECT child_count FROM comparison_aggregates WHERE parent_id=?1"
            },
            [parent_id],
            |r| r.get(0),
        )
        .optional()?;
    let filter = if changes_only {
        " AND has_changes=1"
    } else {
        ""
    };
    let index = if changes_only {
        "tree_changes_page"
    } else {
        "tree_page"
    };
    let mut rows = Vec::with_capacity(200);
    let mut last = None;
    let mut more = false;
    // Two indexed ranges preserve directory-first order without OFFSET or scanning old pages.
    for directory in [true, false] {
        if directory && cursor.as_ref().is_some_and(|c| !c.directory) {
            continue;
        }
        let seek = cursor.as_ref().filter(|c| c.directory == directory);
        let range = if seek.is_some() {
            " AND (c.sort_rank,c.node_id)>(?3,?4)"
        } else {
            ""
        };
        let sql = format!("SELECT c.node_id,n.name,c.expandable,c.status,c.has_changes,c.changed_descendant_count,c.child_count,c.before_kind,c.after_kind,c.before_size,c.after_size,c.before_allocated,c.after_allocated,c.size_delta,c.allocated_delta,c.sort_rank,json_extract(b.details,'$[0]'),json_extract(b.details,'$[1]'),json_extract(a.details,'$[0]'),json_extract(a.details,'$[1]') FROM comparison_nodes c INDEXED BY {index} JOIN node_records n ON n.id=c.node_id LEFT JOIN snapshot_entries bs ON bs.node_id=c.node_id AND bs.side=0 LEFT JOIN entry_values b ON b.id=bs.value_id LEFT JOIN snapshot_entries aas ON aas.node_id=c.node_id AND aas.side=1 LEFT JOIN entry_values a ON a.id=aas.value_id WHERE c.parent_id=?1 AND c.expandable=?2{filter}{range} ORDER BY c.sort_rank,c.node_id LIMIT {}",201-rows.len());
        let mut stmt = conn.prepare(&sql)?;
        let mut query = match seek {
            Some(c) => stmt.query(params![parent_id, directory, c.rank, c.node])?,
            None => stmt.query(params![parent_id, directory])?,
        };
        while let Some(row) = query.next()? {
            if rows.len() == 200 {
                more = true;
                break;
            }
            let value = child(row)?;
            last = Some(Cursor {
                comparison: comparison_id.into(),
                parent: parent.map(str::to_owned),
                changes: changes_only,
                directory: value.expandable,
                rank: row.get(15)?,
                node: row.get(0)?,
            });
            rows.push(value);
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
    Ok(ChildPage {
        rows,
        next_cursor,
        total_children: total.unwrap_or(0) as u64,
    })
}

pub fn get_details(conn: &Connection, node_id: &str) -> Result<NodeDetails> {
    let number = node_number(node_id)?;
    let delta: Option<(i64, i64, i64)> = conn
        .query_row(
            "SELECT size_delta,allocated_delta,parent_id FROM comparison_nodes WHERE node_id=?1",
            [number],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (size_delta, allocated_delta, parent_id) =
        delta.ok_or_else(|| ApiError::new("INVALID_NODE", "节点不存在或已过期"))?;
    let mut stmt = conn.prepare("SELECT path,kind,size,allocated,details,mft,volume FROM entries WHERE side=?1 AND node_id=?2")?;
    let mut entries = [None, None];
    for side in 0..2 {
        let raw: Option<(String, String, i64, i64, String, Option<String>, String)> = stmt
            .query_row(params![side, number], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })
            .optional()?;
        if let Some((path, kind, size, allocated, json, mft, volume)) = raw {
            let values: [Option<String>; 14] = serde_json::from_str(&json)
                .map_err(|e| ApiError::new("CACHE_INVALID", e.to_string()))?;
            let [modified, attributes, files, folders, _, parent_mft, accessed, created, direct_size, direct_allocated, drive_capacity, free_space, used_space, reserved_space] =
                values;
            let hardlink_count = if kind == "file" && mft.is_some() {
                conn.query_row("SELECT count(*) FROM entry_values v INDEXED BY entry_values_mft CROSS JOIN snapshot_entries s INDEXED BY snapshot_values ON s.value_id=v.id WHERE s.side=?1 AND s.kind=0 AND v.volume=?2 AND v.mft=?3 AND v.mft IS NOT NULL", params![side,volume,mft], |r| r.get::<_,i64>(0))? as u64
            } else {
                0
            };
            entries[side as usize] = Some(Entry {
                path,
                kind,
                size: size.to_string(),
                allocated: allocated.to_string(),
                modified,
                attributes,
                files,
                folders,
                mft,
                parent_mft,
                accessed,
                created,
                direct_size,
                direct_allocated,
                drive_capacity,
                free_space,
                used_space,
                reserved_space,
                hardlink_count,
            });
        }
    }
    let [before, after] = entries;
    Ok(NodeDetails {
        node_id: node_id.into(),
        parent_id: (parent_id != 0).then(|| format!("n{parent_id}")),
        before,
        after,
        size_delta: size_delta.to_string(),
        allocated_delta: allocated_delta.to_string(),
    })
}
