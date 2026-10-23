use crate::{
    diff,
    store::{self, Comparison, ExtensionStat, NodeId},
    types::*,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
};

/// The caller supplies the normalized lowercase basename, not a full path.
pub fn extension(name: &str) -> &str {
    if name.starts_with('.') {
        return "";
    }
    match name.rfind('.') {
        Some(i) if i + 1 < name.len() => &name[i..],
        _ => "",
    }
}

const CACHE_BYTES: usize = 16 * 1024 * 1024;
type ScopeKey = (usize, NodeId);
enum Statistics {
    Global,
    Directory(Vec<ExtensionStat>),
}
struct Scope {
    statistics: Arc<Statistics>,
    order: Vec<usize>,
    allocated: bool,
    total: [u64; 3],
    bytes: usize,
}
impl Scope {
    fn len(&self) -> usize {
        self.order.len()
    }

    fn get<'a>(
        &'a self,
        comparison: &'a Comparison,
        side: usize,
        index: usize,
    ) -> &'a ExtensionStat {
        match self.statistics.as_ref() {
            Statistics::Global => &comparison.extensions[side][self.order[index]],
            Statistics::Directory(stats) => &stats[self.order[index]],
        }
    }
}
/// FIFO byte-bounded memoization of requested immutable scopes; eviction never changes cursor keys.
#[derive(Default)]
pub struct ScopeCache {
    scopes: HashMap<ScopeKey, Arc<Scope>>,
    order: VecDeque<ScopeKey>,
    bytes: usize,
}
impl ScopeCache {
    fn insert(&mut self, key: ScopeKey, scope: Arc<Scope>) -> Arc<Scope> {
        if let Some(existing) = self.scopes.get(&key) {
            if existing.allocated == scope.allocated {
                return existing.clone();
            }
        }
        if let Some(old) = self.scopes.remove(&key) {
            self.bytes -= old.bytes;
            self.order.retain(|k| *k != key);
        }
        if scope.bytes > CACHE_BYTES {
            return scope;
        }
        while self.bytes + scope.bytes > CACHE_BYTES {
            if let Some(old) = self.order.pop_front() {
                if let Some(removed) = self.scopes.remove(&old) {
                    self.bytes -= removed.bytes;
                }
            } else {
                break;
            }
        }
        self.bytes += scope.bytes;
        self.order.push_back(key);
        self.scopes.insert(key, scope.clone());
        scope
    }
}
fn weight(stat: &ExtensionStat, allocated: bool) -> u64 {
    if allocated {
        stat.allocated
    } else {
        stat.size
    }
}
fn totals(stats: &[ExtensionStat]) -> Result<[u64; 3]> {
    let mut total = [0; 3];
    for stat in stats {
        for (sum, value) in total
            .iter_mut()
            .zip([stat.size, stat.allocated, stat.files])
        {
            *sum = store::checked_add(*sum, value, "扩展名汇总")?;
        }
    }
    Ok(total)
}
fn scope(
    comparison: &Comparison,
    side: usize,
    node: NodeId,
    allocated: bool,
) -> Result<Arc<Scope>> {
    let key = (side, node);
    let cached = comparison
        .extension_cache
        .lock()
        .map_err(|_| ApiError::new("INTERNAL_ERROR", "扩展名缓存锁不可用"))?
        .scopes
        .get(&key)
        .cloned();
    if let Some(ref cached) = cached {
        if cached.allocated == allocated {
            return Ok(cached.clone());
        }
    }
    // Keep one requested ordering per scope; metric switches share immutable totals.
    let statistics = if let Some(ref cached) = cached {
        cached.statistics.clone()
    } else if node == 0 {
        Arc::new(Statistics::Global)
    } else {
        let mut aggregated: HashMap<&str, [u64; 3]> = HashMap::new();
        let mut pending = Vec::new();
        if comparison
            .entry(node, side)
            .is_some_and(|entry| entry.kind == NodeKind::Directory)
        {
            pending.push(comparison.children(node).iter());
        }
        while let Some(children) = pending.last_mut() {
            let Some(&id) = children.next() else {
                pending.pop();
                continue;
            };
            let Some(entry) = comparison.entry(id, side) else {
                continue;
            };
            if entry.kind == NodeKind::Directory {
                pending.push(comparison.children(id).iter());
            } else {
                let total = aggregated.entry(comparison.extension(id)).or_default();
                for (sum, value) in total.iter_mut().zip([entry.size, entry.allocated, 1]) {
                    *sum = store::checked_add(*sum, value, "扩展名目录汇总")?;
                }
            }
        }
        let stats: Vec<_> = aggregated
            .into_iter()
            .map(|(extension, [size, allocated, files])| ExtensionStat {
                extension: extension.into(),
                size,
                allocated,
                files,
            })
            .collect();
        Arc::new(Statistics::Directory(stats))
    };
    let stats = match statistics.as_ref() {
        Statistics::Global => &comparison.extensions[side],
        Statistics::Directory(stats) => stats,
    };
    let mut order: Vec<_> = (0..stats.len()).collect();
    order.sort_unstable_by(|&a, &b| {
        weight(&stats[b], allocated)
            .cmp(&weight(&stats[a], allocated))
            .then_with(|| stats[a].extension.cmp(&stats[b].extension))
    });
    let total = match cached {
        Some(ref cached) => cached.total,
        None => totals(stats)?,
    };
    let bytes = order.capacity() * std::mem::size_of::<usize>()
        + std::mem::size_of::<Scope>()
        + std::mem::size_of::<Statistics>()
        + 160
        + match statistics.as_ref() {
            Statistics::Global => 0,
            Statistics::Directory(stats) => {
                stats.capacity() * std::mem::size_of::<ExtensionStat>()
                    + stats.iter().map(|v| v.extension.capacity()).sum::<usize>()
            }
        };
    let value = Scope {
        statistics,
        order,
        allocated,
        total,
        bytes,
    };
    let value = Arc::new(value);
    Ok(comparison
        .extension_cache
        .lock()
        .map_err(|_| ApiError::new("INTERNAL_ERROR", "扩展名缓存锁不可用"))?
        .insert(key, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::JobControl;

    #[test]
    fn metric_switches_share_statistics_keep_live_orders_and_account_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scope.csv");
        std::fs::write(
            &path,
            "文件名称,大小,分配\nC:\\r\\,0,0\nC:\\r\\a.txt,20,1\nC:\\r\\b.bin,10,30\n",
        )
        .unwrap();
        let c = store::build_comparison(&path, &path, "test", &JobControl::default(), &mut |_| {})
            .unwrap();
        let root = (1..=c.nodes.len() as u32)
            .find(|&id| c.container(id))
            .unwrap();
        for node in [0, root] {
            let size = scope(&c, 0, node, false).unwrap();
            assert_eq!(size.get(&c, 0, 0).extension, ".txt");
            for allocated in [true, false, true, false] {
                let current = scope(&c, 0, node, allocated).unwrap();
                assert!(Arc::ptr_eq(&size.statistics, &current.statistics));
                assert_eq!(current.total, [30, 31, 2]);
                assert_eq!(
                    current.get(&c, 0, 0).extension,
                    if allocated { ".bin" } else { ".txt" }
                );
                // A request already holding the previous ordering remains immutable.
                assert_eq!(size.get(&c, 0, 0).extension, ".txt");
                let cache = c.extension_cache.lock().unwrap();
                assert_eq!(
                    cache.bytes,
                    cache.scopes.values().map(|v| v.bytes).sum::<usize>()
                );
                assert_eq!(cache.order.len(), cache.scopes.len());
                assert!(cache.bytes <= CACHE_BYTES);
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
    weight: u64,
    extension: String,
}
fn item(stat: &ExtensionStat) -> ExtensionItem {
    ExtensionItem {
        extension: stat.extension.clone(),
        size: stat.size.to_string(),
        allocated: stat.allocated.to_string(),
        files: stat.files,
    }
}
fn value([size, allocated, files]: [u64; 3]) -> TypeValue {
    TypeValue {
        size: size.to_string(),
        allocated: allocated.to_string(),
        files,
    }
}
fn warnings() -> Vec<String> {
    vec![
        "按实际文件路径汇总；不使用目录导出汇总，不按 MFT 去重；缺失父目录会中断目录范围汇总"
            .into(),
    ]
}
pub fn list_extensions(
    comparison: &Comparison,
    comparison_id: &str,
    parent: Option<&str>,
    side: SnapshotSide,
    metric: Metric,
    cursor: Option<&str>,
) -> Result<ExtensionPage> {
    comparison.check_id(comparison_id)?;
    let node = match parent {
        None => 0,
        Some(id) => {
            let node = diff::node_number(id)?;
            if !comparison.valid_node(node) {
                return Err(ApiError::new("INVALID_NODE", "节点不存在或已过期"));
            }
            node
        }
    };
    let index = match side {
        SnapshotSide::Before => 0,
        SnapshotSide::After => 1,
    };
    let allocated = matches!(metric, Metric::Allocated);
    if node != 0 {
        if let Some(entry) = comparison
            .entry(node, index)
            .filter(|e| e.kind == NodeKind::File)
        {
            if cursor.is_some() {
                return Err(ApiError::new("INVALID_CURSOR", "文件范围没有后续分页"));
            }
            return Ok(ExtensionPage {
                rows: vec![ExtensionItem {
                    extension: comparison.extension(node).into(),
                    size: entry.size.to_string(),
                    allocated: entry.allocated.to_string(),
                    files: 1,
                }],
                next_cursor: None,
                total_extensions: 1,
                total: value([entry.size, entry.allocated, 1]),
                warnings: warnings(),
            });
        }
    }
    let stats = scope(comparison, index, node, allocated)?;
    let start = if let Some(text) = cursor {
        if text.len() > 32768 {
            return Err(ApiError::new("INVALID_CURSOR", "分页游标过长"));
        }
        let c: Cursor = serde_json::from_str(text)
            .map_err(|_| ApiError::new("INVALID_CURSOR", "无效分页游标"))?;
        if c.comparison != comparison_id
            || c.parent.as_deref() != parent
            || c.side != side
            || matches!(c.metric, Metric::Allocated) != allocated
        {
            return Err(ApiError::new("INVALID_CURSOR", "分页游标范围不匹配"));
        }
        let mut low = 0;
        let mut high = stats.len();
        while low < high {
            let mid = low + (high - low) / 2;
            let stat = stats.get(comparison, index, mid);
            match c
                .weight
                .cmp(&weight(stat, allocated))
                .then_with(|| stat.extension.cmp(&c.extension))
            {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => {
                    low = mid;
                    break;
                }
            }
        }
        if low == stats.len() || {
            let stat = stats.get(comparison, index, low);
            stat.extension != c.extension || weight(stat, allocated) != c.weight
        } {
            return Err(ApiError::new("INVALID_CURSOR", "分页游标排序键无效"));
        }
        low + 1
    } else {
        0
    };
    let end = (start + 200).min(stats.len());
    let rows = (start..end)
        .map(|i| item(stats.get(comparison, index, i)))
        .collect();
    let next_cursor = if end < stats.len() {
        let stat = stats.get(comparison, index, end - 1);
        Some(
            serde_json::to_string(&Cursor {
                comparison: comparison_id.into(),
                parent: parent.map(str::to_owned),
                side,
                metric,
                weight: weight(stat, allocated),
                extension: stat.extension.clone(),
            })
            .map_err(|e| ApiError::new("SERIALIZATION_ERROR", e.to_string()))?,
        )
    } else {
        None
    };
    Ok(ExtensionPage {
        rows,
        next_cursor,
        total_extensions: stats.len() as u64,
        total: value(stats.total),
        warnings: warnings(),
    })
}
