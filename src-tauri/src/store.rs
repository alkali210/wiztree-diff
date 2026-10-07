use crate::{
    import::{self, JobControl, Progress},
    types::*,
};
use hashbrown::HashTable;
use std::{collections::HashMap, hash::BuildHasher, path::Path, sync::Mutex};

pub type NodeId = u32;
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
pub struct TextRef {
    block: u32,
    start: u32,
    len: u32,
}
#[derive(Default)]
struct Strings {
    blocks: Vec<Vec<u8>>,
}
impl Strings {
    fn get(&self, r: TextRef) -> &str {
        std::str::from_utf8(self.bytes_ref(r)).expect("text references contain UTF-8")
    }
    fn bytes_ref(&self, r: TextRef) -> &[u8] {
        if r.len == 0 {
            return &[];
        }
        &self.blocks[r.block as usize][r.start as usize..(r.start as usize + r.len as usize)]
    }
    fn put(&mut self, text: &str) -> Result<TextRef> {
        self.put_bytes(text.as_bytes())
    }
    fn put_bytes(&mut self, text: &[u8]) -> Result<TextRef> {
        if text.is_empty() {
            return Ok(TextRef::default());
        }
        let len = u32::try_from(text.len()).map_err(|_| memory_error())?;
        if self
            .blocks
            .last()
            .is_none_or(|b| b.capacity() - b.len() < text.len())
        {
            let mut block = Vec::new();
            block
                .try_reserve_exact(text.len().max(1024 * 1024))
                .map_err(|_| memory_error())?;
            self.blocks.try_reserve(1).map_err(|_| memory_error())?;
            self.blocks.push(block);
        }
        let block = u32::try_from(self.blocks.len() - 1).map_err(|_| memory_error())?;
        let bytes = self.blocks.last_mut().unwrap();
        let start = bytes.len() as u32;
        bytes.extend_from_slice(text);
        Ok(TextRef { block, start, len })
    }
    fn bytes(&self) -> usize {
        self.blocks.iter().map(|b| b.capacity()).sum()
    }
}
fn memory_error() -> ApiError {
    ApiError::new(
        "MEMORY_LIMIT",
        "内存不足或记录数超出运行时容量；未截断输入，也未写入磁盘缓存",
    )
}
fn push<T>(v: &mut Vec<T>, item: T) -> Result<u32> {
    let id = u32::try_from(v.len() + 1).map_err(|_| memory_error())?;
    v.try_reserve(1).map_err(|_| memory_error())?;
    v.push(item);
    Ok(id)
}
#[derive(Debug)]
pub struct Node {
    pub parent: NodeId,
    pub before: u32,
    pub after: u32,
    pub depth: u32,
    pub status: Status,
    pub has_changes: bool,
    pub expandable: bool,
    pub changed_descendant_count: u64,
    prefix: TextRef,
    basename: TextRef,
    extension: u32,
    pub(crate) aggregate: u32,
}
#[derive(Debug)]
pub struct SideEntry {
    pub kind: NodeKind,
    pub size: u64,
    pub allocated: u64,
    pub hardlink_count: u64,
    details: u32,
}
struct Details {
    prefix: TextRef,
    suffix: TextRef,
    mask: u16,
    data: TextRef,
}
#[derive(Clone, Copy, Default, Debug)]
pub struct FileAggregate {
    /// Before, after, and gross per-file change. Directory CSV totals never enter these arrays.
    pub size: [u64; 3],
    pub allocated: [u64; 3],
    pub files: [u64; 3],
    pub visible_size: [u64; 3],
    pub visible_allocated: [u64; 3],
    pub added: u64,
    /// Added, removed, typeChanged, modified file-path markers.
    pub changes: [u64; 4],
}
impl FileAggregate {
    fn add(&mut self, other: Self) -> Result<()> {
        for (a, b) in [
            (&mut self.size, other.size),
            (&mut self.allocated, other.allocated),
            (&mut self.files, other.files),
            (&mut self.visible_size, other.visible_size),
            (&mut self.visible_allocated, other.visible_allocated),
        ] {
            for i in 0..3 {
                a[i] = a[i]
                    .checked_add(b[i])
                    .ok_or_else(|| ApiError::new("AGGREGATE_OVERFLOW", "文件路径汇总溢出"))?;
            }
        }
        self.added += other.added;
        for i in 0..4 {
            self.changes[i] += other.changes[i];
        }
        for values in [self.size, self.allocated] {
            if values[0] > i64::MAX as u64 || values[1] > i64::MAX as u64 {
                return Err(ApiError::new(
                    "AGGREGATE_OVERFLOW",
                    "单侧文件路径汇总超出 i64 范围",
                ));
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct ExtensionStat {
    pub extension: String,
    pub size: u64,
    pub allocated: u64,
    pub files: u64,
}
/// A published comparison is immutable. Only bounded requested-directory statistics are memoized.
pub struct Comparison {
    pub summary: ComparisonSummary,
    pub nodes: Vec<Node>,
    entries: [Vec<SideEntry>; 2],
    strings: Strings,
    details: Vec<Details>,
    extensions_names: Vec<TextRef>,
    offsets: Vec<u32>,
    child_ids: Vec<NodeId>,
    changed_offsets: Vec<u32>,
    changed_ids: Vec<NodeId>,
    pub(crate) aggregates: Vec<FileAggregate>,
    pub roots: [Vec<NodeId>; 2],
    pub categories: [[[u64; 3]; 8]; 2],
    pub extensions: [Vec<ExtensionStat>; 2],
    pub(crate) extension_cache: Mutex<crate::file_extensions::ScopeCache>,
}
impl Comparison {
    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize - 1]
    }
    pub fn valid_node(&self, id: NodeId) -> bool {
        id != 0 && id as usize <= self.nodes.len()
    }
    pub fn text(&self, r: TextRef) -> &str {
        self.strings.get(r)
    }
    pub fn entry(&self, id: NodeId, side: usize) -> Option<&SideEntry> {
        let node = self.node(id);
        let index = if side == 0 { node.before } else { node.after };
        index
            .checked_sub(1)
            .map(|i| &self.entries[side][i as usize])
    }
    pub fn children(&self, parent: NodeId) -> &[NodeId] {
        &self.child_ids
            [self.offsets[parent as usize] as usize..self.offsets[parent as usize + 1] as usize]
    }
    pub fn changed_children(&self, parent: NodeId) -> &[NodeId] {
        &self.changed_ids[self.changed_offsets[parent as usize] as usize
            ..self.changed_offsets[parent as usize + 1] as usize]
    }
    pub fn container(&self, id: NodeId) -> bool {
        self.node(id).expandable || !self.children(id).is_empty()
    }
    pub fn basename(&self, id: NodeId) -> &str {
        self.text(self.node(id).basename)
    }
    pub fn canonical_path(&self, id: NodeId) -> String {
        let n = self.node(id);
        let mut out = self.text(n.prefix).to_owned();
        out.push_str(self.text(n.basename));
        out
    }
    pub fn path(&self, id: NodeId, side: usize) -> String {
        let Some(e) = self.entry(id, side) else {
            return String::new();
        };
        let d = &self.details[e.details as usize - 1];
        let mut out = self.text(d.prefix).to_owned();
        out.push_str(self.text(d.suffix));
        out
    }
    pub fn name(&self, id: NodeId) -> &str {
        let e = self.entry(id, 0).or_else(|| self.entry(id, 1)).unwrap();
        let d = &self.details[e.details as usize - 1];
        if self.text(self.node(id).prefix).is_empty() {
            self.text(d.suffix)
        } else {
            self.text(d.suffix).trim_end_matches(['\\', '/'])
        }
    }
    pub fn extension(&self, id: NodeId) -> &str {
        self.text(self.extensions_names[self.node(id).extension as usize])
    }
    pub fn entry_details<'a>(&'a self, entry: &SideEntry) -> [Option<&'a str>; 14] {
        let d = &self.details[entry.details as usize - 1];
        let mut fields = [None; 14];
        let mut bytes = self.strings.bytes_ref(d.data);
        for (i, f) in fields.iter_mut().enumerate() {
            if d.mask & (1 << i) != 0 {
                let len = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
                *f = Some(std::str::from_utf8(&bytes[4..4 + len]).expect("details contain UTF-8"));
                bytes = &bytes[4 + len..];
            }
        }
        fields
    }
    fn field_ref(&self, entry: &SideEntry, index: usize) -> Option<TextRef> {
        let d = &self.details[entry.details as usize - 1];
        if d.mask & (1 << index) == 0 {
            return None;
        }
        let bytes = self.strings.bytes_ref(d.data);
        let mut offset = 0usize;
        for i in 0..=index {
            if d.mask & (1 << i) != 0 {
                let len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
                offset += 4;
                if i == index {
                    return Some(TextRef {
                        block: d.data.block,
                        start: d.data.start + offset as u32,
                        len,
                    });
                }
                offset += len as usize;
            }
        }
        None
    }
    pub fn own_files(&self, id: NodeId) -> FileAggregate {
        let b = self.entry(id, 0).filter(|e| e.kind == NodeKind::File);
        let a = self.entry(id, 1).filter(|e| e.kind == NodeKind::File);
        let size = [b.map_or(0, |e| e.size), a.map_or(0, |e| e.size)];
        let allocated = [b.map_or(0, |e| e.allocated), a.map_or(0, |e| e.allocated)];
        let status = &self.node(id).status;
        FileAggregate {
            size: [size[0], size[1], size[1].abs_diff(size[0])],
            allocated: [
                allocated[0],
                allocated[1],
                allocated[1].abs_diff(allocated[0]),
            ],
            files: [
                u64::from(b.is_some()),
                u64::from(a.is_some()),
                u64::from(b.is_some() || a.is_some()),
            ],
            visible_size: [
                u64::from(size[0] > 0),
                u64::from(size[1] > 0),
                u64::from(size[0] != size[1]),
            ],
            visible_allocated: [
                u64::from(allocated[0] > 0),
                u64::from(allocated[1] > 0),
                u64::from(allocated[0] != allocated[1]),
            ],
            added: u64::from(a.is_some() && b.is_none()),
            changes: [
                u64::from(matches!(status, Status::Added)),
                u64::from(matches!(status, Status::Removed)),
                u64::from(matches!(status, Status::TypeChanged)),
                u64::from(matches!(status, Status::Modified)),
            ]
            .map(|v| if a.is_some() || b.is_some() { v } else { 0 }),
        }
    }
    pub fn aggregate(&self, id: NodeId) -> FileAggregate {
        if id == 0 {
            return self.aggregates[0];
        }
        let index = self.node(id).aggregate;
        if index == 0 {
            self.own_files(id)
        } else {
            self.aggregates[index as usize]
        }
    }
    pub fn check_id(&self, comparison_id: &str) -> Result<()> {
        if comparison_id == self.summary.comparison_id {
            Ok(())
        } else {
            Err(ApiError::new("STALE_COMPARISON", "对比 ID 已过期"))
        }
    }
    pub fn volume(&self, id: NodeId) -> &str {
        let n = self.node(id);
        let prefix = self.text(n.prefix);
        import::volume(if prefix.is_empty() {
            self.text(n.basename)
        } else {
            prefix
        })
    }
    pub fn memory_bytes(&self) -> usize {
        self.strings.bytes()
            + self.nodes.capacity() * std::mem::size_of::<Node>()
            + self
                .entries
                .iter()
                .map(|e| e.capacity() * std::mem::size_of::<SideEntry>())
                .sum::<usize>()
            + self.details.capacity() * std::mem::size_of::<Details>()
            + (self.offsets.capacity()
                + self.changed_offsets.capacity()
                + self.child_ids.capacity()
                + self.changed_ids.capacity())
                * 4
            + self.aggregates.capacity() * std::mem::size_of::<FileAggregate>()
    }
}

pub fn checked_add(a: u64, b: u64, field: &str) -> Result<u64> {
    a.checked_add(b)
        .filter(|v| *v <= i64::MAX as u64)
        .ok_or_else(|| ApiError::new("AGGREGATE_OVERFLOW", format!("{field} 超出 i64 范围")))
}

struct Builder {
    data: Comparison,
    hasher: hashbrown::DefaultHashBuilder,
    prefixes: HashTable<TextRef>,
    index: HashTable<NodeId>,
    extension_ids: HashMap<String, u32>,
    extension_values: [Vec<[u64; 3]>; 2],
    totals: [[u64; 2]; 2],
    encoded: Vec<u8>,
}
fn empty_summary() -> SourceSummary {
    SourceSummary {
        path: String::new(),
        description: None,
        rows: 0,
        files: 0,
        folders: 0,
        roots: Vec::new(),
        root_count: 0,
        roots_truncated: false,
        size: "0".into(),
        allocated: "0".into(),
    }
}
impl Builder {
    fn new(id: &str) -> Self {
        Self {
            data: Comparison {
                summary: ComparisonSummary {
                    comparison_id: id.into(),
                    before: empty_summary(),
                    after: empty_summary(),
                    statuses: Counts {
                        files: StatusCounts::default(),
                        folders: StatusCounts::default(),
                    },
                    warnings: Vec::new(),
                },
                nodes: Vec::new(),
                entries: [Vec::new(), Vec::new()],
                strings: Strings::default(),
                details: Vec::new(),
                extensions_names: vec![TextRef::default()],
                offsets: Vec::new(),
                child_ids: Vec::new(),
                changed_offsets: Vec::new(),
                changed_ids: Vec::new(),
                aggregates: vec![FileAggregate::default()],
                roots: [Vec::new(), Vec::new()],
                categories: [[[0; 3]; 8]; 2],
                extensions: [Vec::new(), Vec::new()],
                extension_cache: Mutex::new(Default::default()),
            },
            hasher: Default::default(),
            prefixes: HashTable::new(),
            index: HashTable::new(),
            extension_ids: HashMap::from([(String::new(), 0)]),
            extension_values: [vec![[0; 3]], vec![[0; 3]]],
            totals: [[0; 2]; 2],
            encoded: Vec::with_capacity(1024),
        }
    }
    fn intern_prefix(&mut self, value: &str) -> Result<TextRef> {
        if value.is_empty() {
            return Ok(TextRef::default());
        }
        let hash = self.hasher.hash_one(value);
        if let Some(r) = self.prefixes.find(hash, |r| self.data.text(*r) == value) {
            return Ok(*r);
        }
        let r = self.data.strings.put(value)?;
        let c = &self.data;
        let hasher = &self.hasher;
        self.prefixes
            .try_reserve(1, |r| hasher.hash_one(c.text(*r)))
            .map_err(|_| memory_error())?;
        self.prefixes
            .insert_unique(hash, r, |r| hasher.hash_one(c.text(*r)));
        Ok(r)
    }
    fn lookup(&self, path: &str) -> Option<NodeId> {
        let (prefix, basename) = import::canonical_parts(path);
        let prefix = if prefix.is_empty() {
            TextRef::default()
        } else {
            *self.prefixes.find(self.hasher.hash_one(prefix), |r| {
                self.data.text(*r) == prefix
            })?
        };
        let hash = self.hasher.hash_one((prefix, basename));
        self.index
            .find(hash, |id| {
                let n = self.data.node(*id);
                n.prefix == prefix && self.data.text(n.basename) == basename
            })
            .copied()
    }
    fn extension_id(&mut self, text: &str) -> Result<u32> {
        if let Some(id) = self.extension_ids.get(text) {
            return Ok(*id);
        }
        let r = self.data.strings.put(text)?;
        let id = self.data.extensions_names.len() as u32;
        push(&mut self.data.extensions_names, r)?;
        self.extension_ids
            .try_reserve(1)
            .map_err(|_| memory_error())?;
        self.extension_ids.insert(text.into(), id);
        for values in &mut self.extension_values {
            push(values, [0; 3])?;
        }
        Ok(id)
    }
    fn add(&mut self, side: usize, row: import::ParsedRow<'_>) -> Result<()> {
        let (prefix, basename) = import::canonical_parts(row.key);
        let prefix = self.intern_prefix(prefix)?;
        let hash = self.hasher.hash_one((prefix, basename));
        let found = self
            .index
            .find(hash, |id| {
                let n = self.data.node(*id);
                n.prefix == prefix && self.data.text(n.basename) == basename
            })
            .copied();
        let id = match found {
            Some(id) => id,
            None => {
                let extension = self.extension_id(crate::file_extensions::extension(basename))?;
                let basename = self.data.strings.put(basename)?;
                let id = push(
                    &mut self.data.nodes,
                    Node {
                        parent: 0,
                        before: 0,
                        after: 0,
                        depth: row.depth,
                        status: Status::Added,
                        has_changes: true,
                        expandable: false,
                        changed_descendant_count: 0,
                        prefix,
                        basename,
                        extension,
                        aggregate: 0,
                    },
                )?;
                let c = &self.data;
                let h = &self.hasher;
                self.index
                    .try_reserve(1, |id| {
                        let n = c.node(*id);
                        h.hash_one((n.prefix, c.text(n.basename)))
                    })
                    .map_err(|_| memory_error())?;
                self.index.insert_unique(hash, id, |id| {
                    let n = c.node(*id);
                    h.hash_one((n.prefix, c.text(n.basename)))
                });
                id
            }
        };
        if self.data.entry(id, side).is_some() {
            let mut e = ApiError::new("INVALID_FIELD", "重复或大小写规范化冲突路径");
            e.record = Some(row.record);
            e.column = Some(import::COLUMNS[0].into());
            return Err(e);
        }
        // Preserve exact original spelling while sharing the directory prefix.
        let (raw_prefix, raw_suffix) = if self.data.text(prefix).is_empty() {
            ("", row.original)
        } else {
            let trimmed = row.original.trim_end_matches(['\\', '/']);
            match trimmed.rfind(['\\', '/']) {
                Some(i) => row.original.split_at(i + 1),
                None => ("", row.original),
            }
        };
        let raw_prefix = self.intern_prefix(raw_prefix)?;
        let shared = self.data.entry(id, 0).filter(|_| side == 1).and_then(|e| {
            let d = &self.data.details[e.details as usize - 1];
            if d.prefix == raw_prefix
                && self.data.text(d.suffix) == raw_suffix
                && self.data.entry_details(e) == row.values
            {
                Some(e.details)
            } else {
                None
            }
        });
        let details = match shared {
            Some(id) => id,
            None => {
                let suffix = if raw_suffix == self.data.basename(id) {
                    self.data.node(id).basename
                } else {
                    self.data.strings.put(raw_suffix)?
                };
                self.encoded.clear();
                let mut mask = 0;
                for (i, value) in row.values.iter().enumerate() {
                    if let Some(value) = value {
                        mask |= 1 << i;
                        let len = u32::try_from(value.len()).map_err(|_| memory_error())?;
                        self.encoded
                            .try_reserve(value.len() + 4)
                            .map_err(|_| memory_error())?;
                        self.encoded.extend_from_slice(&len.to_le_bytes());
                        self.encoded.extend_from_slice(value.as_bytes());
                    }
                }
                let data = self.data.strings.put_bytes(&self.encoded)?;
                push(
                    &mut self.data.details,
                    Details {
                        prefix: raw_prefix,
                        suffix,
                        mask,
                        data,
                    },
                )?
            }
        };
        let entry = push(
            &mut self.data.entries[side],
            SideEntry {
                kind: row.kind,
                size: row.size,
                allocated: row.allocated,
                hardlink_count: 0,
                details,
            },
        )?;
        let node = &mut self.data.nodes[id as usize - 1];
        if side == 0 {
            node.before = entry;
        } else {
            node.after = entry;
        }
        node.expandable |= row.kind == NodeKind::Directory;
        let status = match (self.data.entry(id, 0), self.data.entry(id, 1)) {
            (None, Some(_)) => Status::Added,
            (Some(_), None) => Status::Removed,
            (Some(b), Some(a)) if b.kind != a.kind => Status::TypeChanged,
            (Some(b), Some(a)) if b.size != a.size || b.allocated != a.allocated => {
                Status::Modified
            }
            _ => Status::Unchanged,
        };
        let n = &mut self.data.nodes[id as usize - 1];
        n.status = status;
        n.has_changes = status != Status::Unchanged;
        if row.kind == NodeKind::File {
            self.totals[side][0] = checked_add(self.totals[side][0], row.size, "文件路径大小")?;
            self.totals[side][1] =
                checked_add(self.totals[side][1], row.allocated, "文件路径分配")?;
            let category = crate::file_categories::CATEGORIES
                .iter()
                .position(|c| *c == crate::file_categories::category(row.name))
                .unwrap();
            let values = &mut self.data.categories[side][category];
            values[0] = checked_add(values[0], row.size, "文件类别大小")?;
            values[1] = checked_add(values[1], row.allocated, "文件类别分配")?;
            values[2] += 1;
            let values = &mut self.extension_values[side][self.data.node(id).extension as usize];
            values[0] = checked_add(values[0], row.size, "扩展名大小")?;
            values[1] = checked_add(values[1], row.allocated, "扩展名分配")?;
            values[2] += 1;
        }
        Ok(())
    }
    fn finish(mut self, control: &JobControl) -> Result<Comparison> {
        let count = self.data.nodes.len();
        // Resolve each actual immediate parent once; prefix keys are not synthetic nodes.
        for i in 0..count {
            if i % 1024 == 0 {
                control.check()?;
            }
            let parent = parent_from_prefix(self.data.text(self.data.nodes[i].prefix))
                .and_then(|p| self.lookup(p))
                .unwrap_or(0);
            self.data.nodes[i].parent = parent;
            for side in 0..2 {
                if parent != 0
                    && self.data.entry(i as u32 + 1, side).is_some()
                    && self
                        .data
                        .entry(parent, side)
                        .is_some_and(|e| e.kind == NodeKind::File)
                {
                    return Err(ApiError::new(
                        "INVALID_HIERARCHY",
                        format!(
                            "同侧父路径记录是文件：{}",
                            self.data.path(i as u32 + 1, side)
                        ),
                    ));
                }
            }
        }
        self.data.offsets = filled(count + 2, 0u32)?;
        for n in &self.data.nodes {
            self.data.offsets[n.parent as usize + 1] += 1;
        }
        for i in 1..self.data.offsets.len() {
            self.data.offsets[i] += self.data.offsets[i - 1];
        }
        let mut positions = self.data.offsets.clone();
        self.data.child_ids = filled(count, 0u32)?;
        for (i, n) in self.data.nodes.iter().enumerate() {
            self.data.child_ids[positions[n.parent as usize] as usize] = i as u32 + 1;
            positions[n.parent as usize] += 1;
        }
        drop(positions);
        let nodes = &self.data.nodes;
        let strings = &self.data.strings;
        for parent in 0..=count {
            if parent % 1024 == 0 {
                control.check()?;
            }
            let lo = self.data.offsets[parent] as usize;
            let hi = self.data.offsets[parent + 1] as usize;
            self.data.child_ids[lo..hi].sort_unstable_by(|a, b| {
                let (na, nb) = (&nodes[*a as usize - 1], &nodes[*b as usize - 1]);
                nb.expandable
                    .cmp(&na.expandable)
                    .then_with(|| strings.get(na.basename).cmp(strings.get(nb.basename)))
                    .then(a.cmp(b))
            });
        }
        // Only containers retain aggregates; leaves are calculated directly from side values.
        for id in 1..=count as u32 {
            if self.data.container(id) {
                let index = self.data.aggregates.len() as u32;
                push(&mut self.data.aggregates, FileAggregate::default())?;
                self.data.nodes[id as usize - 1].aggregate = index;
            }
        }
        let mut order = Vec::new();
        order.try_reserve_exact(count).map_err(|_| memory_error())?;
        order.extend(1..=count as u32);
        order.sort_unstable_by_key(|id| std::cmp::Reverse(self.data.node(*id).depth));
        let mut export_counts = filled(self.data.aggregates.len(), [0u64; 4])?;
        let mut mismatches = 0u64;
        for (i, id) in order.iter().copied().enumerate() {
            if i % 1024 == 0 {
                control.check()?;
            }
            let mut aggregate = self.data.own_files(id);
            let mut changed = 0u64;
            let mut counts = [0u64; 4];
            for &child in self.data.children(id) {
                aggregate.add(self.data.aggregate(child))?;
                changed += self.data.node(child).changed_descendant_count
                    + u64::from(self.data.node(child).status != Status::Unchanged);
                for side in 0..2 {
                    if self
                        .data
                        .entry(id, side)
                        .is_some_and(|e| e.kind == NodeKind::Directory)
                    {
                        if let Some(e) = self.data.entry(child, side) {
                            if e.kind == NodeKind::File {
                                counts[side * 2] += 1;
                            } else {
                                counts[side * 2 + 1] += 1;
                                let c = export_counts[self.data.node(child).aggregate as usize];
                                counts[side * 2] += c[side * 2];
                                counts[side * 2 + 1] += c[side * 2 + 1];
                            }
                        }
                    }
                }
            }
            let index = self.data.node(id).aggregate;
            if index != 0 {
                self.data.aggregates[index as usize] = aggregate;
                export_counts[index as usize] = counts;
            }
            if self.data.node(id).expandable {
                let n = &mut self.data.nodes[id as usize - 1];
                n.changed_descendant_count = changed;
                n.has_changes |= changed > 0;
                for side in 0..2 {
                    if let Some(e) = self
                        .data
                        .entry(id, side)
                        .filter(|e| e.kind == NodeKind::Directory)
                    {
                        let details = self.data.entry_details(e);
                        if details[2].is_some_and(|v| v.parse::<u64>().unwrap() != counts[side * 2])
                            || details[3]
                                .is_some_and(|v| v.parse::<u64>().unwrap() != counts[side * 2 + 1])
                        {
                            mismatches += 1;
                        }
                    }
                }
            }
            let status = self.data.node(id).status;
            let target = if self.data.node(id).expandable {
                &mut self.data.summary.statuses.folders
            } else {
                &mut self.data.summary.statuses.files
            };
            match status {
                Status::Added => target.added += 1,
                Status::Removed => target.removed += 1,
                Status::Modified => target.modified += 1,
                Status::Unchanged => target.unchanged += 1,
                Status::TypeChanged => target.type_changed += 1,
            }
        }
        drop(export_counts);
        drop(order);
        let mut total = FileAggregate::default();
        for &id in self.data.children(0) {
            total.add(self.data.aggregate(id))?;
        }
        self.data.aggregates[0] = total;
        self.data.changed_offsets = filled(count + 2, 0u32)?;
        let changes = self.data.nodes.iter().filter(|n| n.has_changes).count();
        self.data
            .changed_ids
            .try_reserve_exact(changes)
            .map_err(|_| memory_error())?;
        for parent in 0..=count {
            for position in self.data.offsets[parent]..self.data.offsets[parent + 1] {
                let id = self.data.child_ids[position as usize];
                if self.data.node(id).has_changes {
                    self.data.changed_ids.push(id);
                }
            }
            self.data.changed_offsets[parent + 1] = self.data.changed_ids.len() as u32;
        }
        if mismatches > 0 {
            self.data.summary.warnings.push(format!(
                "{mismatches} 条目录导出计数与实际后代不符；导出范围可能不一致/不完整"
            ));
        }
        self.finish_roots(control)?;
        // Parent/range validation is complete. Do not keep build-only dictionaries
        // alive while grouping MFT records or publishing the resident comparison.
        self.index = HashTable::new();
        self.prefixes = HashTable::new();
        self.extension_ids = HashMap::new();
        self.encoded = Vec::new();
        // These immutable arrays will never grow again. Compact once, not per row.
        self.data.nodes.shrink_to_fit();
        self.data.details.shrink_to_fit();
        self.data.aggregates.shrink_to_fit();
        self.data.extensions_names.shrink_to_fit();
        for entries in &mut self.data.entries {
            entries.shrink_to_fit();
        }
        self.finish_hardlinks(control)?;
        for side in 0..2 {
            for (id, values) in self.extension_values[side].iter().enumerate() {
                if values[2] > 0 {
                    let item = ExtensionStat {
                        extension: self.data.text(self.data.extensions_names[id]).into(),
                        size: values[0],
                        allocated: values[1],
                        files: values[2],
                    };
                    push(&mut self.data.extensions[side], item)?;
                }
            }
        }
        control.check()?;
        Ok(self.data)
    }
    fn finish_roots(&mut self, control: &JobControl) -> Result<()> {
        for side in 0..2 {
            let (mut size, mut allocated, mut missing, mut nested_missing) =
                (0u64, 0u64, 0u64, 0u64);
            let mut examples = Vec::new();
            for id in 1..=self.data.nodes.len() as u32 {
                if id % 1024 == 0 {
                    control.check()?;
                }
                let Some(entry) = self.data.entry(id, side) else {
                    continue;
                };
                let parent = self.data.node(id).parent;
                if parent != 0 && self.data.entry(parent, side).is_some() {
                    continue;
                }
                let (kind, entry_size, entry_allocated) = (entry.kind, entry.size, entry.allocated);
                let mut ancestor = parent_from_prefix(self.data.text(self.data.node(id).prefix));
                let mut nested = false;
                while let Some(path) = ancestor {
                    if let Some(node) = self.lookup(path) {
                        if let Some(e) = self.data.entry(node, side) {
                            if e.kind == NodeKind::File {
                                return Err(ApiError::new(
                                    "INVALID_HIERARCHY",
                                    format!("同侧祖先路径记录是文件：{}", self.data.path(id, side)),
                                ));
                            }
                            nested = true;
                            break;
                        }
                    }
                    ancestor = canonical_parent(path);
                }
                if kind == NodeKind::File {
                    missing += 1;
                    if examples.len() < 5 {
                        examples.push(self.data.path(id, side));
                    }
                }
                if nested {
                    nested_missing += 1;
                } else {
                    size = checked_add(size, entry_size, "根大小")?;
                    allocated = checked_add(allocated, entry_allocated, "根分配")?;
                }
                push(&mut self.data.roots[side], id)?;
            }
            let mut roots = std::mem::take(&mut self.data.roots[side]);
            roots.sort_unstable_by(|a, b| canonical_cmp(&self.data, *a, *b).then(a.cmp(b)));
            let preview: Vec<Root> = roots
                .iter()
                .take(200)
                .map(|id| {
                    let e = self.data.entry(*id, side).unwrap();
                    Root {
                        node_id: format!("n{id}"),
                        path: self.data.path(*id, side),
                        kind: if e.kind == NodeKind::File {
                            "file"
                        } else {
                            "directory"
                        }
                        .into(),
                        size: e.size.to_string(),
                        allocated: e.allocated.to_string(),
                    }
                })
                .collect();
            let summary = if side == 0 {
                &mut self.data.summary.before
            } else {
                &mut self.data.summary.after
            };
            summary.size = size.to_string();
            summary.allocated = allocated.to_string();
            summary.root_count = roots.len() as u64;
            summary.roots_truncated = roots.len() > 200;
            summary.roots = preview;
            self.data.roots[side] = roots;
            if nested_missing > 0 {
                self.data.summary.warnings.push(format!("{}快照有 {nested_missing} 个嵌套导出根缺少中间父目录记录；导出范围可能不完整，工作区汇总不重复累加嵌套根",if side==0{"之前"}else{"之后"}));
            }
            if missing > 0 {
                self.data.summary.warnings.push(format!(
                    "{}快照有 {missing} 个文件缺少父目录记录，导出范围可能不完整；示例：{}",
                    if side == 0 { "之前" } else { "之后" },
                    examples.join("、")
                ));
            }
        }
        if self.data.roots[0] != self.data.roots[1] {
            self.data
                .summary
                .warnings
                .push("导出根范围不同；差异仅表示 CSV 记录差异，不断言磁盘删除".into());
        }
        Ok(())
    }
    fn finish_hardlinks(&mut self, control: &JobControl) -> Result<()> {
        // Cache byte spans once. Sorting must not decode all cold CSV fields per comparison.
        struct Key {
            node: NodeId,
            volume: TextRef,
            mft: TextRef,
        }
        for side in 0..2 {
            let count = self.data.entries[side]
                .iter()
                .filter(|e| {
                    e.kind == NodeKind::File
                        && self.data.details[e.details as usize - 1].mask & (1 << 4) != 0
                })
                .count();
            let mut keys = Vec::new();
            keys.try_reserve_exact(count).map_err(|_| memory_error())?;
            for id in 1..=self.data.nodes.len() as u32 {
                if id % 1024 == 0 {
                    control.check()?;
                }
                let Some(e) = self
                    .data
                    .entry(id, side)
                    .filter(|e| e.kind == NodeKind::File)
                else {
                    continue;
                };
                let Some(mft) = self.data.field_ref(e, 4) else {
                    continue;
                };
                let n = self.data.node(id);
                let mut volume = if n.prefix.len == 0 {
                    n.basename
                } else {
                    n.prefix
                };
                volume.len = self.data.volume(id).len() as u32;
                keys.push(Key {
                    node: id,
                    volume,
                    mft,
                });
            }
            control.check()?;
            let strings = &self.data.strings;
            keys.sort_unstable_by(|a, b| {
                strings
                    .bytes_ref(a.volume)
                    .cmp(strings.bytes_ref(b.volume))
                    .then_with(|| strings.bytes_ref(a.mft).cmp(strings.bytes_ref(b.mft)))
            });
            control.check()?;
            let mut start = 0;
            while start < keys.len() {
                control.check()?;
                let mut end = start + 1;
                while end < keys.len()
                    && self.data.strings.bytes_ref(keys[start].volume)
                        == self.data.strings.bytes_ref(keys[end].volume)
                    && self.data.strings.bytes_ref(keys[start].mft)
                        == self.data.strings.bytes_ref(keys[end].mft)
                {
                    if end % 1024 == 0 {
                        control.check()?;
                    }
                    end += 1;
                }
                for key in &keys[start..end] {
                    let n = self.data.node(key.node);
                    let index = if side == 0 { n.before } else { n.after };
                    self.data.entries[side][index as usize - 1].hardlink_count =
                        (end - start) as u64;
                }
                start = end;
            }
        }
        Ok(())
    }
}
fn filled<T: Clone>(count: usize, value: T) -> Result<Vec<T>> {
    let mut v = Vec::new();
    v.try_reserve_exact(count).map_err(|_| memory_error())?;
    v.resize(count, value);
    Ok(v)
}
fn parent_from_prefix(prefix: &str) -> Option<&str> {
    if prefix.is_empty() {
        return None;
    }
    let trimmed = prefix.trim_end_matches('\\');
    if (trimmed.len() == 2 && trimmed.ends_with(':'))
        || (trimmed.starts_with("\\\\") && trimmed[2..].split('\\').count() == 2)
    {
        Some(prefix)
    } else {
        Some(trimmed)
    }
}
fn canonical_parent(path: &str) -> Option<&str> {
    if path.ends_with('\\') {
        return None;
    }
    path.rfind('\\').map(|i| {
        let p = &path[..i];
        if (p.len() == 2 && p.ends_with(':'))
            || (p.starts_with("\\\\") && p[2..].split('\\').count() == 2)
        {
            &path[..=i]
        } else {
            p
        }
    })
}
fn canonical_cmp(c: &Comparison, a: NodeId, b: NodeId) -> std::cmp::Ordering {
    let (a, b) = (c.node(a), c.node(b));
    c.text(a.prefix)
        .bytes()
        .chain(c.text(a.basename).bytes())
        .cmp(c.text(b.prefix).bytes().chain(c.text(b.basename).bytes()))
}
pub fn build_comparison(
    before: &Path,
    after: &Path,
    comparison_id: &str,
    control: &JobControl,
    progress: &mut dyn FnMut(Progress),
) -> Result<Comparison> {
    let mut builder = Builder::new(comparison_id);
    builder.data.summary.before =
        import::load(before, 0, control, progress, |row| builder.add(0, row))?;
    builder.data.summary.after =
        import::load(after, 1, control, progress, |row| builder.add(1, row))?;
    progress(Progress {
        phase: "finalizing".into(),
        bytes_read: 0,
        total_bytes: 0,
        rows: builder.data.summary.before.rows + builder.data.summary.after.rows,
    });
    builder.finish(control)
}
