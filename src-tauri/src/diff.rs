use crate::{
    store::{Comparison, NodeId, SideEntry},
    types::*,
};
use serde::{Deserialize, Serialize};

pub(crate) fn node_number(id: &str) -> Result<NodeId> {
    id.strip_prefix('n')
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse::<NodeId>().ok())
        .filter(|v| *v > 0)
        .ok_or_else(|| ApiError::new("INVALID_NODE", "无效节点 ID"))
}
fn checked_node(comparison: &Comparison, id: &str) -> Result<NodeId> {
    let node = node_number(id)?;
    if comparison.valid_node(node) {
        Ok(node)
    } else {
        Err(ApiError::new("INVALID_NODE", "节点不存在或已过期"))
    }
}
fn parent_number(comparison: &Comparison, parent: Option<&str>) -> Result<NodeId> {
    let Some(id) = parent else {
        return Ok(0);
    };
    let node = checked_node(comparison, id)?;
    if comparison.node(node).expandable {
        Ok(node)
    } else {
        Err(ApiError::new("INVALID_PARENT", "文件节点不能展开"))
    }
}
fn kind(entry: &SideEntry) -> String {
    match entry.kind {
        NodeKind::File => "file",
        NodeKind::Directory => "directory",
    }
    .into()
}
fn side(comparison: &Comparison, id: NodeId, index: usize) -> Option<SideValue> {
    comparison.entry(id, index).map(|entry| {
        let fields = comparison.entry_details(entry);
        SideValue {
            kind: kind(entry),
            size: entry.size.to_string(),
            allocated: entry.allocated.to_string(),
            modified: fields[0].map(str::to_owned),
            attributes: fields[1].map(str::to_owned),
        }
    })
}
fn deltas(comparison: &Comparison, id: NodeId) -> (String, String) {
    let before = comparison.entry(id, 0);
    let after = comparison.entry(id, 1);
    (
        (after.map_or(0, |e| e.size) as i128 - before.map_or(0, |e| e.size) as i128).to_string(),
        (after.map_or(0, |e| e.allocated) as i128 - before.map_or(0, |e| e.allocated) as i128)
            .to_string(),
    )
}
fn child(comparison: &Comparison, id: NodeId) -> ChildRow {
    let node = comparison.node(id);
    let (size_delta, allocated_delta) = deltas(comparison, id);
    ChildRow {
        node_id: format!("n{id}"),
        name: comparison.name(id).into(),
        expandable: node.expandable,
        status: node.status.clone(),
        has_changes: node.has_changes,
        changed_descendant_count: node.changed_descendant_count,
        child_count: comparison.children(id).len() as u64,
        before: side(comparison, id, 0),
        after: side(comparison, id, 1),
        size_delta,
        allocated_delta,
    }
}
fn decode<T: serde::de::DeserializeOwned>(text: &str) -> Result<T> {
    if text.len() > 32768 {
        return Err(ApiError::new("INVALID_CURSOR", "分页游标过长"));
    }
    serde_json::from_str(text).map_err(|_| ApiError::new("INVALID_CURSOR", "无效分页游标"))
}
fn encode<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value).map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))
}
fn invalid_cursor() -> ApiError {
    ApiError::new("INVALID_CURSOR", "分页游标范围或排序键无效")
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RootCursor {
    comparison: String,
    side: SnapshotSide,
    path: String,
    node: NodeId,
}

pub fn list_roots(
    comparison: &Comparison,
    comparison_id: &str,
    side: SnapshotSide,
    cursor: Option<&str>,
) -> Result<RootPage> {
    comparison.check_id(comparison_id)?;
    let index = match side {
        SnapshotSide::Before => 0,
        SnapshotSide::After => 1,
    };
    let roots = &comparison.roots[index];
    let start = if let Some(text) = cursor {
        let c: RootCursor = decode(text)?;
        if c.comparison != comparison_id || c.side != side || !comparison.valid_node(c.node) {
            return Err(invalid_cursor());
        }
        let position = roots
            .binary_search_by(|id| {
                comparison
                    .canonical_path(*id)
                    .cmp(&c.path)
                    .then(id.cmp(&c.node))
            })
            .map_err(|_| invalid_cursor())?;
        position + 1
    } else {
        0
    };
    let end = (start + 200).min(roots.len());
    let rows = roots[start..end]
        .iter()
        .map(|&id| {
            let entry = comparison
                .entry(id, index)
                .expect("root belongs to its snapshot");
            Root {
                node_id: format!("n{id}"),
                path: comparison.path(id, index),
                kind: kind(entry),
                size: entry.size.to_string(),
                allocated: entry.allocated.to_string(),
            }
        })
        .collect();
    let next_cursor = if end < roots.len() {
        let node = roots[end - 1];
        Some(encode(&RootCursor {
            comparison: comparison_id.into(),
            side,
            path: comparison.canonical_path(node),
            node,
        })?)
    } else {
        None
    };
    Ok(RootPage {
        rows,
        next_cursor,
        total_roots: roots.len() as u64,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    comparison: String,
    parent: Option<String>,
    changes: bool,
    directory: bool,
    rank: u64,
    node: NodeId,
}

pub fn list_children(
    comparison: &Comparison,
    comparison_id: &str,
    parent: Option<&str>,
    changes_only: bool,
    cursor: Option<&str>,
) -> Result<ChildPage> {
    comparison.check_id(comparison_id)?;
    let parent_id = parent_number(comparison, parent)?;
    let children = if changes_only {
        comparison.changed_children(parent_id)
    } else {
        comparison.children(parent_id)
    };
    let start = if let Some(text) = cursor {
        let c: Cursor = decode(text)?;
        if c.comparison != comparison_id
            || c.parent.as_deref() != parent
            || c.changes != changes_only
        {
            return Err(invalid_cursor());
        }
        let position = usize::try_from(c.rank)
            .ok()
            .and_then(|r| r.checked_sub(1))
            .ok_or_else(invalid_cursor)?;
        if children.get(position).copied() != Some(c.node)
            || comparison.node(c.node).expandable != c.directory
        {
            return Err(invalid_cursor());
        }
        position + 1
    } else {
        0
    };
    let end = (start + 200).min(children.len());
    let rows = children[start..end]
        .iter()
        .map(|&id| child(comparison, id))
        .collect();
    let next_cursor = if end < children.len() {
        let node = children[end - 1];
        Some(encode(&Cursor {
            comparison: comparison_id.into(),
            parent: parent.map(str::to_owned),
            changes: changes_only,
            directory: comparison.node(node).expandable,
            rank: end as u64,
            node,
        })?)
    } else {
        None
    };
    Ok(ChildPage {
        rows,
        next_cursor,
        total_children: children.len() as u64,
    })
}

pub fn get_details(comparison: &Comparison, node_id: &str) -> Result<NodeDetails> {
    let id = checked_node(comparison, node_id)?;
    let entry = |index| {
        comparison.entry(id, index).map(|e| {
        let [modified, attributes, files, folders, mft, parent_mft, accessed, created, direct_size,
            direct_allocated, drive_capacity, free_space, used_space, reserved_space] = comparison.entry_details(e).map(|v| v.map(str::to_owned));
        Entry {
            path: comparison.path(id, index), kind: kind(e), size: e.size.to_string(), allocated: e.allocated.to_string(),
            modified, attributes, files, folders, mft, parent_mft, accessed, created, direct_size, direct_allocated,
            drive_capacity, free_space, used_space, reserved_space, hardlink_count: e.hardlink_count,
        }
    })
    };
    let (size_delta, allocated_delta) = deltas(comparison, id);
    let parent = comparison.node(id).parent;
    Ok(NodeDetails {
        node_id: node_id.into(),
        parent_id: (parent != 0).then(|| format!("n{parent}")),
        before: entry(0),
        after: entry(1),
        size_delta,
        allocated_delta,
    })
}
