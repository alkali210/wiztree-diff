use base64::Engine;
use std::{cell::RefCell, collections::HashMap, ops::Deref};
use wiztree_diff_lib::{
    global_treemap::TreemapLayout,
    import::{self, JobControl},
    store::{self, Comparison},
    types::{
        ChartMode, FullTreemapData, Metric, NodeKind, Result, Status, TreemapHit, TreemapRect,
    },
};
// Tests explicitly retain requested layouts to compare independently owned views.
// Production keeps only its active layout; there is no comparison-owned view cache.
struct TestComparison {
    data: Comparison,
    layouts: RefCell<HashMap<(u8, u32), TreemapLayout>>,
}
impl Deref for TestComparison {
    type Target = Comparison;
    fn deref(&self) -> &Comparison {
        &self.data
    }
}
impl TestComparison {
    fn with_layout<T>(
        &self,
        metric: Metric,
        mode: ChartMode,
        depth: u32,
        f: impl FnOnce(&TreemapLayout) -> Result<T>,
    ) -> Result<T> {
        let key = (
            match metric {
                Metric::Size => 0,
                Metric::Allocated => 3,
            } + match mode {
                ChartMode::Before => 0,
                ChartMode::After => 1,
                ChartMode::Delta => 2,
            },
            depth,
        );
        let mut layouts = self.layouts.borrow_mut();
        if !layouts.contains_key(&key) {
            layouts.insert(
                key,
                TreemapLayout::build(&self.data, metric, mode, depth, &JobControl::default())?,
            );
        }
        f(&layouts[&key])
    }
}
fn request_frame(
    c: &TestComparison,
    metric: Metric,
    mode: ChartMode,
    depth: u32,
) -> Result<FullTreemapData> {
    c.with_layout(metric, mode, depth, |layout| layout.frame(c, "test-layout"))
}
fn request_bounds(
    c: &TestComparison,
    metric: Metric,
    mode: ChartMode,
    id: &str,
    depth: u32,
) -> Result<Option<TreemapRect>> {
    c.with_layout(metric, mode, depth, |layout| layout.get_bounds(c, id))
}
fn request_hit(
    c: &TestComparison,
    metric: Metric,
    mode: ChartMode,
    x: f64,
    y: f64,
    depth: u32,
) -> Result<Option<TreemapHit>> {
    c.with_layout(metric, mode, depth, |layout| layout.hit_test(c, x, y))
}
fn build(before: &str, after: &str) -> (tempfile::TempDir, TestComparison) {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("before.csv");
    let a = dir.path().join("after.csv");
    std::fs::write(&b, before).unwrap();
    std::fs::write(&a, after).unwrap();
    let data = store::build_comparison(&b, &a, "treemap-test", &JobControl::default(), &mut |_| {})
        .unwrap();
    (
        dir,
        TestComparison {
            data,
            layouts: RefCell::new(HashMap::new()),
        },
    )
}
fn node(c: &Comparison, path: &str) -> String {
    let normalized = import::normalize(path).0;
    let id = (1..=c.nodes.len() as u32)
        .find(|id| c.canonical_path(*id) == normalized)
        .unwrap();
    format!("n{id}")
}
// Expected leaves come from snapshot records; geometry is observed via public methods.
fn file_rects(
    c: &TestComparison,
    metric: Metric,
    mode: ChartMode,
) -> Vec<(i64, f64, f64, f64, f64, i64)> {
    let side = usize::from(matches!(mode, ChartMode::After));
    (1..=c.nodes.len() as u32)
        .filter_map(|id| {
            let entry = c.entry(id, side)?;
            let weight = match metric {
                Metric::Size => entry.size,
                Metric::Allocated => entry.allocated,
            };
            if entry.kind != NodeKind::File
                || weight == 0
                || (matches!(mode, ChartMode::Delta) && c.node(id).status == Status::Unchanged)
            {
                return None;
            }
            let b = request_bounds(c, metric, mode, &format!("n{id}"), 0)
                .unwrap()
                .unwrap();
            Some((id as i64, b.x, b.y, b.width, b.height, weight as i64))
        })
        .collect()
}
#[test]
fn appending_other_views_preserves_precise_hits_in_existing_maps() {
    let mut before = String::from("文件名称,大小,分配\n");
    let mut after = before.clone();
    for i in 0..1027 {
        before.push_str(&format!("C:\\file{i}.bin,{},{:}\n", i % 17 + 1, i % 23 + 1));
        after.push_str(&format!(
            "C:\\file{i}.bin,{},{:}\n",
            i % 11 + 101,
            i % 29 + 101
        ));
    }
    let (_dir, c) = build(&before, &after);
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::After, ChartMode::Before, ChartMode::Delta] {
            let frame = request_frame(&c, metric, mode, 0).unwrap();
            assert_eq!(frame.rendered_block_count, 1027);
        }
    }
    // Independently owned requested layouts must not replace earlier hit candidates.
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::After, ChartMode::Before, ChartMode::Delta] {
            for i in [0, 19, 300, 700, 1026] {
                let id = node(&c, &format!("C:\\file{i}.bin"));
                let r = request_bounds(&c, metric, mode, &id, 0).unwrap().unwrap();
                let hit = request_hit(
                    &c,
                    metric,
                    mode,
                    r.x + r.width / 2.0,
                    r.y + r.height / 2.0,
                    0,
                )
                .unwrap()
                .unwrap();
                assert_eq!(hit.node_id, id);
            }
        }
    }
}

#[test]
fn sparse_opposite_snapshot_children_keep_the_real_file_ancestor_and_all_leaves() {
    let b = "文件名称,大小,分配\nC:\\swap,12,16\n";
    let a = "文件名称,大小,分配\nC:\\swap\\child.bin,12,16\n";
    let (_dir, c) = build(b, a);
    for (mode, path) in [
        (ChartMode::Before, "C:\\swap"),
        (ChartMode::After, "C:\\swap\\child.bin"),
    ] {
        let frame = request_frame(&c, Metric::Size, mode, 0).unwrap();
        assert_eq!(frame.weight_total, "12");
        assert_eq!(
            (frame.visible_file_count, frame.rendered_block_count),
            (1, 1)
        );
        let id = node(&c, path);
        let rect = request_bounds(&c, Metric::Size, mode, &id, 0)
            .unwrap()
            .unwrap();
        let hit = request_hit(
            &c,
            Metric::Size,
            mode,
            rect.x + rect.width / 2.0,
            rect.y + rect.height / 2.0,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.node_id, id);
        assert!(!hit.collapsed);
    }
    let d = request_frame(&c, Metric::Size, ChartMode::Delta, 0).unwrap();
    assert_eq!(
        (
            d.weight_total.as_str(),
            d.visible_file_count,
            d.rendered_block_count
        ),
        ("24", 2, 2)
    );
    let child = node(&c, "C:\\swap\\child.bin");
    let rect = request_bounds(&c, Metric::Size, ChartMode::Delta, &child, 0)
        .unwrap()
        .unwrap();
    let hit = request_hit(
        &c,
        Metric::Size,
        ChartMode::Delta,
        rect.x + rect.width / 2.0,
        rect.y + rect.height / 2.0,
        0,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        (
            hit.node_id.as_str(),
            hit.weight.as_str(),
            hit.value.as_str()
        ),
        (child.as_str(), "12", "12")
    );
    // A file-only union parent is structural, not an exported directory.
    // Depth one must not invent a directory header or hide its opposite-side child.
    let limited = request_frame(&c, Metric::Size, ChartMode::Delta, 1).unwrap();
    assert_eq!(
        (limited.weight_total.as_str(), limited.rendered_block_count),
        ("24", 2)
    );
    assert!(limited
        .labels
        .iter()
        .all(|label| label.kind == NodeKind::File));
    let rect = request_bounds(&c, Metric::Size, ChartMode::Delta, &child, 1)
        .unwrap()
        .unwrap();
    let hit = request_hit(
        &c,
        Metric::Size,
        ChartMode::Delta,
        rect.x + rect.width / 2.0,
        rect.y + rect.height / 2.0,
        1,
    )
    .unwrap()
    .unwrap();
    assert_eq!(hit.node_id, child);
    assert!(!hit.collapsed);
}

#[test]
fn nested_directory_headers_select_real_ancestors_without_occluding_files() {
    use wiztree_diff_lib::types::NodeKind;
    let csv="文件名称,大小,分配\nC:\\r\\,999,999\nC:\\r\\root.txt,20,20\nC:\\r\\A\\,999,999\nC:\\r\\A\\a.txt,20,20\nC:\\r\\A\\sub\\,999,999\nC:\\r\\A\\sub\\one.bin,40,40\nC:\\r\\A\\sub\\two.py,40,40\nC:\\r\\B\\,999,999\nC:\\r\\B\\big.dll,80,80\nC:\\r\\B\\small.jpg,40,40\n";
    let (_dir, c) = build(csv, csv);
    let frame = request_frame(&c, Metric::Size, ChartMode::Before, 0).unwrap();
    assert_eq!(
        (
            frame.file_count,
            frame.visible_file_count,
            frame.rendered_block_count
        ),
        (6, 6, 6)
    );
    assert_eq!(frame.weight_total, "240");
    for (path, weight) in [
        ("C:\\r", "240"),
        ("C:\\r\\A", "100"),
        ("C:\\r\\A\\sub", "80"),
        ("C:\\r\\B", "120"),
    ] {
        let id = node(&c, path);
        let label = frame
            .labels
            .iter()
            .find(|l| l.node_id == id && l.kind == NodeKind::Directory)
            .unwrap();
        assert_eq!(label.weight, weight);
        let hit = request_hit(
            &c,
            Metric::Size,
            ChartMode::Before,
            label.x + label.width / 2.0,
            label.y + label.height / 2.0,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.node_id, id);
        assert_eq!(hit.kind, NodeKind::Directory);
        assert!(!hit.collapsed);
        assert_eq!(hit.weight, weight);
        assert!(hit.rect.height > label.height);
    }
    for path in [
        "C:\\r\\root.txt",
        "C:\\r\\A\\a.txt",
        "C:\\r\\A\\sub\\one.bin",
        "C:\\r\\A\\sub\\two.py",
        "C:\\r\\B\\big.dll",
        "C:\\r\\B\\small.jpg",
    ] {
        let id = node(&c, path);
        let r = request_bounds(&c, Metric::Size, ChartMode::Before, &id, 0)
            .unwrap()
            .unwrap();
        assert!(r.width > 0.0 && r.height > 0.0);
        let parent = path.rsplit_once('\\').unwrap().0;
        let p = frame
            .labels
            .iter()
            .find(|l| l.node_id == node(&c, parent))
            .unwrap();
        assert!(r.y >= p.y + p.height - 1e-12);
        let hit = request_hit(
            &c,
            Metric::Size,
            ChartMode::Before,
            r.x + r.width / 2.0,
            r.y + r.height / 2.0,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.node_id, id);
        assert_eq!(hit.kind, NodeKind::File);
    }
    let limited = request_frame(&c, Metric::Size, ChartMode::After, 1).unwrap();
    let header = limited
        .labels
        .iter()
        .find(|l| l.kind == NodeKind::Directory)
        .unwrap();
    let hit = request_hit(
        &c,
        Metric::Size,
        ChartMode::After,
        header.x + header.width / 2.0,
        header.y + header.height / 2.0,
        1,
    )
    .unwrap()
    .unwrap();
    assert!(hit.collapsed);
    assert_eq!((hit.node_id, hit.weight), (node(&c, "C:\\r"), "240".into()));
}

#[test]
fn unlimited_deep_chain_keeps_the_leaf_after_title_space_becomes_unreadable() {
    use wiztree_diff_lib::types::NodeKind;
    let mut csv = String::from("文件名称,大小,分配\n");
    let mut path = String::from("C:\\r");
    for _ in 0..2048 {
        csv.push_str(&format!("{path}\\,1,1\n"));
        path.push_str("\\sub");
    }
    let file = format!("{path}\\deep.bin");
    // The last actual directory exists, not an orphan file shortcut.
    csv.push_str(&format!("{path}\\,1,1\n{file},1,1\n"));
    let (_dir, c) = build(&csv, &csv);
    let frame = request_frame(&c, Metric::Size, ChartMode::After, 0).unwrap();
    assert_eq!((frame.file_count, frame.rendered_block_count), (1, 1));
    assert_eq!(frame.weight_total, "1");
    let labels = frame
        .labels
        .iter()
        .filter(|l| l.kind == NodeKind::Directory)
        .count();
    assert!(labels > 1 && labels < 81);
    let bounds = request_bounds(&c, Metric::Size, ChartMode::After, &node(&c, &file), 0)
        .unwrap()
        .unwrap();
    assert!(bounds.width > 0.0 && bounds.height > 0.0);
    let hit = request_hit(
        &c,
        Metric::Size,
        ChartMode::After,
        bounds.x + bounds.width / 2.0,
        bounds.y + bounds.height / 2.0,
        0,
    )
    .unwrap()
    .unwrap();
    assert_eq!(hit.node_id, node(&c, &file));
    assert_eq!(hit.kind, NodeKind::File);
}
fn fixture() -> (String, String) {
    let h = "文件名称,大小,分配,MFTRECNO\n";
    let mut b=format!("{h}C:\\root\\,999999,999999\nC:\\root\\sub\\,999999,999999\nC:\\root\\sub\\same.TXT,15,16,42\nC:\\root\\sub\\hardlink.txt,15,16,42\nC:\\root\\zero,0,4\nC:\\root\\swap,31,32\nC:\\root\\removed.bin,7,8\nC:\\root\\gap\\lost.py,9,12\nZ:\\detached\\big.dat,9007199254740993,9007199254740993\n");
    let mut a=format!("{h}C:\\root\\,888888,888888\nC:\\root\\sub\\,888888,888888\nC:\\root\\sub\\same.TXT,15,16,42\nC:\\root\\sub\\hardlink.txt,15,16,42\nC:\\root\\zero,0,4\nC:\\root\\swap\\,600,600\nC:\\root\\swap\\child.mp3,17,20\nC:\\root\\added.bin,13,16\nC:\\root\\gap\\lost.py,9,12\nZ:\\detached\\big.dat,9007199254741000,9007199254741001\n");
    for i in 0..600 {
        b.push_str(&format!("C:\\root\\f{i:04}.x{i},1,2\n"));
        a.push_str(&format!("C:\\root\\f{i:04}.x{i},2,4\n"));
    }
    (b, a)
}
#[test]
fn unlimited_global_baseline_views_preserve_integer_totals_hierarchy_and_ownership() {
    let (b, a) = fixture();
    let (_dir, c) = build(&b, &a);
    let expected = [
        (
            Metric::Size,
            ChartMode::Before,
            607,
            606,
            9007199254741670i64,
        ),
        (Metric::Size, ChartMode::After, 607, 606, 9007199254742269),
        (Metric::Size, ChartMode::Delta, 609, 605, 675),
        (
            Metric::Allocated,
            ChartMode::Before,
            607,
            607,
            9007199254742281,
        ),
        (
            Metric::Allocated,
            ChartMode::After,
            607,
            607,
            9007199254743485,
        ),
        (Metric::Allocated, ChartMode::Delta, 609, 605, 1284),
    ];
    for (metric, mode, files, visible, weight) in expected {
        let frame = request_frame(&c, metric, mode, 0).unwrap();
        assert_eq!(
            (frame.file_count, frame.visible_file_count),
            (files, visible)
        );
        assert_eq!(frame.weight_total, weight.to_string());
        assert!(frame.labels.len() <= 320);
        if matches!(mode, ChartMode::Delta) {
            for i in [0, 317, 599] {
                let id = node(&c, &format!("C:\\root\\f{i:04}.x{i}"));
                let r = request_bounds(&c, metric, mode, &id, 0).unwrap().unwrap();
                let hit = request_hit(
                    &c,
                    metric,
                    mode,
                    r.x + r.width / 2.0,
                    r.y + r.height / 2.0,
                    0,
                )
                .unwrap()
                .unwrap();
                assert_eq!(hit.node_id, id);
                assert_eq!(
                    hit.weight,
                    if matches!(metric, Metric::Size) {
                        "1"
                    } else {
                        "2"
                    }
                );
            }
            continue;
        }
        let rects = file_rects(&c, metric, mode);
        assert_eq!(rects.len() as u64, visible);
        if !matches!(mode, ChartMode::Delta) {
            assert_eq!(rects.iter().map(|r| r.5).sum::<i64>(), weight);
        }
        for &(id, x, y, w, h, file_weight) in &rects {
            assert!(w > 0.0 && h > 0.0, "positive file lost area: {id}");
            assert!(x >= -1e-12 && y >= -1e-12 && x + w <= 1.0 + 1e-10 && y + h <= 1.0 + 1e-10);
            // Very small geometries remain available even when no f64 interior
            // coordinate or atlas pixel can represent their separation.
            if w > 1e-10 && h > 1e-10 {
                let hit = request_hit(&c, metric, mode, x + w * 0.5, y + h * 0.5, 0)
                    .unwrap()
                    .unwrap();
                assert_eq!(hit.node_id, format!("n{id}"));
                assert_eq!(hit.weight, file_weight.to_string());
            }
        }
        // Header/gutter space is decoration. Proportions remain exact within
        // each parent content area rather than the entire image rectangle.
        for &(id, x, y, w, h, file_weight) in &rects {
            let parent = c.node(id as u32).parent;
            let parent = if parent == 0 { None } else { Some(parent) };
            if let Some(parent) = parent {
                let pid = format!("n{parent}");
                let p = request_bounds(&c, metric, mode, &pid, 0).unwrap().unwrap();
                let header = frame
                    .labels
                    .iter()
                    .find(|l| {
                        l.node_id == pid && l.kind == wiztree_diff_lib::types::NodeKind::Directory
                    })
                    .map_or(0.0, |l| l.height);
                let side = if matches!(mode, ChartMode::After) {
                    1
                } else {
                    0
                };
                let aggregate = c.aggregate(parent);
                let parent_weight = match metric {
                    Metric::Size => aggregate.size[side],
                    Metric::Allocated => aggregate.allocated[side],
                };
                let content = (p.width - if header > 0.0 { 16.0 / 4096.0 } else { 0.0 })
                    * (p.height - header - if header > 0.0 { 8.0 / 1024.0 } else { 0.0 });
                assert!(
                    (w * h / content - file_weight as f64 / parent_weight as f64).abs() < 1e-10
                );
                assert!(
                    x >= p.x - 1e-10
                        && y >= p.y - 1e-10
                        && x + w <= p.x + p.width + 1e-10
                        && y + h <= p.y + p.height + 1e-10
                );
            }
        }
        // Nonoverlap uses strict interior overlap, tolerating floating boundary rounding.
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let overlap_x = (a.1 + a.3).min(b.1 + b.3) - a.1.max(b.1);
                let overlap_y = (a.2 + a.4).min(b.2 + b.4) - a.2.max(b.2);
                assert!(
                    overlap_x <= 1e-12 || overlap_y <= 1e-12,
                    "overlap {} {}",
                    a.0,
                    b.0
                );
            }
        }
    }
    let delta = request_frame(&c, Metric::Size, ChartMode::Delta, 0).unwrap();
    assert_eq!(
        (
            &delta.positive_total,
            &delta.negative_total,
            &delta.net_delta
        ),
        (&"637".to_owned(), &"38".to_owned(), &"599".to_owned())
    );
    let allocated = request_frame(&c, Metric::Allocated, ChartMode::Delta, 0).unwrap();
    assert_eq!(
        (
            allocated.positive_total,
            allocated.negative_total,
            allocated.net_delta
        ),
        ("1244".into(), "40".into(), "1204".into())
    );
    let orphan = node(&c, "C:\\root\\gap\\lost.py");
    assert_eq!(
        wiztree_diff_lib::diff::get_details(&c, &orphan)
            .unwrap()
            .parent_id,
        None
    );
    assert!(request_bounds(
        &c,
        Metric::Size,
        ChartMode::After,
        &node(&c, "C:\\root\\zero"),
        0
    )
    .unwrap()
    .is_some());
    assert!(request_bounds(
        &c,
        Metric::Allocated,
        ChartMode::After,
        &node(&c, "C:\\root\\zero"),
        0
    )
    .unwrap()
    .is_some());
    for (x, y) in [
        (f64::NAN, 0.0),
        (0.0, f64::INFINITY),
        (-0.1, 0.0),
        (0.0, 1.01),
    ] {
        assert_eq!(
            request_hit(&c, Metric::Size, ChartMode::After, x, y, 0)
                .unwrap_err()
                .code,
            "INVALID_POSITION"
        );
    }
    assert_eq!(
        request_bounds(&c, Metric::Size, ChartMode::After, "bad", 0)
            .unwrap_err()
            .code,
        "INVALID_NODE"
    );
    assert_eq!(
        request_bounds(&c, Metric::Size, ChartMode::After, "n99999999", 0)
            .unwrap_err()
            .code,
        "INVALID_NODE"
    );
}

#[test]
fn zero_and_empty_frames_keep_counts_and_exported_directory_values_without_fake_tiles() {
    let b = "文件名称,大小,分配\nC:\\root\\,123,456\nC:\\root\\zero.bin,0,0\n";
    let (_dir, c) = build(b, b);
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
            let f = request_frame(&c, metric, mode, 0).unwrap();
            assert_eq!((f.file_count, f.visible_file_count), (1, 0));
            assert_eq!(f.weight_total, "0");
            assert!(request_hit(&c, metric, mode, 0.5, 0.5, 0)
                .unwrap()
                .is_none());
            let exported = match mode {
                ChartMode::Delta => "0",
                _ => match metric {
                    Metric::Size => "123",
                    Metric::Allocated => "456",
                },
            };
            assert_eq!(f.exported_total, exported);
        }
    }
}

#[test]
fn equal_siblings_have_deterministic_shared_edge_and_outer_edge_hits() {
    let csv = "文件名称,大小,分配\nC:\\a.bin,1,1\nC:\\b.bin,1,1\n";
    let (_dir, c) = build(csv, csv);
    let mut r = file_rects(&c, Metric::Size, ChartMode::After);
    r.sort_by(|a, b| a.1.total_cmp(&b.1));
    let right = &r[1];
    let edge = request_hit(&c, Metric::Size, ChartMode::After, right.1, 0.5, 0)
        .unwrap()
        .unwrap();
    assert_eq!(edge.node_id, format!("n{}", right.0));
    let outer = request_hit(&c, Metric::Size, ChartMode::After, 1.0, 1.0, 0)
        .unwrap()
        .unwrap();
    assert_eq!(outer.node_id, format!("n{}", right.0));
}

#[test]
fn leaf_aggregate_overflow_and_precancelled_layout_fail_explicitly() {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("before.csv");
    let a = dir.path().join("after.csv");
    std::fs::write(
        &b,
        format!(
            "文件名称,大小,分配\nC:\\1.bin,{},0\nC:\\2.bin,{},0\n",
            i64::MAX,
            i64::MAX
        ),
    )
    .unwrap();
    std::fs::write(&a, "文件名称,大小,分配\nC:\\zero.bin,0,0\n").unwrap();
    let error = store::build_comparison(&b, &a, "overflow", &JobControl::default(), &mut |_| {})
        .err()
        .unwrap();
    assert_eq!(error.code, "AGGREGATE_OVERFLOW");
    let (_dir, c) = build(
        "文件名称,大小,分配\nC:\\one.bin,1,1\n",
        "文件名称,大小,分配\nC:\\zero.bin,0,0\n",
    );
    let control = JobControl::default();
    control.cancel();
    assert_eq!(
        TreemapLayout::build(&c, Metric::Size, ChartMode::Before, 0, &control)
            .err()
            .unwrap()
            .code,
        "CANCELLED"
    );
}

#[test]
fn hundred_thousand_actual_parents_and_sparse_roots_are_not_sampled_or_capped() {
    let mut csv = String::from("文件名称,大小,分配\n");
    for i in 0..100001 {
        csv.push_str(&format!(
            "C:\\actual{i}\\,1000000,1000000\nC:\\actual{i}\\f.bin,1,2\n"
        ));
    }
    let (_dir, c) = build(&csv, &csv);
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
            let f = request_frame(&c, metric, mode, 0).unwrap();
            assert_eq!(f.file_count, 100001);
            assert_eq!(
                f.visible_file_count,
                if matches!(mode, ChartMode::Delta) {
                    0
                } else {
                    100001
                }
            );
            assert_eq!(
                f.weight_total,
                if matches!(mode, ChartMode::Delta) {
                    "0"
                } else if matches!(metric, Metric::Size) {
                    "100001"
                } else {
                    "200002"
                }
            );
        }
    }
    let frame = request_frame(&c, Metric::Size, ChartMode::After, 0).unwrap();
    assert_eq!(frame.rendered_block_count, 100001);
    for i in [0, 50000, 100000] {
        let id = node(&c, &format!("C:\\actual{i}\\f.bin"));
        let r = request_bounds(&c, Metric::Size, ChartMode::After, &id, 0)
            .unwrap()
            .unwrap();
        let hit = request_hit(
            &c,
            Metric::Size,
            ChartMode::After,
            r.x + r.width / 2.0,
            r.y + r.height / 2.0,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.node_id, id);
    }
}

#[test]
fn active_job_cancellation_leaves_owned_layout_unchanged() {
    let mut csv = String::from("文件名称,大小,分配\n");
    for i in 0..10000 {
        csv.push_str(&format!("C:\\f{i}.bin,1,2\n"));
    }
    let (_dir, c) = build(&csv, "文件名称,大小,分配\nC:\\after.bin,0,0\n");
    let old = TreemapLayout::build(
        &c,
        Metric::Size,
        ChartMode::Before,
        0,
        &JobControl::default(),
    )
    .unwrap();
    let initial = old.frame(&c, "old").unwrap();
    let control = JobControl::default();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(1));
            control.cancel();
        });
        let error = TreemapLayout::build(&c, Metric::Allocated, ChartMode::Before, 0, &control)
            .err()
            .unwrap();
        assert_eq!(error.code, "CANCELLED");
    });
    let unchanged = old.frame(&c, "old").unwrap();
    assert_eq!(unchanged.image_data_url, initial.image_data_url);
    assert_eq!(unchanged.file_count, 10000);
    assert_eq!(unchanged.rendered_block_count, 10000);
    assert_eq!(unchanged.weight_total, "10000");
}

fn rect_tuple(r: &wiztree_diff_lib::types::TreemapRect) -> (f64, f64, f64, f64) {
    (r.x, r.y, r.width, r.height)
}
fn pixels(frame: &wiztree_diff_lib::types::FullTreemapData) -> Vec<u8> {
    let png = base64::engine::general_purpose::STANDARD
        .decode(
            frame
                .image_data_url
                .strip_prefix("data:image/png;base64,")
                .unwrap(),
        )
        .unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .unwrap();
    let mut bytes = vec![0; reader.output_buffer_size()];
    reader.next_frame(&mut bytes).unwrap();
    bytes
}

#[test]
fn real_rust_depth_smoke_preserves_all_leaf_areas_and_cancelling_descendant_markers() {
    let h = "文件名称,大小,分配\n";
    let b=format!("{h}C:\\deep\\export\\,999,999\nC:\\deep\\export\\sub\\,777,777\nC:\\deep\\export\\sub\\deep\\,666,666\nC:\\deep\\export\\sub\\deep\\more\\,555,555\nC:\\deep\\export\\root.txt,10,10\nC:\\deep\\export\\sub\\keep.bin,20,20\nC:\\deep\\export\\sub\\deep\\grow.txt,10,10\nC:\\deep\\export\\sub\\deep\\removed.bin,20,20\nC:\\deep\\export\\sub\\deep\\more\\keep.mp3,40,40\n");
    let a = b
        .replace("grow.txt,10,10", "grow.txt,20,20")
        .replace("removed.bin,20,20", "added.py,10,10");
    let (_dir, c) = build(&b, &a);
    let root = node(&c, "C:\\deep\\export");
    let deep = node(&c, "C:\\deep\\export\\sub\\deep");
    let hidden = node(&c, "C:\\deep\\export\\sub\\deep\\grow.txt");
    for metric in [Metric::Size, Metric::Allocated] {
        for depth in [1, 3, 0] {
            let before = request_frame(&c, metric, ChartMode::Before, depth).unwrap();
            let delta = request_frame(&c, metric, ChartMode::Delta, depth).unwrap();
            let after = request_frame(&c, metric, ChartMode::After, depth).unwrap();
            assert_eq!(before.max_depth, depth);
            assert_eq!((before.file_count, before.visible_file_count), (5, 5));
            assert_eq!((delta.file_count, delta.visible_file_count), (6, 3));
            assert_eq!(delta.added_file_count, 1);
            assert_eq!(
                (
                    before.weight_total.as_str(),
                    delta.weight_total.as_str(),
                    after.weight_total.as_str()
                ),
                ("100", "40", "100")
            );
            assert_eq!(
                (
                    delta.positive_total.as_str(),
                    delta.negative_total.as_str(),
                    delta.net_delta.as_str()
                ),
                ("20", "20", "0")
            );
            assert_eq!(
                before.rendered_block_count,
                match depth {
                    1 => 1,
                    3 => 3,
                    _ => 5,
                }
            );
            assert_eq!(delta.rendered_block_count, if depth == 0 { 3 } else { 1 });
            for path in [
                "C:\\deep\\export\\root.txt",
                "C:\\deep\\export\\sub\\keep.bin",
                "C:\\deep\\export\\sub\\deep\\grow.txt",
                "C:\\deep\\export\\sub\\deep\\removed.bin",
                "C:\\deep\\export\\sub\\deep\\more\\keep.mp3",
            ] {
                let id = node(&c, path);
                assert!(request_bounds(&c, metric, ChartMode::Before, &id, depth)
                    .unwrap()
                    .is_some());
                let d = request_bounds(&c, metric, ChartMode::Delta, &id, depth).unwrap();
                if path.ends_with("grow.txt") || path.ends_with("removed.bin") {
                    let r = d.unwrap();
                    assert!(r.width * r.height > 0.0);
                } else {
                    assert!(d.is_none());
                }
            }
            let highlight = request_bounds(&c, metric, ChartMode::Delta, &hidden, depth)
                .unwrap()
                .unwrap();
            if depth != 0 {
                let ancestor = request_bounds(
                    &c,
                    metric,
                    ChartMode::Delta,
                    if depth == 1 { &root } else { &deep },
                    depth,
                )
                .unwrap()
                .unwrap();
                assert_eq!(rect_tuple(&highlight), rect_tuple(&ancestor));
                let hit = request_hit(
                    &c,
                    metric,
                    ChartMode::Delta,
                    highlight.x + highlight.width / 2.0,
                    highlight.y + highlight.height / 2.0,
                    depth,
                )
                .unwrap()
                .unwrap();
                assert_eq!(
                    hit.node_id,
                    if depth == 1 {
                        root.clone()
                    } else {
                        deep.clone()
                    }
                );
                assert_eq!(hit.kind, wiztree_diff_lib::types::NodeKind::Directory);
                assert!(hit.collapsed);
                assert_eq!(hit.status, Status::Modified);
                assert_eq!(hit.value, "0");
                let rgba = pixels(&delta);
                for rgb in [[35, 148, 107], [218, 83, 97], [225, 183, 80]] {
                    assert!(rgba.chunks_exact(4).any(|p| p[..3] == rgb));
                }
            } else {
                let hit = request_hit(
                    &c,
                    metric,
                    ChartMode::Delta,
                    highlight.x + highlight.width / 2.0,
                    highlight.y + highlight.height / 2.0,
                    depth,
                )
                .unwrap()
                .unwrap();
                assert_eq!(hit.node_id, hidden);
                assert!(!hit.collapsed);
            }
        }
    }
}

#[test]
fn unchanged_self_comparison_has_no_delta_blocks_or_hits_at_any_depth() {
    let csv="文件名称,大小,分配\nC:\\r\\,20,20\nC:\\r\\sub\\,20,20\nC:\\r\\sub\\a.txt,8,8\nC:\\r\\sub\\b.mp3,12,12\n";
    let (_dir, c) = build(csv, csv);
    let id = node(&c, "C:\\r\\sub\\a.txt");
    for metric in [Metric::Size, Metric::Allocated] {
        for depth in [1, 3, 0] {
            let before = request_frame(&c, metric, ChartMode::Before, depth).unwrap();
            let delta = request_frame(&c, metric, ChartMode::Delta, depth).unwrap();
            assert_eq!(
                (
                    delta.file_count,
                    delta.visible_file_count,
                    delta.rendered_block_count
                ),
                (2, 0, 0)
            );
            assert_eq!(delta.weight_total, "0");
            assert_eq!(delta.net_delta, "0");
            assert!(delta.labels.is_empty());
            let rgba = pixels(&delta);
            assert!(rgba.chunks_exact(4).all(|p| p == &rgba[..4]));
            assert_ne!(before.image_data_url, delta.image_data_url);
            let r = request_bounds(&c, metric, ChartMode::Before, &id, depth)
                .unwrap()
                .unwrap();
            assert!(request_bounds(&c, metric, ChartMode::Delta, &id, depth)
                .unwrap()
                .is_none());
            assert!(request_hit(
                &c,
                metric,
                ChartMode::Delta,
                r.x + r.width / 2.0,
                r.y + r.height / 2.0,
                depth
            )
            .unwrap()
            .is_none());
        }
    }
}

#[test]
fn new_only_additions_receive_their_own_delta_area_even_without_before_roots() {
    let h = "文件名称,大小,分配\n";
    let b = format!("{h}C:\\r\\,10,10\nC:\\r\\old.txt,10,10\n");
    let mut a = format!("{b}C:\\r\\new.txt,2,2\n");
    for i in 0..20 {
        a.push_str(&format!("Z:\\new{i}.bin,1,1\n"));
    }
    let (_dir, c) = build(&b, &a);
    for depth in [1, 3, 0] {
        let d = request_frame(&c, Metric::Size, ChartMode::Delta, depth).unwrap();
        assert_eq!(d.weight_total, "22");
        assert_eq!(d.added_file_count, 21);
        for path in ["C:\\r\\new.txt", "Z:\\new0.bin", "Z:\\new19.bin"] {
            let id = node(&c, path);
            let rect = request_bounds(&c, Metric::Size, ChartMode::Delta, &id, depth)
                .unwrap()
                .unwrap();
            let hit = request_hit(
                &c,
                Metric::Size,
                ChartMode::Delta,
                rect.x + rect.width / 2.0,
                rect.y + rect.height / 2.0,
                depth,
            )
            .unwrap()
            .unwrap();
            if depth != 1 || path.starts_with("Z:") {
                assert_eq!(hit.node_id, id);
            }
        }
    }
    let empty_before = format!("{h}Y:\\empty\\,0,0\n");
    let (_dir, c) = build(&empty_before, &a);
    let d = request_frame(&c, Metric::Size, ChartMode::Delta, 1).unwrap();
    assert_eq!(d.weight_total, "32");
    assert_eq!(d.rendered_block_count, 21);
    assert_eq!(d.added_file_count, 22);
    assert!(request_bounds(
        &c,
        Metric::Size,
        ChartMode::Delta,
        &node(&c, "Z:\\new0.bin"),
        1
    )
    .unwrap()
    .is_some());
}

#[test]
fn type_changes_keep_both_removed_and_added_leaves_in_delta() {
    let b="文件名称,大小,分配\nC:\\r\\,20,20\nC:\\r\\swap,12,12\nC:\\r\\reverse\\,8,8\nC:\\r\\reverse\\child.bin,8,8\n";
    let a="文件名称,大小,分配\nC:\\r\\,20,20\nC:\\r\\swap\\,12,12\nC:\\r\\swap\\child.txt,12,12\nC:\\r\\reverse,8,8\n";
    let (_dir, c) = build(b, a);
    for depth in [3, 0] {
        let d = request_frame(&c, Metric::Size, ChartMode::Delta, depth).unwrap();
        assert_eq!(d.weight_total, "40");
        assert_eq!(d.rendered_block_count, 4);
        let swap = node(&c, "C:\\r\\swap");
        let header = d
            .labels
            .iter()
            .find(|l| {
                l.node_id == swap && matches!(l.kind, wiztree_diff_lib::types::NodeKind::Directory)
            })
            .unwrap();
        let hit = request_hit(
            &c,
            Metric::Size,
            ChartMode::Delta,
            header.x + header.width / 2.0,
            header.y + header.height / 2.0,
            depth,
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            (hit.node_id, hit.status, hit.kind, hit.weight, hit.value),
            (
                swap,
                Status::TypeChanged,
                wiztree_diff_lib::types::NodeKind::Directory,
                "24".into(),
                "0".into()
            )
        );
        for (path, value) in [
            ("C:\\r\\swap\\child.txt", "12"),
            ("C:\\r\\reverse\\child.bin", "-8"),
        ] {
            let id = node(&c, path);
            let r = request_bounds(&c, Metric::Size, ChartMode::Delta, &id, depth)
                .unwrap()
                .unwrap();
            let hit = request_hit(
                &c,
                Metric::Size,
                ChartMode::Delta,
                r.x + r.width / 2.0,
                r.y + r.height / 2.0,
                depth,
            )
            .unwrap()
            .unwrap();
            assert_eq!((hit.node_id, hit.value), (id, value.into()));
        }
        let before_child = node(&c, "C:\\r\\reverse\\child.bin");
        let after = request_frame(&c, Metric::Size, ChartMode::After, depth).unwrap();
        assert_eq!(after.rendered_block_count, 2);
        let child = request_bounds(&c, Metric::Size, ChartMode::After, &before_child, depth)
            .unwrap()
            .unwrap();
        let hit = request_hit(
            &c,
            Metric::Size,
            ChartMode::After,
            child.x + child.width / 2.0,
            child.y + child.height / 2.0,
            depth,
        )
        .unwrap()
        .unwrap();
        assert_ne!(hit.node_id, before_child);
    }
}
#[test]
fn switching_modes_keeps_every_subpixel_hit_and_published_frames_read_only() {
    let mut before = String::from("文件名称,大小,分配\nC:\\big.bin,1000000000000,2000000000000\n");
    let mut after = String::from("文件名称,大小,分配\nC:\\big.bin,2000000000000,3000000000000\n");
    for i in 0..1030 {
        before.push_str(&format!("C:\\tiny{i}.bin,1,2\n"));
        after.push_str(&format!("C:\\tiny{i}.bin,2,3\n"));
    }
    let (_dir, c) = build(&before, &after);
    for metric in [Metric::Size, Metric::Allocated] {
        let before_frame = request_frame(&c, metric, ChartMode::Before, 0).unwrap();
        for mode in [ChartMode::Delta, ChartMode::Before, ChartMode::After] {
            let frame = request_frame(&c, metric, mode, 0).unwrap();
            assert_eq!(frame.rendered_block_count, 1031);
        }
        for i in 0..1030 {
            let id = node(&c, &format!("C:\\tiny{i}.bin"));
            let b = request_bounds(&c, metric, ChartMode::Before, &id, 0)
                .unwrap()
                .unwrap();
            assert!(b.width > 0.0 && b.height > 0.0);
            assert!(b.width * 4096.0 < 1.0 || b.height * 1024.0 < 1.0);
            let d = request_bounds(&c, metric, ChartMode::Delta, &id, 0)
                .unwrap()
                .unwrap();
            assert_eq!(rect_tuple(&b), rect_tuple(&d));
            for mode in [ChartMode::Before, ChartMode::Delta, ChartMode::After] {
                let r = request_bounds(&c, metric, mode, &id, 0).unwrap().unwrap();
                let hit = request_hit(
                    &c,
                    metric,
                    mode,
                    r.x + r.width / 2.0,
                    r.y + r.height / 2.0,
                    0,
                )
                .unwrap()
                .unwrap();
                assert_eq!(hit.node_id, id);
            }
        }
        assert_eq!(
            request_frame(&c, metric, ChartMode::Before, 0)
                .unwrap()
                .image_data_url,
            before_frame.image_data_url
        );
    }
}

#[test]
fn revisiting_depths_preserves_exact_frames_and_mode_bounds() {
    let csv="文件名称,大小,分配\nC:\\r\\,1000,2000\nC:\\r\\sub\\,1000,2000\nC:\\r\\sub\\deep\\,1000,2000\nC:\\r\\root.bin,10,20\nC:\\r\\sub\\leaf.txt,30,60\nC:\\r\\sub\\deep\\last.bin,60,120\n";
    let (_dir, c) = build(csv, csv);
    let id = node(&c, "C:\\r\\sub\\deep\\last.bin");
    for depth in [1, 2, 3, 4, 5, 1, 0, 2] {
        let b = request_frame(&c, Metric::Size, ChartMode::Before, depth).unwrap();
        let d = request_frame(&c, Metric::Size, ChartMode::Delta, depth).unwrap();
        let a = request_frame(&c, Metric::Size, ChartMode::After, depth).unwrap();
        assert_eq!((b.file_count, b.visible_file_count), (3, 3));
        assert_eq!(b.weight_total, "100");
        assert_eq!(
            b.rendered_block_count,
            match depth {
                1 => 1,
                2 => 2,
                _ => 3,
            }
        );
        assert_eq!(d.rendered_block_count, 0);
        assert!(d.labels.is_empty());
        assert_ne!(b.image_data_url, d.image_data_url);
        assert_eq!(b.image_data_url, a.image_data_url);
        let bounds = request_bounds(&c, Metric::Size, ChartMode::Before, &id, depth)
            .unwrap()
            .unwrap();
        assert!(
            request_bounds(&c, Metric::Size, ChartMode::Delta, &id, depth)
                .unwrap()
                .is_none()
        );
        for mode in [ChartMode::After] {
            assert_eq!(
                rect_tuple(&bounds),
                rect_tuple(
                    &request_bounds(&c, Metric::Size, mode, &id, depth)
                        .unwrap()
                        .unwrap()
                )
            );
        }
    }
}

#[test]
fn hit_and_label_paths_preserve_exact_snapshot_spelling() {
    let before_path = "C:/Root/Leaf.TXT";
    let after_path = "c:\\root\\leaf.txt";
    let (_dir, c) = build(
        &format!("文件名称,大小,分配\n{before_path},10,20\n"),
        &format!("文件名称,大小,分配\n{after_path},30,40\n"),
    );
    let id = node(&c, before_path);
    for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
        let expected = if matches!(mode, ChartMode::Before) {
            before_path
        } else {
            after_path
        };
        let frame = request_frame(&c, Metric::Size, mode, 0).unwrap();
        let label = frame.labels.iter().find(|l| l.node_id == id).unwrap();
        assert_eq!(label.path, expected);
        let hit = request_hit(&c, Metric::Size, mode, 0.5, 0.5, 0)
            .unwrap()
            .unwrap();
        assert_eq!(hit.node_id, id);
        assert_eq!(hit.path, expected);
    }
}

#[test]
fn unchanged_nested_files_reposition_correctly_when_ancestor_area_and_headers_change() {
    let before="文件名称,大小,分配\nC:\\r\\,5100,5100\nC:\\r\\outside.bin,5000,5000\nC:\\r\\stable\\,100,100\nC:\\r\\stable\\sub\\,100,100\nC:\\r\\stable\\sub\\a.txt,40,40\nC:\\r\\stable\\sub\\b.mp3,60,60\n";
    let after = before
        .replace("5100,5100", "600,600")
        .replace("5000,5000", "500,500");
    let (_dir, c) = build(before, &after);
    let sub = node(&c, "C:\\r\\stable\\sub");
    let a = node(&c, "C:\\r\\stable\\sub\\a.txt");
    let b = node(&c, "C:\\r\\stable\\sub\\b.mp3");
    for metric in [Metric::Size, Metric::Allocated] {
        let old = request_frame(&c, metric, ChartMode::Before, 0).unwrap();
        let current = request_frame(&c, metric, ChartMode::After, 0).unwrap();
        assert_eq!(
            (old.weight_total.as_str(), current.weight_total.as_str()),
            ("5100", "600")
        );
        assert!(!old.labels.iter().any(|l| l.node_id == sub));
        let title = current.labels.iter().find(|l| l.node_id == sub).unwrap();
        let ar = request_bounds(&c, metric, ChartMode::After, &a, 0)
            .unwrap()
            .unwrap();
        let br = request_bounds(&c, metric, ChartMode::After, &b, 0)
            .unwrap()
            .unwrap();
        assert!((ar.width * ar.height / (br.width * br.height) - 2.0 / 3.0).abs() < 1e-12);
        for (id, rect, weight) in [(&a, &ar, "40"), (&b, &br, "60")] {
            assert!(rect.y >= title.y + title.height - 1e-12);
            let hit = request_hit(
                &c,
                metric,
                ChartMode::After,
                rect.x + rect.width / 2.0,
                rect.y + rect.height / 2.0,
                0,
            )
            .unwrap()
            .unwrap();
            assert_eq!(&hit.node_id, id);
            assert_eq!(hit.weight, weight);
            let previous = request_bounds(&c, metric, ChartMode::Before, id, 0)
                .unwrap()
                .unwrap();
            assert_ne!(rect_tuple(rect), rect_tuple(&previous));
            let old_parent = request_bounds(&c, metric, ChartMode::Before, &sub, 0)
                .unwrap()
                .unwrap();
            let new_parent = request_bounds(&c, metric, ChartMode::After, &sub, 0)
                .unwrap()
                .unwrap();
            let old_header = old
                .labels
                .iter()
                .find(|l| l.node_id == sub && l.kind == NodeKind::Directory)
                .map_or(0.0, |l| l.height);
            let old_x = old_parent.x + if old_header > 0.0 { 8.0 / 4096.0 } else { 0.0 };
            let old_y = old_parent.y + old_header;
            let old_w = old_parent.width - if old_header > 0.0 { 16.0 / 4096.0 } else { 0.0 };
            let old_h =
                old_parent.height - old_header - if old_header > 0.0 { 8.0 / 1024.0 } else { 0.0 };
            let new_x = new_parent.x + 8.0 / 4096.0;
            let new_y = new_parent.y + title.height;
            let new_w = new_parent.width - 16.0 / 4096.0;
            let new_h = new_parent.height - title.height - 8.0 / 1024.0;
            for (old_fraction, new_fraction) in [
                ((previous.x - old_x) / old_w, (rect.x - new_x) / new_w),
                ((previous.y - old_y) / old_h, (rect.y - new_y) / new_h),
                (previous.width / old_w, rect.width / new_w),
                (previous.height / old_h, rect.height / new_h),
            ] {
                assert!((old_fraction - new_fraction).abs() < 1e-12);
            }
            assert!(request_bounds(&c, metric, ChartMode::Delta, id, 0)
                .unwrap()
                .is_none());
        }
    }
}

#[test]
fn delta_reflows_growth_shrink_additions_and_removals_to_fill_the_atlas() {
    let before="文件名称,大小,分配\nC:\\grow.txt,40,40\nC:\\shrink.bin,60,60\nC:\\removed.mp3,20,20\nC:\\same.jpg,80,80\n";
    let after="文件名称,大小,分配\nC:\\grow.txt,50,60\nC:\\shrink.bin,50,30\nC:\\added.txt,40,40\nC:\\same.jpg,80,80\n";
    let (_dir, c) = build(before, after);
    let same = node(&c, "C:\\same.jpg");
    for (metric, total, positive, negative, net, changes) in [
        (
            Metric::Size,
            80i64,
            50i64,
            30i64,
            20i64,
            [10i64, -10, -20, 40],
        ),
        (
            Metric::Allocated,
            110i64,
            60i64,
            50i64,
            10i64,
            [20i64, -30, -20, 40],
        ),
    ] {
        let d = request_frame(&c, metric, ChartMode::Delta, 0).unwrap();
        assert_eq!(
            (d.file_count, d.visible_file_count, d.rendered_block_count),
            (5, 4, 4)
        );
        assert_eq!(
            (
                d.weight_total,
                d.positive_total,
                d.negative_total,
                d.net_delta
            ),
            (
                total.to_string(),
                positive.to_string(),
                negative.to_string(),
                net.to_string()
            )
        );
        assert!(!d.labels.iter().any(|l| l.node_id == same));
        assert!(request_bounds(&c, metric, ChartMode::Delta, &same, 0)
            .unwrap()
            .is_none());
        let mut area = 0.0;
        for (path, change) in [
            "C:\\grow.txt",
            "C:\\shrink.bin",
            "C:\\removed.mp3",
            "C:\\added.txt",
        ]
        .into_iter()
        .zip(changes)
        {
            let id = node(&c, path);
            let rect = request_bounds(&c, metric, ChartMode::Delta, &id, 0)
                .unwrap()
                .unwrap();
            let weight = change.abs();
            let expected = weight as f64 / total as f64;
            assert!((rect.width * rect.height - expected).abs() < 1e-12);
            area += rect.width * rect.height;
            let hit = request_hit(
                &c,
                metric,
                ChartMode::Delta,
                rect.x + rect.width / 2.0,
                rect.y + rect.height / 2.0,
                0,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                (hit.node_id, hit.path, hit.weight, hit.value),
                (id, path.to_owned(), weight.to_string(), change.to_string())
            );
        }
        assert!((area - 1.0).abs() < 1e-12);
    }
}

#[test]
fn cancelling_directory_changes_keep_gross_area_at_collapsed_and_full_depth() {
    let before="文件名称,大小,分配\nC:\\r\\,180,180\nC:\\r\\grow.txt,40,40\nC:\\r\\shrink.bin,60,60\nC:\\r\\stable\\,80,80\nC:\\r\\stable\\same.mp3,80,80\n";
    let after = before
        .replace("grow.txt,40,40", "grow.txt,50,50")
        .replace("shrink.bin,60,60", "shrink.bin,50,50");
    let (_dir, c) = build(before, &after);
    let root = node(&c, "C:\\r");
    let stable = node(&c, "C:\\r\\stable");
    for metric in [Metric::Size, Metric::Allocated] {
        for depth in [1, 0] {
            let f = request_frame(&c, metric, ChartMode::Delta, depth).unwrap();
            assert_eq!(
                (
                    f.weight_total.as_str(),
                    f.positive_total.as_str(),
                    f.negative_total.as_str(),
                    f.net_delta.as_str()
                ),
                ("20", "10", "10", "0")
            );
            assert_eq!((f.file_count, f.visible_file_count), (3, 2));
            assert_eq!(f.rendered_block_count, if depth == 1 { 1 } else { 2 });
            assert!(request_bounds(&c, metric, ChartMode::Delta, &stable, depth)
                .unwrap()
                .is_none());
            let r = request_bounds(&c, metric, ChartMode::Delta, &root, depth)
                .unwrap()
                .unwrap();
            assert!((r.width * r.height - 1.0).abs() < 1e-12);
            if depth == 1 {
                let hit = request_hit(&c, metric, ChartMode::Delta, 0.5, 0.5, depth)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    (hit.weight.as_str(), hit.value.as_str(), hit.collapsed),
                    ("20", "0", true)
                );
            }
        }
    }
}

#[test]
fn delta_area_uses_only_the_selected_metric() {
    let (_dir, c) = build(
        "文件名称,大小,分配\nC:\\changed.bin,10,10\nC:\\same.bin,20,20\n",
        "文件名称,大小,分配\nC:\\changed.bin,10,20\nC:\\same.bin,20,20\n",
    );
    let id = node(&c, "C:\\changed.bin");
    let size = request_frame(&c, Metric::Size, ChartMode::Delta, 0).unwrap();
    assert_eq!(
        (
            size.weight_total.as_str(),
            size.visible_file_count,
            size.rendered_block_count
        ),
        ("0", 0, 0)
    );
    assert!(request_bounds(&c, Metric::Size, ChartMode::Delta, &id, 0)
        .unwrap()
        .is_none());
    let allocated = request_frame(&c, Metric::Allocated, ChartMode::Delta, 0).unwrap();
    assert_eq!(
        (
            allocated.weight_total.as_str(),
            allocated.visible_file_count,
            allocated.rendered_block_count
        ),
        ("10", 1, 1)
    );
    let rect = request_bounds(&c, Metric::Allocated, ChartMode::Delta, &id, 0)
        .unwrap()
        .unwrap();
    assert!((rect.width * rect.height - 1.0).abs() < 1e-12);
}

#[test]
fn independently_requested_layouts_are_read_only_and_bound_to_comparison() {
    let before = "文件名称,大小,分配\nC:\\r\\,30,60\nC:\\r\\a.txt,10,20\nC:\\r\\b.mp3,20,40\n";
    let after = before.replace("a.txt,10,20", "a.txt,30,60");
    let (dir, c) = build(before, &after);
    assert!(c.layouts.borrow().is_empty());
    let layout = TreemapLayout::build(
        &c,
        Metric::Size,
        ChartMode::After,
        0,
        &JobControl::default(),
    )
    .unwrap();
    let memory = layout.memory_bytes();
    let original = layout.frame(&c, "layout-A").unwrap();
    assert_eq!(original.layout_id, "layout-A");
    assert_eq!(original.comparison_id, c.summary.comparison_id);
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::Delta, ChartMode::Before, ChartMode::After] {
            let other = TreemapLayout::build(&c, metric, mode, 1, &JobControl::default()).unwrap();
            assert!(other.memory_bytes() > 0);
            let _ = other.frame(&c, "other").unwrap();
        }
    }
    let id = node(&c, "C:\\r\\a.txt");
    let rect = layout.get_bounds(&c, &id).unwrap().unwrap();
    assert_eq!(
        layout
            .hit_test(&c, rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
            .unwrap()
            .unwrap()
            .node_id,
        id
    );
    assert_eq!(layout.memory_bytes(), memory);
    assert_eq!(
        layout.frame(&c, "layout-B").unwrap().image_data_url,
        original.image_data_url
    );
    assert!(c.layouts.borrow().is_empty());
    let names = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 2);
    assert!(names.iter().all(|n| n == "before.csv" || n == "after.csv"));
    let (_other_dir, mut stale) = build(before, &after);
    stale.data.summary.comparison_id = "replacement".into();
    assert_eq!(
        layout.frame(&stale, "old").unwrap_err().code,
        "STALE_COMPARISON"
    );
    assert_eq!(
        layout.get_bounds(&stale, &id).unwrap_err().code,
        "STALE_COMPARISON"
    );
    assert_eq!(
        layout.hit_test(&stale, 0.5, 0.5).unwrap_err().code,
        "STALE_COMPARISON"
    );
}
