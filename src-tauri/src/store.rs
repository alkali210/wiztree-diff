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
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct BlobRef {
    block: u32,
    start: u32,
    len: u32,
}
#[derive(Default)]
struct BytesArena {
    blocks: Vec<Vec<u8>>,
}
impl BytesArena {
    #[inline]
    fn get(&self, r: BlobRef) -> &[u8] {
        if r.len == 0 {
            return &[];
        }
        &self.blocks[r.block as usize][r.start as usize..(r.start as usize + r.len as usize)]
    }
    fn put(&mut self, data: &[u8]) -> Result<BlobRef> {
        if data.is_empty() {
            return Ok(BlobRef::default());
        }
        let len = u32::try_from(data.len()).map_err(|_| memory_error())?;
        if self
            .blocks
            .last()
            .is_none_or(|b| b.capacity() - b.len() < data.len())
        {
            let mut block = Vec::new();
            block
                .try_reserve_exact(data.len().max(1024 * 1024))
                .map_err(|_| memory_error())?;
            self.blocks.try_reserve(1).map_err(|_| memory_error())?;
            self.blocks.push(block);
        }
        let block = u32::try_from(self.blocks.len() - 1).map_err(|_| memory_error())?;
        let bytes = self.blocks.last_mut().unwrap();
        let start = bytes.len() as u32;
        bytes.extend_from_slice(data);
        Ok(BlobRef { block, start, len })
    }
    fn bytes(&self) -> usize {
        self.blocks.iter().map(|b| b.capacity()).sum()
    }
}
/// Append-only arena storing strictly valid UTF-8 slices copied from `&str`.
#[derive(Default)]
struct Strings {
    blocks: Vec<Vec<u8>>,
}
impl Strings {
    #[inline]
    fn get(&self, r: TextRef) -> &str {
        if r.len == 0 {
            return "";
        }
        let slice =
            &self.blocks[r.block as usize][r.start as usize..(r.start as usize + r.len as usize)];
        // SAFETY: Strings::put only copies bytes from valid &str values, and TextRef fields are private.
        unsafe { std::str::from_utf8_unchecked(slice) }
    }
    fn put(&mut self, text: &str) -> Result<TextRef> {
        let data = text.as_bytes();
        if data.is_empty() {
            return Ok(TextRef::default());
        }
        let len = u32::try_from(data.len()).map_err(|_| memory_error())?;
        if self
            .blocks
            .last()
            .is_none_or(|b| b.capacity() - b.len() < data.len())
        {
            let mut block = Vec::new();
            block
                .try_reserve_exact(data.len().max(1024 * 1024))
                .map_err(|_| memory_error())?;
            self.blocks.try_reserve(1).map_err(|_| memory_error())?;
            self.blocks.push(block);
        }
        let block = u32::try_from(self.blocks.len() - 1).map_err(|_| memory_error())?;
        let bytes = self.blocks.last_mut().unwrap();
        let start = bytes.len() as u32;
        bytes.extend_from_slice(data);
        Ok(TextRef { block, start, len })
    }
    fn bytes(&self) -> usize {
        self.blocks.iter().map(|b| b.capacity()).sum()
    }
}
#[inline]
fn write_varint(buf: &mut Vec<u8>, mut value: usize) -> Result<()> {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        buf.try_reserve(1).map_err(|_| memory_error())?;
        buf.push(byte);
        if value == 0 {
            return Ok(());
        }
    }
}
#[inline]
fn read_varint(bytes: &[u8], offset: &mut usize) -> usize {
    let mut result = 0usize;
    let mut shift = 0u32;
    loop {
        let byte = bytes[*offset];
        *offset += 1;
        result |= ((byte & 0x7f) as usize) << shift;
        if byte & 0x80 == 0 {
            return result;
        }
        shift += 7;
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
    prefix: TextRef,
    basename: TextRef,
    pub parent: NodeId,
    pub before: u32,
    pub after: u32,
    pub depth: u32,
    pub changed_descendant_count: u32,
    extension: u32,
    pub(crate) aggregate: u32,
    pub status: Status,
    pub has_changes: bool,
    pub expandable: bool,
}
#[derive(Debug)]
pub struct SideEntry {
    pub size: u64,
    pub allocated: u64,
    pub hardlink_count: u32,
    details: u32,
    pub kind: NodeKind,
}
struct Details {
    prefix: TextRef,
    suffix: TextRef,
    data: BlobRef,
    mask: u16,
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
    detail_bytes: BytesArena,
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
    #[inline]
    pub(crate) fn extension_id(&self, id: NodeId) -> u32 {
        self.node(id).extension
    }
    #[inline]
    pub(crate) fn extension_text(&self, ext_id: u32) -> &str {
        self.text(self.extensions_names[ext_id as usize])
    }
    pub fn entry_details<'a>(&'a self, entry: &SideEntry) -> [Option<&'a str>; 14] {
        let d = &self.details[entry.details as usize - 1];
        let mut fields = [None; 14];
        let bytes = self.detail_bytes.get(d.data);
        let mut offset = 0usize;
        for (i, f) in fields.iter_mut().enumerate() {
            if d.mask & (1 << i) != 0 {
                let len = read_varint(bytes, &mut offset);
                // SAFETY: encoded details only store UTF-8 slices copied from &str CSV fields.
                *f = Some(unsafe { std::str::from_utf8_unchecked(&bytes[offset..offset + len]) });
                offset += len;
            }
        }
        fields
    }
    pub(crate) fn entry_field(&self, entry: &SideEntry, index: usize) -> Option<&str> {
        let d = &self.details[entry.details as usize - 1];
        let bit = 1u16 << index;
        if d.mask & bit == 0 {
            return None;
        }
        let bytes = self.detail_bytes.get(d.data);
        let mut offset = 0usize;
        let mut prior = d.mask & (bit - 1);
        while prior != 0 {
            let len = read_varint(bytes, &mut offset);
            offset += len;
            prior &= prior - 1;
        }
        let len = read_varint(bytes, &mut offset);
        // SAFETY: encoded details only store UTF-8 slices copied from &str CSV fields.
        Some(unsafe { std::str::from_utf8_unchecked(&bytes[offset..offset + len]) })
    }
    fn entry_details_eq(&self, d: &Details, mask: u16, values: &[Option<&str>; 14]) -> bool {
        if d.mask != mask {
            return false;
        }
        let bytes = self.detail_bytes.get(d.data);
        let mut offset = 0usize;
        let mut remaining = mask;
        while remaining != 0 {
            let i = remaining.trailing_zeros() as usize;
            remaining &= remaining - 1;
            let len = read_varint(bytes, &mut offset);
            let expected = values[i].unwrap().as_bytes();
            if len != expected.len() || &bytes[offset..offset + len] != expected {
                return false;
            }
            offset += len;
        }
        true
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
            + self.detail_bytes.bytes()
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
    last_prefix: TextRef,
    last_raw_prefix: TextRef,
    extension_ids: HashMap<String, u32>,
    extension_categories: Vec<u8>,
    extension_values: [Vec<[u64; 3]>; 2],
    mft_count: [usize; 2],
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
    fn new(id: &str, est_before: usize, est_after: usize) -> Self {
        let mut nodes = Vec::new();
        let mut entries_before = Vec::new();
        let mut entries_after = Vec::new();
        let mut details = Vec::new();
        let mut index = HashTable::new();
        let est_union = est_before.max(est_after);
        if est_union > 1024 {
            let _ = nodes.try_reserve(est_union);
            let _ = details.try_reserve(est_union);
            let _ = index.try_reserve(est_union, |_| 0);
        }
        if est_before > 1024 {
            let _ = entries_before.try_reserve(est_before);
        }
        if est_after > 1024 {
            let _ = entries_after.try_reserve(est_after);
        }
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
                    order_warning: None,
                },
                nodes,
                entries: [entries_before, entries_after],
                strings: Strings::default(),
                detail_bytes: BytesArena::default(),
                details,
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
            index,
            last_prefix: TextRef::default(),
            last_raw_prefix: TextRef::default(),
            extension_ids: HashMap::from([(String::new(), 0)]),
            extension_categories: vec![7],
            extension_values: [vec![[0; 3]], vec![[0; 3]]],
            mft_count: [0; 2],
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
    #[inline]
    fn intern_canonical_prefix(&mut self, value: &str) -> Result<TextRef> {
        if value.is_empty() {
            return Ok(TextRef::default());
        }
        if self.last_prefix.len as usize == value.len()
            && self.data.text(self.last_prefix) == value
        {
            return Ok(self.last_prefix);
        }
        let r = self.intern_prefix(value)?;
        self.last_prefix = r;
        Ok(r)
    }
    #[inline]
    fn intern_raw_prefix(&mut self, value: &str, canonical: TextRef) -> Result<TextRef> {
        if value.is_empty() {
            return Ok(TextRef::default());
        }
        if self.last_raw_prefix.len as usize == value.len()
            && self.data.text(self.last_raw_prefix) == value
        {
            return Ok(self.last_raw_prefix);
        }
        if canonical.len as usize == value.len() && self.data.text(canonical) == value {
            self.last_raw_prefix = canonical;
            return Ok(canonical);
        }
        let r = self.intern_prefix(value)?;
        self.last_raw_prefix = r;
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
        let cat = if let Some(ext) = text.strip_prefix('.') {
            crate::file_categories::category_index_for_ext(ext) as u8
        } else {
            7
        };
        push(&mut self.extension_categories, cat)?;
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
        let prefix = self.intern_canonical_prefix(prefix)?;
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
                        prefix,
                        basename,
                        parent: 0,
                        before: 0,
                        after: 0,
                        depth: row.depth,
                        changed_descendant_count: 0,
                        extension,
                        aggregate: 0,
                        status: Status::Added,
                        has_changes: true,
                        expandable: false,
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
        let (raw_prefix, raw_suffix) = if prefix.len == 0 {
            ("", row.original)
        } else {
            let trimmed = row.original.trim_end_matches(['\\', '/']);
            match trimmed.rfind(['\\', '/']) {
                Some(i) => row.original.split_at(i + 1),
                None => ("", row.original),
            }
        };
        let raw_prefix = self.intern_raw_prefix(raw_prefix, prefix)?;
        let mut mask = 0u16;
        for (i, value) in row.values.iter().enumerate() {
            if value.is_some() {
                mask |= 1 << i;
            }
        }
        let shared = self.data.entry(id, 0).filter(|_| side == 1).and_then(|e| {
            let d = &self.data.details[e.details as usize - 1];
            if d.prefix == raw_prefix
                && d.mask == mask
                && self.data.text(d.suffix) == raw_suffix
                && self.data.entry_details_eq(d, mask, &row.values)
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
                let mut remaining = mask;
                while remaining != 0 {
                    let i = remaining.trailing_zeros() as usize;
                    remaining &= remaining - 1;
                    let value = row.values[i].unwrap();
                    write_varint(&mut self.encoded, value.len())?;
                    self.encoded
                        .try_reserve(value.len())
                        .map_err(|_| memory_error())?;
                    self.encoded.extend_from_slice(value.as_bytes());
                }
                let data = self.data.detail_bytes.put(&self.encoded)?;
                push(
                    &mut self.data.details,
                    Details {
                        prefix: raw_prefix,
                        suffix,
                        data,
                        mask,
                    },
                )?
            }
        };
        let entry = push(
            &mut self.data.entries[side],
            SideEntry {
                size: row.size,
                allocated: row.allocated,
                hardlink_count: 0,
                details,
                kind: row.kind,
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
            if mask & (1 << 4) != 0 {
                self.mft_count[side] += 1;
            }
            self.totals[side][0] = checked_add(self.totals[side][0], row.size, "文件路径大小")?;
            self.totals[side][1] =
                checked_add(self.totals[side][1], row.allocated, "文件路径分配")?;
            let ext_id = self.data.node(id).extension as usize;
            let category = if row.name.starts_with('.') {
                crate::file_categories::category_index(row.name)
            } else if ext_id == 0 {
                if row.name.eq_ignore_ascii_case("README") {
                    5
                } else {
                    7
                }
            } else {
                let ext_len = self.data.extensions_names[ext_id].len as usize;
                let name_bytes = row.name.as_bytes();
                if name_bytes.len() >= ext_len
                    && name_bytes[name_bytes.len() - ext_len] == b'.'
                    && name_bytes[name_bytes.len() - ext_len..].is_ascii()
                {
                    self.extension_categories[ext_id] as usize
                } else {
                    crate::file_categories::category_index(row.name)
                }
            };
            let values = &mut self.data.categories[side][category];
            values[0] = checked_add(values[0], row.size, "文件类别大小")?;
            values[1] = checked_add(values[1], row.allocated, "文件类别分配")?;
            values[2] += 1;
            let values = &mut self.extension_values[side][ext_id];
            values[0] = checked_add(values[0], row.size, "扩展名大小")?;
            values[1] = checked_add(values[1], row.allocated, "扩展名分配")?;
            values[2] += 1;
        }
        Ok(())
    }
    fn finish(mut self, control: &JobControl) -> Result<Comparison> {
        let count = self.data.nodes.len();
        // Resolve each actual immediate parent once; cache by interned prefix TextRef.
        let mut prefix_parents: hashbrown::HashMap<TextRef, NodeId> = hashbrown::HashMap::new();
        let mut last_prefix = TextRef::default();
        let mut last_parent = 0u32;
        for i in 0..count {
            if i % 1024 == 0 {
                control.check()?;
            }
            let prefix = self.data.nodes[i].prefix;
            let parent = if prefix.len == 0 {
                0
            } else if prefix == last_prefix {
                last_parent
            } else {
                let resolved = if let Some(&p) = prefix_parents.get(&prefix) {
                    p
                } else {
                    let p = parent_from_prefix(self.data.text(prefix))
                        .and_then(|p| self.lookup(p))
                        .unwrap_or(0);
                    prefix_parents.try_reserve(1).map_err(|_| memory_error())?;
                    prefix_parents.insert(prefix, p);
                    p
                };
                last_prefix = prefix;
                last_parent = resolved;
                resolved
            };
            self.data.nodes[i].parent = parent;
            if parent != 0 {
                for side in 0..2 {
                    if self.data.entry(i as u32 + 1, side).is_some()
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
        }
        drop(prefix_parents);
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
            if hi - lo > 1 {
                self.data.child_ids[lo..hi].sort_unstable_by(|a, b| {
                    let (na, nb) = (&nodes[*a as usize - 1], &nodes[*b as usize - 1]);
                    nb.expandable
                        .cmp(&na.expandable)
                        .then_with(|| strings.get(na.basename).cmp(strings.get(nb.basename)))
                        .then(a.cmp(b))
                });
            }
        }
        // Only containers retain aggregates and participate in bottom-up aggregation;
        // tally all node statuses in one linear pass and collect containers.
        let mut containers = Vec::new();
        let mut max_depth = 0u32;
        for id in 1..=count as u32 {
            let expandable = self.data.node(id).expandable;
            let status = self.data.node(id).status;
            let target = if expandable {
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
            if self.data.container(id) {
                let index = self.data.aggregates.len() as u32;
                push(&mut self.data.aggregates, FileAggregate::default())?;
                let n = &mut self.data.nodes[id as usize - 1];
                n.aggregate = index;
                if n.depth > max_depth {
                    max_depth = n.depth;
                }
                containers.try_reserve(1).map_err(|_| memory_error())?;
                containers.push(id);
            }
        }
        if max_depth <= 4096 {
            let buckets = max_depth as usize + 1;
            let mut depth_counts = filled(buckets, 0usize)?;
            for &id in &containers {
                depth_counts[self.data.node(id).depth as usize] += 1;
            }
            let mut depth_offsets = filled(buckets, 0usize)?;
            let mut running = 0usize;
            for d in (0..buckets).rev() {
                depth_offsets[d] = running;
                running += depth_counts[d];
            }
            let mut ordered = filled(containers.len(), 0u32)?;
            for &id in &containers {
                let d = self.data.node(id).depth as usize;
                ordered[depth_offsets[d]] = id;
                depth_offsets[d] += 1;
            }
            containers = ordered;
        } else {
            containers.sort_unstable_by_key(|&id| std::cmp::Reverse(self.data.node(id).depth));
        }
        let mut export_counts = filled(self.data.aggregates.len(), [0u64; 4])?;
        let mut mismatches = 0u64;
        for (i, id) in containers.iter().copied().enumerate() {
            if i % 1024 == 0 {
                control.check()?;
            }
            let mut aggregate = self.data.own_files(id);
            let mut changed = 0u32;
            let mut counts = [0u64; 4];
            let is_dir_side = [
                self.data
                    .entry(id, 0)
                    .is_some_and(|e| e.kind == NodeKind::Directory),
                self.data
                    .entry(id, 1)
                    .is_some_and(|e| e.kind == NodeKind::Directory),
            ];
            for &child in self.data.children(id) {
                aggregate.add(self.data.aggregate(child))?;
                let cn = self.data.node(child);
                changed += cn.changed_descendant_count + u32::from(cn.status != Status::Unchanged);
                for side in 0..2 {
                    if is_dir_side[side] {
                        if let Some(e) = self.data.entry(child, side) {
                            if e.kind == NodeKind::File {
                                counts[side * 2] += 1;
                            } else {
                                counts[side * 2 + 1] += 1;
                                let c = export_counts[cn.aggregate as usize];
                                counts[side * 2] += c[side * 2];
                                counts[side * 2 + 1] += c[side * 2 + 1];
                            }
                        }
                    }
                }
            }
            let index = self.data.node(id).aggregate;
            self.data.aggregates[index as usize] = aggregate;
            export_counts[index as usize] = counts;
            if self.data.node(id).expandable {
                let n = &mut self.data.nodes[id as usize - 1];
                n.changed_descendant_count = changed;
                n.has_changes |= changed > 0;
                for side in 0..2 {
                    if is_dir_side[side] {
                        if let Some(e) = self.data.entry(id, side) {
                            if self
                                .data
                                .entry_field(e, 2)
                                .is_some_and(|v| v.parse::<u64>().unwrap() != counts[side * 2])
                                || self
                                    .data
                                    .entry_field(e, 3)
                                    .is_some_and(|v| v.parse::<u64>().unwrap() != counts[side * 2 + 1])
                            {
                                mismatches += 1;
                            }
                        }
                    }
                }
            }
        }
        drop(export_counts);
        drop(containers);
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
        self.extension_categories = Vec::new();
        self.encoded = Vec::new();
        // Compact immutable arrays when slack is significant, avoiding full-array copies for tight preallocs.
        fn shrink_if_excess<T>(v: &mut Vec<T>) {
            if v.capacity() > v.len() + (v.len() >> 4) + 64 {
                v.shrink_to_fit();
            }
        }
        shrink_if_excess(&mut self.data.nodes);
        shrink_if_excess(&mut self.data.details);
        shrink_if_excess(&mut self.data.aggregates);
        shrink_if_excess(&mut self.data.extensions_names);
        for entries in &mut self.data.entries {
            shrink_if_excess(entries);
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
        #[inline]
        fn parse_u64_fast(s: &str) -> Option<u64> {
            let bytes = s.as_bytes();
            if bytes.len() > 20 {
                return None;
            }
            if bytes.len() <= 19 {
                let mut acc = 0u64;
                for &b in bytes {
                    acc = acc * 10 + (b - b'0') as u64;
                }
                return Some(acc);
            }
            s.parse::<u64>().ok()
        }
        for side in 0..2 {
            let mft_total = self.mft_count[side];
            if mft_total == 0 {
                continue;
            }
            // Split borrows so large MFT slices can borrow `detail_bytes` while `entries` is mutated.
            let Comparison {
                nodes,
                entries,
                strings,
                detail_bytes,
                details,
                ..
            } = &mut self.data;
            let side_entries = &mut entries[side];
            let mut small: Vec<(u32, u64, u32)> = Vec::new();
            small
                .try_reserve_exact(mft_total)
                .map_err(|_| memory_error())?;
            let mut large: Vec<(u32, &str, u32)> = Vec::new();
            let mut volumes: Vec<&str> = Vec::new();
            let mut vol_map: hashbrown::HashMap<&str, u32> = hashbrown::HashMap::new();
            let mut last_prefix = TextRef::default();
            let mut last_vol_id = 0u32;
            for (idx, n) in nodes.iter().enumerate() {
                if idx % 1024 == 0 {
                    control.check()?;
                }
                let entry_idx = if side == 0 { n.before } else { n.after };
                if entry_idx == 0 {
                    continue;
                }
                let e = &side_entries[entry_idx as usize - 1];
                if e.kind != NodeKind::File {
                    continue;
                }
                let d = &details[e.details as usize - 1];
                if d.mask & (1 << 4) == 0 {
                    continue;
                }
                // Decode field 4 (MFTRECNO) directly from detail_bytes.
                let bytes = detail_bytes.get(d.data);
                let mut offset = 0usize;
                let mut prior = d.mask & ((1u16 << 4) - 1);
                while prior != 0 {
                    let len = read_varint(bytes, &mut offset);
                    offset += len;
                    prior &= prior - 1;
                }
                let len = read_varint(bytes, &mut offset);
                // SAFETY: encoded details only store UTF-8 slices copied from &str CSV fields.
                let mft = unsafe { std::str::from_utf8_unchecked(&bytes[offset..offset + len]) };

                let vol_id = if n.prefix.len != 0 && n.prefix == last_prefix {
                    last_vol_id
                } else {
                    let source = if n.prefix.len == 0 {
                        strings.get(n.basename)
                    } else {
                        strings.get(n.prefix)
                    };
                    let vol = import::volume(source);
                    let id = if volumes.len() <= 8 {
                        if let Some(pos) = volumes.iter().position(|&v| v == vol) {
                            pos as u32
                        } else {
                            let id = volumes.len() as u32;
                            volumes.push(vol);
                            id
                        }
                    } else {
                        if vol_map.is_empty() {
                            for (i, &v) in volumes.iter().enumerate() {
                                vol_map.insert(v, i as u32);
                            }
                        }
                        if let Some(&id) = vol_map.get(vol) {
                            id
                        } else {
                            let id = volumes.len() as u32;
                            volumes.push(vol);
                            vol_map.try_reserve(1).map_err(|_| memory_error())?;
                            vol_map.insert(vol, id);
                            id
                        }
                    };
                    if n.prefix.len != 0 {
                        last_prefix = n.prefix;
                        last_vol_id = id;
                    }
                    id
                };

                if let Some(num) = parse_u64_fast(mft) {
                    small.push((vol_id, num, entry_idx));
                } else {
                    large.try_reserve(1).map_err(|_| memory_error())?;
                    large.push((vol_id, mft, entry_idx));
                }
            }
            control.check()?;
            let single_volume = volumes.len() <= 1;
            drop(vol_map);
            drop(volumes);
            if single_volume {
                small.sort_unstable_by_key(|&(_, mft, _)| mft);
            } else {
                small.sort_unstable_by_key(|&(vol, mft, _)| (vol, mft));
            }
            control.check()?;
            let mut start = 0;
            while start < small.len() {
                if start % 1024 == 0 {
                    control.check()?;
                }
                let (vol, mft, _) = small[start];
                let mut end = start + 1;
                while end < small.len() && small[end].0 == vol && small[end].1 == mft {
                    end += 1;
                }
                let count = (end - start) as u32;
                for &(_, _, entry_idx) in &small[start..end] {
                    side_entries[entry_idx as usize - 1].hardlink_count = count;
                }
                start = end;
            }
            drop(small);
            if !large.is_empty() {
                large.sort_unstable_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
                control.check()?;
                let mut start = 0;
                while start < large.len() {
                    if start % 1024 == 0 {
                        control.check()?;
                    }
                    let (vol, mft, _) = large[start];
                    let mut end = start + 1;
                    while end < large.len() && large[end].0 == vol && large[end].1 == mft {
                        end += 1;
                    }
                    let count = (end - start) as u32;
                    for &(_, _, entry_idx) in &large[start..end] {
                        side_entries[entry_idx as usize - 1].hardlink_count = count;
                    }
                    start = end;
                }
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
fn estimate_rows(path: &Path) -> usize {
    std::fs::metadata(path)
        .ok()
        .map_or(0, |m| (m.len() / 180).min(8_000_000) as usize)
}
pub fn build_comparison(
    before: &Path,
    after: &Path,
    comparison_id: &str,
    control: &JobControl,
    progress: &mut dyn FnMut(Progress),
) -> Result<Comparison> {
    let mut builder = Builder::new(comparison_id, estimate_rows(before), estimate_rows(after));
    builder.data.summary.before =
        import::load(before, 0, control, progress, |row| builder.add(0, row))?;
    builder.data.summary.after =
        import::load(after, 1, control, progress, |row| builder.add(1, row))?;
    builder.data.summary.order_warning = import::snapshot_order_warning(
        builder.data.summary.before.description.as_deref(),
        builder.data.summary.after.description.as_deref(),
    );
    progress(Progress {
        phase: "finalizing",
        bytes_read: 0,
        total_bytes: 0,
        rows: builder.data.summary.before.rows + builder.data.summary.after.rows,
    });
    builder.finish(control)
}
