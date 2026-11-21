use crate::{
    store::{Comparison, NodeId, SideEntry},
    types::*,
};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

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
    comparison.entry(id, index).map(|entry| SideValue {
        kind: kind(entry),
        size: entry.size.to_string(),
        allocated: entry.allocated.to_string(),
        modified: comparison.entry_field(entry, 0).map(str::to_owned),
        attributes: comparison.entry_field(entry, 1).map(str::to_owned),
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
        changed_descendant_count: node.changed_descendant_count as u64,
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
    sort: ChildSort,
    directory: bool,
    rank: u64,
    node: NodeId,
}

fn default_cmp(comparison: &Comparison, a: NodeId, b: NodeId) -> Ordering {
    let (na, nb) = (comparison.node(a), comparison.node(b));
    nb.expandable
        .cmp(&na.expandable)
        .then_with(|| comparison.basename(a).cmp(comparison.basename(b)))
        .then(a.cmp(&b))
}

fn apply_direction(order: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Asc => order,
        SortDirection::Desc => order.reverse(),
    }
}

fn cmp_opt<T>(
    a: Option<T>,
    b: Option<T>,
    direction: SortDirection,
    cmp: impl FnOnce(T, T) -> Ordering,
) -> Ordering {
    match (a, b) {
        (Some(x), Some(y)) => apply_direction(cmp(x, y), direction),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

fn side_metric(comparison: &Comparison, id: NodeId, side: usize, metric: Metric) -> Option<u64> {
    comparison.entry(id, side).map(|entry| match metric {
        Metric::Size => entry.size,
        Metric::Allocated => entry.allocated,
    })
}

fn metric_delta(comparison: &Comparison, id: NodeId, metric: Metric) -> i128 {
    let after = side_metric(comparison, id, 1, metric).unwrap_or(0) as i128;
    let before = side_metric(comparison, id, 0, metric).unwrap_or(0) as i128;
    after - before
}

fn kind_key(comparison: &Comparison, id: NodeId) -> (u8, &str) {
    let node = comparison.node(id);
    if node.status == Status::TypeChanged {
        return if comparison
            .entry(id, 0)
            .is_some_and(|e| e.kind == NodeKind::Directory)
        {
            (0, "")
        } else {
            (1, "")
        };
    }
    let current = comparison
        .entry(id, 1)
        .or_else(|| comparison.entry(id, 0));
    if current.is_some_and(|e| e.kind == NodeKind::Directory) {
        return (2, "");
    }
    let base = comparison.basename(id);
    match base.rfind('.') {
        Some(i) if i + 1 < base.len() => (4, &base[i + 1..]),
        _ => (3, ""),
    }
}

fn parse_datetime(text: &str) -> Option<[u16; 6]> {
    let trimmed = text.trim();
    let (date, time) = match trimmed.split_once([' ', 'T']) {
        Some((d, t)) => (d, Some(t.trim())),
        None => (trimmed, None),
    };
    let sep = if date.contains('/') {
        '/'
    } else if date.contains('-') {
        '-'
    } else {
        return None;
    };
    let mut date_parts = date.split(sep);
    let parse_part = |part: &str, max_len: usize| -> Option<u16> {
        if part.is_empty()
            || part.len() > max_len
            || !part.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        part.parse().ok()
    };
    let year = parse_part(date_parts.next()?, 4)?;
    let month = parse_part(date_parts.next()?, 2)?;
    let day = parse_part(date_parts.next()?, 2)?;
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (hour, minute, second) = if let Some(clock) = time {
        let mut clock_parts = clock.split(':');
        let h = parse_part(clock_parts.next()?, 2)?;
        let m = parse_part(clock_parts.next()?, 2)?;
        let s = match clock_parts.next() {
            Some(sec) => parse_part(sec, 2)?,
            None => 0,
        };
        if clock_parts.next().is_some() || h > 23 || m > 59 || s > 59 {
            return None;
        }
        (h, m, s)
    } else {
        (0, 0, 0)
    };
    Some([year, month, day, hour, minute, second])
}

fn sorted_children(comparison: &Comparison, children: &[NodeId], sort: ChildSort) -> Vec<NodeId> {
    let mut ordered = children.to_vec();
    match sort.by {
        ChildSortBy::Name => {
            ordered.sort_unstable_by(|&a, &b| {
                let (na, nb) = (comparison.node(a), comparison.node(b));
                nb.expandable
                    .cmp(&na.expandable)
                    .then_with(|| {
                        apply_direction(
                            comparison.basename(a).cmp(comparison.basename(b)),
                            sort.direction,
                        )
                    })
                    .then(a.cmp(&b))
            });
        }
        ChildSortBy::Before | ChildSortBy::After => {
            let side = usize::from(sort.by == ChildSortBy::After);
            ordered.sort_unstable_by(|&a, &b| {
                cmp_opt(
                    side_metric(comparison, a, side, sort.metric),
                    side_metric(comparison, b, side, sort.metric),
                    sort.direction,
                    |x, y| x.cmp(&y),
                )
                .then_with(|| default_cmp(comparison, a, b))
            });
        }
        ChildSortBy::Delta => {
            ordered.sort_unstable_by(|&a, &b| {
                apply_direction(
                    metric_delta(comparison, a, sort.metric)
                        .cmp(&metric_delta(comparison, b, sort.metric)),
                    sort.direction,
                )
                .then_with(|| default_cmp(comparison, a, b))
            });
        }
        ChildSortBy::Kind => {
            ordered.sort_unstable_by(|&a, &b| {
                apply_direction(
                    kind_key(comparison, a).cmp(&kind_key(comparison, b)),
                    sort.direction,
                )
                .then_with(|| default_cmp(comparison, a, b))
            });
        }
        ChildSortBy::BeforeModified | ChildSortBy::AfterModified => {
            let side = usize::from(sort.by == ChildSortBy::AfterModified);
            let mut keyed: Vec<(Option<(Option<[u16; 6]>, &str)>, NodeId)> = children
                .iter()
                .map(|&id| {
                    let raw = comparison
                        .entry(id, side)
                        .and_then(|e| comparison.entry_field(e, 0));
                    (raw.map(|text| (parse_datetime(text), text)), id)
                })
                .collect();
            keyed.sort_unstable_by(|(ka, a), (kb, b)| {
                cmp_opt(*ka, *kb, sort.direction, |(da, ta), (db, tb)| match (da, db) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => ta.cmp(tb),
                })
                .then_with(|| default_cmp(comparison, *a, *b))
            });
            return keyed.into_iter().map(|(_, id)| id).collect();
        }
        ChildSortBy::Attributes => {
            let attr = |id: NodeId| {
                comparison
                    .entry(id, 1)
                    .and_then(|e| comparison.entry_field(e, 1))
                    .or_else(|| {
                        comparison
                            .entry(id, 0)
                            .and_then(|e| comparison.entry_field(e, 1))
                    })
            };
            ordered.sort_unstable_by(|&a, &b| {
                cmp_opt(attr(a), attr(b), sort.direction, |x, y| x.cmp(y))
                    .then_with(|| default_cmp(comparison, a, b))
            });
        }
    }
    ordered
}

pub fn list_children(
    comparison: &Comparison,
    comparison_id: &str,
    parent: Option<&str>,
    changes_only: bool,
    sort: ChildSort,
    cursor: Option<&str>,
) -> Result<ChildPage> {
    comparison.check_id(comparison_id)?;
    let parent_id = parent_number(comparison, parent)?;
    let base = if changes_only {
        comparison.changed_children(parent_id)
    } else {
        comparison.children(parent_id)
    };
    let owned;
    let children = if sort.by == ChildSortBy::Name && sort.direction == SortDirection::Asc {
        base
    } else {
        owned = sorted_children(comparison, base, sort);
        &owned
    };
    let start = if let Some(text) = cursor {
        let c: Cursor = decode(text)?;
        if c.comparison != comparison_id
            || c.parent.as_deref() != parent
            || c.changes != changes_only
            || c.sort != sort
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
            sort,
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
            drive_capacity, free_space, used_space, reserved_space, hardlink_count: e.hardlink_count as u64,
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
