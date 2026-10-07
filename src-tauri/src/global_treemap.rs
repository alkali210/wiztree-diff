//! One requested, immutable treemap in memory. Geometry always retains full f64 precision.
use crate::{
    import::JobControl,
    store::{Comparison, FileAggregate, NodeId},
    types::*,
};
use base64::Engine;
use std::collections::VecDeque;

pub const ATLAS_WIDTH: u32 = 4096;
pub const ATLAS_HEIGHT: u32 = 1024;
const DIRECTORY_HEADER: f64 = 64.0;
const DIRECTORY_GUTTER: f64 = 8.0;
const LEAF: u8 = 1;
const OWN: u8 = 2;
const HEADER: u8 = 4;
const SPATIAL_BLOCK: usize = 144;
const PALETTE: [[u8; 3]; 16] = [
    [70, 124, 227],
    [152, 72, 209],
    [35, 166, 124],
    [207, 126, 26],
    [226, 68, 88],
    [173, 173, 29],
    [30, 163, 202],
    [191, 72, 162],
    [86, 166, 75],
    [102, 90, 205],
    [206, 106, 44],
    [45, 153, 160],
    [193, 68, 55],
    [105, 141, 30],
    [125, 125, 139],
    [41, 114, 180],
];
pub fn extension_color(extension: &str) -> [u8; 3] {
    let mut hash = 2166136261u32;
    for byte in extension.bytes() {
        hash = (hash ^ byte as u32).wrapping_mul(16777619);
    }
    PALETTE[(hash & 15) as usize]
}
fn side(mode: ChartMode) -> usize {
    match mode {
        ChartMode::Before => 0,
        ChartMode::After => 1,
        ChartMode::Delta => 2,
    }
}
fn values(a: FileAggregate, metric: Metric) -> [u64; 3] {
    match metric {
        Metric::Size => a.size,
        Metric::Allocated => a.allocated,
    }
}
fn visible_counts(a: FileAggregate, metric: Metric) -> [u64; 3] {
    match metric {
        Metric::Size => a.visible_size,
        Metric::Allocated => a.visible_allocated,
    }
}
fn memory_error() -> ApiError {
    ApiError::new("MEMORY_LIMIT", "内存不足；未截断图块，也未写入磁盘缓存")
}
fn push<T>(items: &mut Vec<T>, value: T) -> Result<u32> {
    let index = u32::try_from(items.len()).map_err(|_| memory_error())?;
    items.try_reserve(1).map_err(|_| memory_error())?;
    items.push(value);
    Ok(index)
}
fn zeroes<T: Clone>(count: usize, zero: T) -> Result<Vec<T>> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(count)
        .map_err(|_| memory_error())?;
    result.resize(count, zero);
    Ok(result)
}
#[derive(Clone, Copy, Default)]
struct BoxRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
impl BoxRect {
    fn atlas() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }
    fn bounds(self) -> TreemapRect {
        TreemapRect {
            x: self.x,
            y: self.y,
            width: self.w,
            height: self.h,
        }
    }
    fn pixels(self) -> Self {
        Self {
            x: self.x * ATLAS_WIDTH as f64,
            y: self.y * ATLAS_HEIGHT as f64,
            w: self.w * ATLAS_WIDTH as f64,
            h: self.h * ATLAS_HEIGHT as f64,
        }
    }
    fn normalized(self) -> Self {
        Self {
            x: self.x / ATLAS_WIDTH as f64,
            y: self.y / ATLAS_HEIGHT as f64,
            w: self.w / ATLAS_WIDTH as f64,
            h: self.h / ATLAS_HEIGHT as f64,
        }
    }
    fn contains(self, x: f64, y: f64) -> bool {
        self.x <= x
            && self.y <= y
            && (x < self.x + self.w || (x == 1.0 && self.x + self.w >= 1.0))
            && (y < self.y + self.h || (y == 1.0 && self.y + self.h >= 1.0))
    }
    fn union(self, b: Self) -> Self {
        let x = self.x.min(b.x);
        let y = self.y.min(b.y);
        Self {
            x,
            y,
            w: (self.x + self.w).max(b.x + b.w) - x,
            h: (self.y + self.h).max(b.y + b.h) - y,
        }
    }
}
#[derive(Clone, Copy)]
struct Item {
    rect: BoxRect,
    weight: u64,
    node: NodeId,
    flags: u8,
}
impl Item {
    fn own(self) -> bool {
        self.flags & OWN != 0
    }
    fn leaf(self) -> bool {
        self.flags & LEAF != 0
    }
    fn header(self) -> f64 {
        if self.flags & HEADER != 0 {
            DIRECTORY_HEADER / ATLAS_HEIGHT as f64
        } else {
            0.0
        }
    }
    fn key(self) -> i64 {
        if self.own() {
            -(self.node as i64)
        } else {
            self.node as i64
        }
    }
    fn aggregate(self, c: &Comparison) -> FileAggregate {
        if self.own() {
            c.own_files(self.node)
        } else {
            c.aggregate(self.node)
        }
    }
    fn hit_rect(self) -> BoxRect {
        if self.leaf() {
            self.rect
        } else {
            BoxRect {
                h: self.header(),
                ..self.rect
            }
        }
    }
    fn visible(self, c: &Comparison, mode: ChartMode) -> bool {
        !matches!(mode, ChartMode::Delta) || self.aggregate(c).changes.iter().any(|n| *n > 0)
    }
}
fn directory(c: &Comparison, node: NodeId, mode: ChartMode) -> bool {
    match mode {
        ChartMode::Delta => c.node(node).expandable,
        _ => c
            .entry(node, side(mode))
            .is_some_and(|e| e.kind == NodeKind::Directory),
    }
}
fn content(rect: BoxRect, is_directory: bool) -> (BoxRect, bool) {
    let mut p = rect.pixels();
    let header = is_directory && p.w >= 256.0 && p.h >= 192.0;
    if header {
        p.x += DIRECTORY_GUTTER;
        p.y += DIRECTORY_HEADER;
        p.w -= 2.0 * DIRECTORY_GUTTER;
        p.h -= DIRECTORY_HEADER + DIRECTORY_GUTTER;
    }
    (p.normalized(), header)
}
fn siblings(
    c: &Comparison,
    parent: NodeId,
    metric: Metric,
    mode: ChartMode,
    items: &mut Vec<Item>,
    control: &JobControl,
) -> Result<()> {
    items.clear();
    for (i, &node) in c.children(parent).iter().enumerate() {
        if i % 512 == 0 {
            control.check()?;
        }
        let weight = values(c.aggregate(node), metric)[side(mode)];
        if weight > 0 {
            push(
                items,
                Item {
                    node,
                    weight,
                    rect: BoxRect::default(),
                    flags: if c.container(node) { 0 } else { LEAF },
                },
            )?;
        }
    }
    if parent != 0 {
        let weight = values(c.own_files(parent), metric)[side(mode)];
        if weight > 0 {
            push(
                items,
                Item {
                    node: parent,
                    weight,
                    rect: BoxRect::default(),
                    flags: OWN | LEAF,
                },
            )?;
        }
    }
    items.sort_unstable_by(|a, b| b.weight.cmp(&a.weight).then_with(|| b.key().cmp(&a.key())));
    control.check()
}
fn worst(sum: u64, min: u64, max: u64, r: BoxRect, remaining: u64) -> f64 {
    let scale = r.w * r.h / remaining as f64;
    let short = r.w.min(r.h);
    let area = sum as f64 * scale;
    (short * short * max as f64 * scale / (area * area))
        .max(area * area / (short * short * min as f64 * scale))
}
fn place_row(
    items: &mut [Item],
    sum: u64,
    r: &mut BoxRect,
    remaining: u64,
    control: &JobControl,
) -> Result<()> {
    let vertical = r.w >= r.h;
    let fraction = sum as f64 / remaining as f64;
    let thickness = if vertical {
        r.w * fraction
    } else {
        r.h * fraction
    };
    let length = if vertical { r.h } else { r.w };
    let mut offset = 0.0;
    for (i, item) in items.iter_mut().enumerate() {
        if i % 512 == 0 {
            control.check()?;
        }
        let segment = length * (item.weight as f64 / sum as f64);
        item.rect = if vertical {
            BoxRect {
                x: r.x,
                y: r.y + offset,
                w: thickness,
                h: segment,
            }
        } else {
            BoxRect {
                x: r.x + offset,
                y: r.y,
                w: segment,
                h: thickness,
            }
        };
        item.rect = item.rect.normalized();
        offset += segment;
    }
    // Compute the residual with exact integer subtraction, not 1.0-fraction.
    let residual = (remaining - sum) as f64 / remaining as f64;
    if vertical {
        r.x += thickness;
        r.w *= residual;
    } else {
        r.y += thickness;
        r.h *= residual;
    }
    Ok(())
}
fn partition(items: &mut [Item], rect: BoxRect, total: u64, control: &JobControl) -> Result<()> {
    let mut r = rect.pixels();
    let mut remaining = total;
    let mut start = 0;
    let mut sum = 0u64;
    let mut min = 0u64;
    let mut max = 0u64;
    for i in 0..items.len() {
        if i % 512 == 0 {
            control.check()?;
        }
        let weight = items[i].weight;
        let candidate = sum
            .checked_add(weight)
            .ok_or_else(|| ApiError::new("AGGREGATE_OVERFLOW", "变化权重溢出"))?;
        if i > start
            && worst(candidate, min.min(weight), max.max(weight), r, remaining)
                > worst(sum, min, max, r, remaining)
        {
            place_row(&mut items[start..i], sum, &mut r, remaining, control)?;
            remaining -= sum;
            start = i;
            sum = 0;
        }
        if i == start {
            min = weight;
            max = weight;
        }
        sum += weight;
        min = min.min(weight);
        max = max.max(weight);
    }
    if start < items.len() {
        place_row(&mut items[start..], sum, &mut r, remaining, control)?;
    }
    Ok(())
}
// Only before container content rectangles are retained temporarily. No before
// PNG, labels, full frame, spatial index, or file geometry is generated.
fn container_slot(c: &Comparison, node: NodeId) -> usize {
    if node == 0 {
        0
    } else {
        c.node(node).aggregate as usize
    }
}
fn before_reference(
    c: &Comparison,
    metric: Metric,
    max_depth: u32,
    control: &JobControl,
) -> Result<Vec<BoxRect>> {
    let mut reference = zeroes(c.aggregates.len(), BoxRect::default())?;
    reference[0] = BoxRect::atlas();
    let mut queue = VecDeque::from([(0, BoxRect::atlas(), 0u32)]);
    let mut scratch = Vec::new();
    while let Some((parent, rect, level)) = queue.pop_front() {
        control.check()?;
        let total = values(c.aggregate(parent), metric)[0];
        if total == 0 {
            continue;
        }
        siblings(c, parent, metric, ChartMode::Before, &mut scratch, control)?;
        partition(&mut scratch, rect, total, control)?;
        for (i, item) in scratch.iter().enumerate() {
            if i % 512 == 0 {
                control.check()?;
            }
            if item.leaf() {
                continue;
            }
            let is_dir = directory(c, item.node, ChartMode::Before);
            let (inner, _) = content(item.rect, is_dir);
            reference[container_slot(c, item.node)] = inner;
            if max_depth == 0 || level + 1 < max_depth || !is_dir {
                queue.try_reserve(1).map_err(|_| memory_error())?;
                queue.push_back((item.node, inner, level + 1));
            }
        }
    }
    Ok(reference)
}
fn unchanged_siblings(
    c: &Comparison,
    parent: NodeId,
    metric: Metric,
    control: &JobControl,
) -> Result<bool> {
    for (i, &node) in c.children(parent).iter().enumerate() {
        if i % 512 == 0 {
            control.check()?;
        }
        let weights = values(c.aggregate(node), metric);
        if weights[0] != weights[1] {
            return Ok(false);
        }
    }
    if parent != 0 {
        let weights = values(c.own_files(parent), metric);
        if weights[0] != weights[1] {
            return Ok(false);
        }
    }
    Ok(true)
}
#[derive(Clone, Copy, Default)]
struct SpatialNode {
    rect: BoxRect,
    start: u32,
    count: u32,
    left: u32,
    right: u32,
}
/// Geometry, node locations, and a balanced spatial index for exactly one view.
pub struct TreemapLayout {
    comparison_id: String,
    metric: Metric,
    mode: ChartMode,
    max_depth: u32,
    items: Vec<Item>,
    locations: Vec<u32>,
    spatial: Vec<SpatialNode>,
    spatial_items: Vec<u32>,
    png: Vec<u8>,
    positive: u64,
    negative: u64,
    exported: i128,
    rendered: u64,
    labels: Vec<TreemapLabel>,
}
impl TreemapLayout {
    pub fn build(
        c: &Comparison,
        metric: Metric,
        mode: ChartMode,
        max_depth: u32,
        control: &JobControl,
    ) -> Result<Self> {
        control.check()?;
        let mut result = Self {
            comparison_id: c.summary.comparison_id.clone(),
            metric,
            mode,
            max_depth,
            items: Vec::new(),
            locations: zeroes(c.nodes.len() + 1, u32::MAX)?,
            spatial: Vec::new(),
            spatial_items: Vec::new(),
            png: Vec::new(),
            positive: 0,
            negative: 0,
            exported: 0,
            rendered: 0,
            labels: Vec::new(),
        };
        let reference = if matches!(mode, ChartMode::After) && values(c.aggregate(0), metric)[1] > 0
        {
            Some(before_reference(c, metric, max_depth, control)?)
        } else {
            None
        };
        let mut queue = VecDeque::from([(0, BoxRect::atlas(), 0u32)]);
        let mut scratch = Vec::new();
        // Level batches preserve the established parent-key and sibling draw order.
        while !queue.is_empty() {
            control.check()?;
            let count = queue.len();
            queue.make_contiguous().sort_unstable_by_key(|p| p.0);
            for _ in 0..count {
                let (parent, rect, level) = queue.pop_front().unwrap();
                let total = values(c.aggregate(parent), metric)[side(mode)];
                if total == 0 {
                    continue;
                }
                let old = reference.as_ref().map(|r| r[container_slot(c, parent)]);
                let reuse = old.is_some_and(|r| r.w > 0.0 && r.h > 0.0)
                    && unchanged_siblings(c, parent, metric, control)?;
                if reuse {
                    let old = old.unwrap();
                    siblings(c, parent, metric, ChartMode::Before, &mut scratch, control)?;
                    partition(&mut scratch, old, total, control)?;
                    let sx = rect.w / old.w;
                    let sy = rect.h / old.h;
                    let tx = rect.x - old.x * sx;
                    let ty = rect.y - old.y * sy;
                    for item in &mut scratch {
                        item.rect = BoxRect {
                            x: item.rect.x * sx + tx,
                            y: item.rect.y * sy + ty,
                            w: item.rect.w * sx,
                            h: item.rect.h * sy,
                        };
                    }
                } else {
                    siblings(c, parent, metric, mode, &mut scratch, control)?;
                    partition(&mut scratch, rect, total, control)?;
                }
                for mut item in scratch.iter().copied() {
                    if !item.leaf() {
                        let is_dir = directory(c, item.node, mode);
                        let (inner, header) = content(item.rect, is_dir);
                        if header {
                            item.flags |= HEADER;
                        }
                        if max_depth != 0 && level + 1 >= max_depth && is_dir {
                            item.flags |= LEAF;
                        } else {
                            queue.try_reserve(1).map_err(|_| memory_error())?;
                            queue.push_back((item.node, inner, level + 1));
                        }
                    }
                    let index = push(&mut result.items, item)?;
                    if !item.own() {
                        result.locations[item.node as usize] = index;
                    }
                }
            }
        }
        drop(reference);
        result.index(c, control)?;
        let mut pixels = zeroes(ATLAS_WIDTH as usize * ATLAS_HEIGHT as usize * 4, 0u8)?;
        result.paint(c, control, &mut pixels)?;
        {
            let mut encoder = png::Encoder::new(&mut result.png, ATLAS_WIDTH, ATLAS_HEIGHT);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            encoder
                .write_header()
                .map_err(png_error)?
                .write_image_data(&pixels)
                .map_err(png_error)?;
        }
        result.finish_metadata(c, control)?;
        control.check()?;
        Ok(result)
    }
    fn index(&mut self, c: &Comparison, control: &JobControl) -> Result<()> {
        for (i, item) in self.items.iter().copied().enumerate() {
            if i % 512 == 0 {
                control.check()?;
            }
            if item.visible(c, self.mode) && (item.leaf() || item.header() > 0.0) {
                push(
                    &mut self.spatial_items,
                    u32::try_from(i).map_err(|_| memory_error())?,
                )?;
            }
        }
        if self.spatial_items.is_empty() {
            return Ok(());
        }
        push(&mut self.spatial, SpatialNode::default())?;
        let mut stack = vec![(0u32, 0usize, self.spatial_items.len())];
        while let Some((node, start, end)) = stack.pop() {
            control.check()?;
            let mut bounds = self.items[self.spatial_items[start] as usize].hit_rect();
            for (i, &index) in self.spatial_items[start + 1..end].iter().enumerate() {
                if i % 512 == 0 {
                    control.check()?;
                }
                bounds = bounds.union(self.items[index as usize].hit_rect());
            }
            if end - start <= SPATIAL_BLOCK {
                self.spatial[node as usize] = SpatialNode {
                    rect: bounds,
                    start: start as u32,
                    count: (end - start) as u32,
                    left: 0,
                    right: 0,
                };
            } else {
                let horizontal = bounds.w * ATLAS_WIDTH as f64 >= bounds.h * ATLAS_HEIGHT as f64;
                let middle = start + (end - start) / 2;
                let items = &self.items;
                self.spatial_items[start..end].select_nth_unstable_by(middle - start, |a, b| {
                    let a = items[*a as usize].hit_rect();
                    let b = items[*b as usize].hit_rect();
                    if horizontal {
                        (a.x + a.w / 2.0).total_cmp(&(b.x + b.w / 2.0))
                    } else {
                        (a.y + a.h / 2.0).total_cmp(&(b.y + b.h / 2.0))
                    }
                });
                let left = push(&mut self.spatial, SpatialNode::default())?;
                let right = push(&mut self.spatial, SpatialNode::default())?;
                self.spatial[node as usize] = SpatialNode {
                    rect: bounds,
                    start: 0,
                    count: 0,
                    left,
                    right,
                };
                stack.push((right, middle, end));
                stack.push((left, start, middle));
            }
        }
        Ok(())
    }
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.comparison_id.capacity()
            + self.items.capacity() * std::mem::size_of::<Item>()
            + self.locations.capacity() * 4
            + self.spatial.capacity() * std::mem::size_of::<SpatialNode>()
            + self.spatial_items.capacity() * 4
            + self.png.capacity()
            + self.labels.capacity() * std::mem::size_of::<TreemapLabel>()
            + self
                .labels
                .iter()
                .map(|l| {
                    l.node_id.capacity()
                        + l.name.capacity()
                        + l.path.capacity()
                        + l.weight.capacity()
                })
                .sum::<usize>()
    }
    pub fn get_bounds(&self, c: &Comparison, node_id: &str) -> Result<Option<TreemapRect>> {
        c.check_id(&self.comparison_id)?;
        let mut node = node_id
            .strip_prefix('n')
            .and_then(|n| n.parse::<NodeId>().ok())
            .filter(|n| c.valid_node(*n))
            .ok_or_else(|| ApiError::new("INVALID_NODE", "节点不存在或已过期"))?;
        if matches!(self.mode, ChartMode::Delta) && values(c.aggregate(node), self.metric)[2] == 0 {
            return Ok(None);
        }
        while node != 0 {
            let index = self.locations[node as usize];
            if index != u32::MAX {
                let item = self.items[index as usize];
                if item.visible(c, self.mode) {
                    return Ok(Some(item.rect.bounds()));
                }
            }
            node = c.node(node).parent;
        }
        Ok(None)
    }
    fn metadata_side(&self, c: &Comparison, item: Item) -> usize {
        match self.mode {
            ChartMode::Before => 0,
            ChartMode::After => 1,
            ChartMode::Delta => usize::from(
                c.entry(item.node, 1)
                    .is_some_and(|e| !item.own() || e.kind == NodeKind::File),
            ),
        }
    }
    pub fn hit_test(&self, c: &Comparison, x: f64, y: f64) -> Result<Option<TreemapHit>> {
        c.check_id(&self.comparison_id)?;
        if !x.is_finite()
            || !y.is_finite()
            || !(0.0..=1.0).contains(&x)
            || !(0.0..=1.0).contains(&y)
        {
            return Err(ApiError::new(
                "INVALID_POSITION",
                "坐标必须是有限的归一化坐标",
            ));
        }
        if self.spatial.is_empty() {
            return Ok(None);
        }
        // Median splitting of u32 item counts has at most 32 levels.
        let mut stack = [0u32; 64];
        let mut length = 1;
        let mut selected: Option<Item> = None;
        while length > 0 {
            length -= 1;
            let b = self.spatial[stack[length] as usize];
            if !b.rect.contains(x, y) {
                continue;
            }
            if b.count == 0 {
                stack[length] = b.left;
                stack[length + 1] = b.right;
                length += 2;
            } else {
                for &index in &self.spatial_items[b.start as usize..(b.start + b.count) as usize] {
                    let item = self.items[index as usize];
                    if item.hit_rect().contains(x, y)
                        && selected.is_none_or(|old| item.node < old.node)
                    {
                        selected = Some(item);
                    }
                }
            }
        }
        let Some(item) = selected else {
            return Ok(None);
        };
        let metadata_side = self.metadata_side(c, item);
        let aggregate = item.aggregate(c);
        let is_directory = !item.own() && directory(c, item.node, self.mode);
        let weights = values(aggregate, self.metric);
        let value = if matches!(self.mode, ChartMode::Delta) {
            (weights[1] as i128 - weights[0] as i128).to_string()
        } else {
            item.weight.to_string()
        };
        let status = if is_directory {
            if aggregate.changes[2] > 0 {
                Status::TypeChanged
            } else if aggregate.changes[3] > 0 {
                Status::Modified
            } else if aggregate.changes[1] > 0 {
                Status::Removed
            } else if aggregate.changes[0] > 0 {
                Status::Added
            } else {
                Status::Unchanged
            }
        } else {
            c.node(item.node).status.clone()
        };
        Ok(Some(TreemapHit {
            node_id: format!("n{}", item.node),
            name: c.name(item.node).to_owned(),
            path: c.path(item.node, metadata_side),
            extension: c.extension(item.node).to_owned(),
            weight: item.weight.to_string(),
            value,
            status,
            kind: if is_directory {
                NodeKind::Directory
            } else {
                NodeKind::File
            },
            collapsed: !item.own() && c.container(item.node) && item.leaf(),
            rect: item.rect.bounds(),
        }))
    }
    pub fn frame(&self, c: &Comparison, layout_id: &str) -> Result<FullTreemapData> {
        c.check_id(&self.comparison_id)?;
        let aggregate = c.aggregate(0);
        let weights = values(aggregate, self.metric);
        let positive = self.positive;
        let negative = self.negative;
        let exported = self.exported;
        let labels = self.labels.clone();
        let rendered = self.rendered;
        let mut image_data_url = String::with_capacity(22 + self.png.len().div_ceil(3) * 4);
        image_data_url.push_str("data:image/png;base64,");
        base64::engine::general_purpose::STANDARD.encode_string(&self.png, &mut image_data_url);
        Ok(FullTreemapData {
            comparison_id: self.comparison_id.clone(),
            layout_id: layout_id.to_owned(),
            image_data_url,
            atlas_width: ATLAS_WIDTH,
            atlas_height: ATLAS_HEIGHT,
            file_count: aggregate.files[side(self.mode)],
            visible_file_count: visible_counts(aggregate, self.metric)[side(self.mode)],
            rendered_block_count: rendered,
            max_depth: self.max_depth,
            added_file_count: aggregate.added,
            weight_total: weights[side(self.mode)].to_string(),
            positive_total: positive.to_string(),
            negative_total: negative.to_string(),
            net_delta: (weights[1] as i128 - weights[0] as i128).to_string(),
            exported_total: exported.to_string(),
            labels,
            warnings: frame_warnings(self.mode),
        })
    }
    fn finish_metadata(&mut self, c: &Comparison, control: &JobControl) -> Result<()> {
        for id in 1..=c.nodes.len() as u32 {
            if id % 512 == 0 {
                control.check()?;
            }
            let own = values(c.own_files(id), self.metric);
            if own[1] >= own[0] {
                self.positive += own[1] - own[0];
            } else {
                self.negative += own[0] - own[1];
            }
        }
        let mut exported = [0u64; 2];
        for (s, source) in [&c.summary.before, &c.summary.after]
            .into_iter()
            .enumerate()
        {
            let total = match self.metric {
                Metric::Size => &source.size,
                Metric::Allocated => &source.allocated,
            };
            exported[s] = total
                .parse()
                .map_err(|_| ApiError::new("AGGREGATE_OVERFLOW", "导出总量无效"))?;
        }
        self.exported = match self.mode {
            ChartMode::Before => exported[0] as i128,
            ChartMode::After => exported[1] as i128,
            ChartMode::Delta => exported[1] as i128 - exported[0] as i128,
        };
        (self.labels, self.rendered) = self.labels(c, control)?;
        control.check()
    }
    fn labels(&self, c: &Comparison, control: &JobControl) -> Result<(Vec<TreemapLabel>, u64)> {
        let mut directories = Vec::<Item>::with_capacity(257);
        let mut files = Vec::<Item>::with_capacity(65);
        let mut rendered = 0;
        for (i, item) in self.items.iter().copied().enumerate() {
            if i % 512 == 0 {
                control.check()?;
            }
            if !item.visible(c, self.mode) {
                continue;
            }
            if item.leaf() {
                rendered += 1;
            }
            let expected_directory = item.header() > 0.0;
            let eligible = expected_directory
                || (item.leaf() && item.rect.w * 4096.0 >= 96.0 && item.rect.h * 1024.0 >= 22.0);
            let actual_directory = if matches!(self.mode, ChartMode::Delta) {
                c.container(item.node)
            } else {
                directory(c, item.node, self.mode)
            };
            if eligible && expected_directory == actual_directory {
                let (list, limit) = if expected_directory {
                    (&mut directories, 256)
                } else {
                    (&mut files, 64)
                };
                list.push(item);
                list.sort_unstable_by(|a, b| {
                    (b.rect.w * b.rect.h)
                        .total_cmp(&(a.rect.w * a.rect.h))
                        .then_with(|| a.node.cmp(&b.node))
                });
                list.truncate(limit);
            }
        }
        let mut labels = Vec::with_capacity(directories.len() + files.len());
        for (kind, list) in [(NodeKind::Directory, directories), (NodeKind::File, files)] {
            for item in list {
                labels.push(TreemapLabel {
                    node_id: format!("n{}", item.node),
                    name: c.name(item.node).to_owned(),
                    path: c.path(item.node, self.metadata_side(c, item)),
                    kind: kind.clone(),
                    weight: item.weight.to_string(),
                    x: item.rect.x,
                    y: item.rect.y,
                    width: item.rect.w,
                    height: if kind == NodeKind::Directory {
                        item.header()
                    } else {
                        item.rect.h
                    },
                });
            }
        }
        Ok((labels, rendered))
    }
    fn paint(&self, c: &Comparison, control: &JobControl, pixels: &mut [u8]) -> Result<()> {
        let mut ticks = 0u64;
        for p in pixels.chunks_exact_mut(4) {
            p.copy_from_slice(&[52, 55, 59, 255]);
        }
        for item in self.items.iter().copied() {
            control.check()?;
            if !item.leaf() || !item.visible(c, self.mode) {
                continue;
            }
            let p = if item.header() > 0.0 {
                content(item.rect, true).0
            } else {
                item.rect
            };
            let p = p.pixels();
            let (x, y, w, h) = (p.x, p.y, p.w, p.h);
            let color = extension_color(if !item.own() && c.container(item.node) {
                ""
            } else {
                c.extension(item.node)
            });
            let x0 = x.floor().max(0.0) as u32;
            let y0 = y.floor().max(0.0) as u32;
            let x1 = (x + w).ceil().max(x0 as f64 + 1.0).min(ATLAS_WIDTH as f64) as u32;
            let y1 = (y + h).ceil().max(y0 as f64 + 1.0).min(ATLAS_HEIGHT as f64) as u32;
            for py in y0..y1 {
                let cy = ((y + h).min(py as f64 + 1.0) - y.max(py as f64))
                    .max(0.0)
                    .min(h);
                for px in x0..x1 {
                    ticks += 1;
                    if ticks % 512 == 0 {
                        control.check()?;
                    }
                    let cx = ((x + w).min(px as f64 + 1.0) - x.max(px as f64))
                        .max(0.0)
                        .min(w);
                    let coverage = (cx * cy).clamp(0.0, 1.0);
                    let u = ((px as f64 + 0.5 - x) / w).clamp(0.0, 1.0);
                    let z = ((py as f64 + 0.5 - y) / h).clamp(0.0, 1.0);
                    let cushion = 0.74 + 0.26 * (4.0 * u * (1.0 - u) * 4.0 * z * (1.0 - z)).sqrt();
                    let edge = if w > 5.0
                        && h > 5.0
                        && (px == x0 || py == y0 || px + 1 == x1 || py + 1 == y1)
                    {
                        0.65
                    } else {
                        1.0
                    };
                    let i = ((py * ATLAS_WIDTH + px) * 4) as usize;
                    for channel in 0..3 {
                        pixels[i + channel] = (pixels[i + channel] as f64 * (1.0 - coverage)
                            + color[channel] as f64 * cushion * edge * coverage)
                            .round() as u8;
                    }
                }
            }
        }
        for item in self.items.iter().copied() {
            control.check()?;
            if item.leaf()
                || !c.node(item.node).expandable
                || !item.visible(c, self.mode)
                || item.rect.w * 4096.0 < 8.0
                || item.rect.h * 1024.0 < 8.0
            {
                continue;
            }
            let (x, y, ex, ey) = pixel_bounds(item.rect);
            if ex > x && ey > y {
                for px in x..ex {
                    darken(pixels, px, y);
                    darken(pixels, px, ey - 1);
                }
                for py in y..ey {
                    darken(pixels, x, py);
                    darken(pixels, ex - 1, py);
                }
            }
        }
        if matches!(self.mode, ChartMode::Delta) {
            for item in self.items.iter().copied() {
                control.check()?;
                let aggregate = item.aggregate(c);
                let weights = values(aggregate, self.metric);
                let mut marks = 0;
                for (i, bit) in [1, 2, 4, 8].into_iter().enumerate() {
                    if aggregate.changes[i] > 0 {
                        marks |= bit;
                    }
                }
                if weights[1] > weights[0] {
                    marks |= 16;
                } else if weights[1] < weights[0] {
                    marks |= 32;
                }
                mark(pixels, item.rect, marks);
            }
        }
        Ok(())
    }
}
fn png_error(error: png::EncodingError) -> ApiError {
    ApiError::new("IMAGE_ERROR", error.to_string())
}
fn frame_warnings(mode: ChartMode) -> Vec<String> {
    let mut warnings=vec!["全局实际文件汇总；目录标题和边距是结构装饰，不另计占用。零权重文件计入数量但不占面积。有限深度下，截断目录包含全部后代文件，选择隐藏文件会定位到可见祖先。".into()];
    if matches!(mode, ChartMode::Delta) {
        warnings.push("差异块按所选指标的增减绝对值重排并铺满全图；新增与删除均有独立面积，目录面积为后代绝对变化之和，净值为零不代表没有变化。所选指标变化为零的项目不占面积。".into());
    }
    warnings
}
fn pixel_bounds(r: BoxRect) -> (u32, u32, u32, u32) {
    (
        (r.x * ATLAS_WIDTH as f64).floor().max(0.0) as u32,
        (r.y * ATLAS_HEIGHT as f64).floor().max(0.0) as u32,
        ((r.x + r.w) * ATLAS_WIDTH as f64)
            .ceil()
            .min(ATLAS_WIDTH as f64) as u32,
        ((r.y + r.h) * ATLAS_HEIGHT as f64)
            .ceil()
            .min(ATLAS_HEIGHT as f64) as u32,
    )
}
fn mark(pixels: &mut [u8], r: BoxRect, marks: u8) {
    let (x, y, ex, ey) = pixel_bounds(r);
    let mut inset = 0;
    for (bit, color, dashed) in [
        (4, [148, 114, 204], false),
        (
            8,
            if marks & 16 != 0 {
                [35, 148, 107]
            } else if marks & 32 != 0 {
                [218, 83, 97]
            } else {
                [225, 183, 80]
            },
            false,
        ),
        (2, [218, 83, 97], true),
        (1, [35, 148, 107], true),
    ] {
        if marks & bit != 0 {
            border(pixels, x, y, ex, ey, inset, color, dashed);
            inset += 8;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn border(
    pixels: &mut [u8],
    x: u32,
    y: u32,
    ex: u32,
    ey: u32,
    inset: u32,
    color: [u8; 3],
    dashed: bool,
) {
    if ex <= x + inset * 2 + 7 || ey <= y + inset * 2 + 7 {
        return;
    }
    let x = x + inset;
    let y = y + inset;
    let ex = ex - inset;
    let ey = ey - inset;
    // Six atlas pixels survive the normal ~4x downscale. Small tiles
    // retain most extension fill beneath change-direction and status markers.
    let thickness = 6.min(((ex - x).min(ey - y) / 6).max(1));
    for layer in 0..thickness {
        for px in x..ex {
            if !dashed || (px - x) / 16 % 2 == 0 {
                for py in [y + layer, ey - 1 - layer] {
                    let i = ((py * ATLAS_WIDTH + px) * 4) as usize;
                    pixels[i..i + 3].copy_from_slice(&color);
                }
            }
        }
        for py in y + thickness..ey - thickness {
            if !dashed || (py - y) / 16 % 2 == 0 {
                for px in [x + layer, ex - 1 - layer] {
                    let i = ((py * ATLAS_WIDTH + px) * 4) as usize;
                    pixels[i..i + 3].copy_from_slice(&color);
                }
            }
        }
    }
}
fn darken(pixels: &mut [u8], x: u32, y: u32) {
    if x < ATLAS_WIDTH && y < ATLAS_HEIGHT {
        let i = ((y * ATLAS_WIDTH + x) * 4) as usize;
        for c in 0..3 {
            pixels[i + c] = (pixels[i + c] as u16 * 3 / 4) as u8;
        }
    }
}
