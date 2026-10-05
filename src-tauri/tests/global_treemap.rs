use base64::Engine;
use rusqlite::{params, Connection};
use wiztree_diff_lib::{
    global_treemap,
    import::{self, JobControl},
    store,
    types::{ChartMode, Metric, Status},
};

fn build(before: &str, after: &str) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("before.csv");
    let a = dir.path().join("after.csv");
    let db = dir.path().join("comparison.sqlite");
    std::fs::write(&b, before).unwrap();
    std::fs::write(&a, after).unwrap();
    store::build_comparison(
        &b,
        &a,
        &db,
        "treemap-test",
        &JobControl::default(),
        &mut |_| {},
    )
    .unwrap();
    let conn = store::open_reader(&db).unwrap();
    (dir, conn)
}
fn node(conn: &Connection, path: &str) -> String {
    let normalized = import::normalize(path).0;
    let (prefix, basename) = import::canonical_parts(&normalized);
    format!("n{}",conn.query_row(
        "SELECT n.id FROM path_prefixes p JOIN node_records n ON n.prefix_id=p.id WHERE p.path=?1 AND n.basename_key=?2",
        params![prefix,basename],|r|r.get::<_,i64>(0),
    ).unwrap())
}

// Expected files come from imported CSV records; geometry is observed only through
// the public bounds/hit APIs, independent of the persistent representation.
fn file_rects(
    c: &Connection,
    metric: Metric,
    mode: ChartMode,
) -> Vec<(i64, f64, f64, f64, f64, i64)> {
    let side = if matches!(mode, ChartMode::After) {
        1
    } else {
        0
    };
    let column = if matches!(metric, Metric::Size) {
        "size"
    } else {
        "allocated"
    };
    let mut query=c.prepare(&format!("SELECT node_id,{column} FROM entries WHERE side=?1 AND kind='file' AND {column}>0 ORDER BY node_id")).unwrap();
    let rects = query
        .query_map([side], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))
        .unwrap()
        .map(|r| {
            let (id, weight) = r.unwrap();
            let b = global_treemap::get_bounds(c, metric, mode, &format!("n{id}"), 0)
                .unwrap()
                .unwrap();
            (id, b.x, b.y, b.width, b.height, weight)
        })
        .collect();
    rects
}
fn imported_connection(before: &str, after: &str) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("before.csv");
    let a = dir.path().join("after.csv");
    std::fs::write(&b, before).unwrap();
    std::fs::write(&a, after).unwrap();
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(store::SCHEMA).unwrap();
    let control = JobControl::default();
    let mut bs = import::load(&c, &b, 0, &control, &mut |_| {}).unwrap();
    let mut as_ = import::load(&c, &a, 1, &control, &mut |_| {}).unwrap();
    store::finish_import(&c, &mut bs, &mut as_, &control).unwrap();
    (dir, c)
}
#[test]
fn appending_other_views_preserves_precise_hits_in_existing_maps() {
    let mut before = String::from("文件名称,大小,分配\n");
    let mut after = before.clone();
    for i in 0..1027 {
        before.push_str(&format!("C:\\file{i}.bin,{},{:}\n", i % 17 + 1, i % 23 + 1));
        after.push_str(&format!("C:\\file{i}.bin,{},{:}\n", i % 11 + 1, i % 29 + 1));
    }
    let (_dir, c) = build(&before, &after);
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::After, ChartMode::Before, ChartMode::Delta] {
            let frame = global_treemap::get_frame(&c, metric, mode, 0).unwrap();
            assert_eq!(frame.rendered_block_count, 1027);
        }
    }
    // Independently persisted mode families must not replace earlier hit candidates.
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::After, ChartMode::Before, ChartMode::Delta] {
            for i in [0, 19, 300, 700, 1026] {
                let id = node(&c, &format!("C:\\file{i}.bin"));
                let r = global_treemap::get_bounds(&c, metric, mode, &id, 0)
                    .unwrap()
                    .unwrap();
                let hit = global_treemap::hit_test(
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
        (ChartMode::Delta, "C:\\swap"),
    ] {
        let frame = global_treemap::get_frame(&c, Metric::Size, mode, 0).unwrap();
        assert_eq!(frame.weight_total, "12");
        assert_eq!(
            (frame.visible_file_count, frame.rendered_block_count),
            (1, 1)
        );
        let id = node(&c, path);
        let rect = global_treemap::get_bounds(&c, Metric::Size, mode, &id, 0)
            .unwrap()
            .unwrap();
        let hit = global_treemap::hit_test(
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
}

#[test]
fn nested_directory_headers_select_real_ancestors_without_occluding_files() {
    use wiztree_diff_lib::types::NodeKind;
    let csv="文件名称,大小,分配\nC:\\r\\,999,999\nC:\\r\\root.txt,20,20\nC:\\r\\A\\,999,999\nC:\\r\\A\\a.txt,20,20\nC:\\r\\A\\sub\\,999,999\nC:\\r\\A\\sub\\one.bin,40,40\nC:\\r\\A\\sub\\two.py,40,40\nC:\\r\\B\\,999,999\nC:\\r\\B\\big.dll,80,80\nC:\\r\\B\\small.jpg,40,40\n";
    let (_dir, c) = build(csv, csv);
    let frame = global_treemap::get_frame(&c, Metric::Size, ChartMode::Before, 0).unwrap();
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
        let hit = global_treemap::hit_test(
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
        let r = global_treemap::get_bounds(&c, Metric::Size, ChartMode::Before, &id, 0)
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
        let hit = global_treemap::hit_test(
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
    let limited = global_treemap::get_frame(&c, Metric::Size, ChartMode::After, 1).unwrap();
    let header = limited
        .labels
        .iter()
        .find(|l| l.kind == NodeKind::Directory)
        .unwrap();
    let hit = global_treemap::hit_test(
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
    for _ in 0..80 {
        csv.push_str(&format!("{path}\\,1,1\n"));
        path.push_str("\\sub");
    }
    let file = format!("{path}\\deep.bin");
    // The last actual directory exists, not an orphan file shortcut.
    csv.push_str(&format!("{path}\\,1,1\n{file},1,1\n"));
    let (_dir, c) = build(&csv, &csv);
    let frame = global_treemap::get_frame(&c, Metric::Size, ChartMode::After, 0).unwrap();
    assert_eq!((frame.file_count, frame.rendered_block_count), (1, 1));
    assert_eq!(frame.weight_total, "1");
    let labels = frame
        .labels
        .iter()
        .filter(|l| l.kind == NodeKind::Directory)
        .count();
    assert!(labels > 1 && labels < 81);
    let bounds =
        global_treemap::get_bounds(&c, Metric::Size, ChartMode::After, &node(&c, &file), 0)
            .unwrap()
            .unwrap();
    assert!(bounds.width > 0.0 && bounds.height > 0.0);
    let hit = global_treemap::hit_test(
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
        (Metric::Size, ChartMode::Delta, 609, 606, 9007199254741670),
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
        (
            Metric::Allocated,
            ChartMode::Delta,
            609,
            607,
            9007199254742281,
        ),
    ];
    for (metric, mode, files, visible, weight) in expected {
        let frame = global_treemap::get_frame(&c, metric, mode, 0).unwrap();
        assert_eq!(
            (frame.file_count, frame.visible_file_count),
            (files, visible)
        );
        assert_eq!(frame.weight_total, weight.to_string());
        assert!(frame.labels.len() <= 320);
        let png = base64::engine::general_purpose::STANDARD
            .decode(
                frame
                    .image_data_url
                    .strip_prefix("data:image/png;base64,")
                    .unwrap(),
            )
            .unwrap();
        let decoder = png::Decoder::new(std::io::Cursor::new(png));
        let reader = decoder.read_info().unwrap();
        assert_eq!((reader.info().width, reader.info().height), (4096, 1024));
        let rects = file_rects(&c, metric, mode);
        assert_eq!(rects.len() as u64, visible);
        assert_eq!(rects.iter().map(|r| r.5).sum::<i64>(), weight);
        for &(id, x, y, w, h, file_weight) in &rects {
            assert!(w > 0.0 && h > 0.0, "positive file lost area: {id}");
            assert!(x >= -1e-12 && y >= -1e-12 && x + w <= 1.0 + 1e-10 && y + h <= 1.0 + 1e-10);
            // Very small geometries are persisted even when no f64 interior
            // coordinate or atlas pixel can represent their separation.
            if w > 1e-10 && h > 1e-10 {
                let hit = global_treemap::hit_test(&c, metric, mode, x + w * 0.5, y + h * 0.5, 0)
                    .unwrap()
                    .unwrap();
                assert_eq!(hit.node_id, format!("n{id}"));
                assert_eq!(hit.weight, file_weight.to_string());
            }
        }
        // Header/gutter space is decoration. Proportions remain exact within
        // each parent content area rather than the entire image rectangle.
        for &(id, x, y, w, h, file_weight) in &rects {
            let parent: Option<i64> = c
                .query_row("SELECT parent_id FROM nodes WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .unwrap();
            if let Some(parent) = parent {
                let pid = format!("n{parent}");
                let p = global_treemap::get_bounds(&c, metric, mode, &pid, 0)
                    .unwrap()
                    .unwrap();
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
                let col = if matches!(metric, Metric::Size) {
                    "size"
                } else {
                    "allocated"
                };
                let parent_weight:i64=c.query_row(&format!("WITH RECURSIVE scope(id) AS (SELECT ?2 UNION ALL SELECT n.id FROM nodes n JOIN scope s ON n.parent_id=s.id) SELECT sum(e.{col}) FROM scope s JOIN entries e ON e.node_id=s.id WHERE e.side=?1 AND e.kind='file'"),params![side,parent],|r|r.get(0)).unwrap();
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
    let delta = global_treemap::get_frame(&c, Metric::Size, ChartMode::Delta, 0).unwrap();
    assert_eq!(
        (
            &delta.positive_total,
            &delta.negative_total,
            &delta.net_delta
        ),
        (&"637".to_owned(), &"38".to_owned(), &"599".to_owned())
    );
    let allocated = global_treemap::get_frame(&c, Metric::Allocated, ChartMode::Delta, 0).unwrap();
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
    let swap = node(&c, "C:\\root\\swap");
    let own_rect = global_treemap::get_bounds(&c, Metric::Size, ChartMode::Delta, &swap, 0)
        .unwrap()
        .unwrap();
    let own = (own_rect.x, own_rect.y, own_rect.width, own_rect.height);
    let hit = global_treemap::hit_test(
        &c,
        Metric::Size,
        ChartMode::Delta,
        own.0 + own.2 / 2.0,
        own.1 + own.3 / 2.0,
        0,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        (hit.node_id, hit.status, hit.value),
        (swap.clone(), Status::TypeChanged, "-31".into())
    );
    let bounds = global_treemap::get_bounds(&c, Metric::Size, ChartMode::Delta, &swap, 0)
        .unwrap()
        .unwrap();
    assert!((bounds.width * bounds.height - 31.0 / 9007199254741670.0).abs() < 1e-10);
    assert!(global_treemap::get_bounds(
        &c,
        Metric::Size,
        ChartMode::After,
        &node(&c, "C:\\root\\zero"),
        0
    )
    .unwrap()
    .is_some());
    assert!(global_treemap::get_bounds(
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
            global_treemap::hit_test(&c, Metric::Size, ChartMode::After, x, y, 0)
                .unwrap_err()
                .code,
            "INVALID_POSITION"
        );
    }
    assert_eq!(
        global_treemap::get_bounds(&c, Metric::Size, ChartMode::After, "bad", 0)
            .unwrap_err()
            .code,
        "INVALID_NODE"
    );
    assert_eq!(
        global_treemap::get_bounds(&c, Metric::Size, ChartMode::After, "n99999999", 0)
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
            let f = global_treemap::get_frame(&c, metric, mode, 0).unwrap();
            assert_eq!((f.file_count, f.visible_file_count), (1, 0));
            assert_eq!(f.weight_total, "0");
            assert!(global_treemap::hit_test(&c, metric, mode, 0.5, 0.5, 0)
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
    let edge = global_treemap::hit_test(&c, Metric::Size, ChartMode::After, right.1, 0.5, 0)
        .unwrap()
        .unwrap();
    assert_eq!(edge.node_id, format!("n{}", right.0));
    let outer = global_treemap::hit_test(&c, Metric::Size, ChartMode::After, 1.0, 1.0, 0)
        .unwrap()
        .unwrap();
    assert_eq!(outer.node_id, format!("n{}", right.0));
}

#[test]
fn leaf_aggregate_overflow_fails_and_cancelled_build_writes_nothing() {
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
    let error = store::build_comparison(
        &b,
        &a,
        &dir.path().join("overflow.sqlite"),
        "overflow",
        &JobControl::default(),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(error.code, "AGGREGATE_OVERFLOW");
    let (_dir, c) = imported_connection(
        "文件名称,大小,分配\nC:\\one.bin,1,1\n",
        "文件名称,大小,分配\nC:\\zero.bin,0,0\n",
    );
    let control = JobControl::default();
    control.cancel();
    assert_eq!(
        global_treemap::materialize(&c, &control).unwrap_err().code,
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
            let f = global_treemap::get_frame(&c, metric, mode, 0).unwrap();
            assert_eq!(f.file_count, 100001);
            assert_eq!(f.visible_file_count, 100001);
            assert_eq!(
                f.weight_total,
                if matches!(metric, Metric::Size) {
                    "100001"
                } else {
                    "200002"
                }
            );
        }
    }
    let frame = global_treemap::get_frame(&c, Metric::Size, ChartMode::After, 0).unwrap();
    assert_eq!(frame.rendered_block_count, 100001);
    for i in [0, 50000, 100000] {
        let id = node(&c, &format!("C:\\actual{i}\\f.bin"));
        let r = global_treemap::get_bounds(&c, Metric::Size, ChartMode::After, &id, 0)
            .unwrap()
            .unwrap();
        let hit = global_treemap::hit_test(
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
fn active_job_cancellation_interrupts_sql_and_rolls_back_materialization() {
    let mut csv = String::from("文件名称,大小,分配\n");
    for i in 0..10000 {
        csv.push_str(&format!("C:\\f{i}.bin,1,2\n"));
    }
    let (_dir, c) = imported_connection(&csv, "文件名称,大小,分配\nC:\\after.bin,0,0\n");
    let control = JobControl::default();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            loop {
                let attached = control.interrupt.lock().unwrap().is_some();
                if attached {
                    break;
                }
                std::thread::yield_now();
            }
            control.cancel();
        });
        assert_eq!(
            global_treemap::materialize(&c, &control).unwrap_err().code,
            "CANCELLED"
        );
    });
    global_treemap::materialize(&c, &JobControl::default()).unwrap();
    let frame = global_treemap::get_frame(&c, Metric::Size, ChartMode::Before, 0).unwrap();
    assert_eq!(frame.file_count, 10000);
    assert_eq!(frame.rendered_block_count, 10000);
    assert_eq!(frame.weight_total, "10000");
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
    let (dir, c) = build(&b, &a);
    let root = node(&c, "C:\\deep\\export");
    let deep = node(&c, "C:\\deep\\export\\sub\\deep");
    let hidden = node(&c, "C:\\deep\\export\\sub\\deep\\more\\keep.mp3");
    for metric in [Metric::Size, Metric::Allocated] {
        for depth in [1, 3, 0] {
            let before = global_treemap::get_frame(&c, metric, ChartMode::Before, depth).unwrap();
            let delta = global_treemap::get_frame(&c, metric, ChartMode::Delta, depth).unwrap();
            let after = global_treemap::get_frame(&c, metric, ChartMode::After, depth).unwrap();
            assert_eq!(before.max_depth, depth);
            assert_eq!((before.file_count, before.visible_file_count), (5, 5));
            assert_eq!((delta.file_count, delta.visible_file_count), (6, 5));
            assert_eq!(delta.added_file_count, 1);
            assert_eq!(
                (
                    before.weight_total.as_str(),
                    delta.weight_total.as_str(),
                    after.weight_total.as_str()
                ),
                ("100", "100", "100")
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
            assert_eq!(delta.rendered_block_count, before.rendered_block_count);
            for path in [
                "C:\\deep\\export\\root.txt",
                "C:\\deep\\export\\sub\\keep.bin",
                "C:\\deep\\export\\sub\\deep\\grow.txt",
                "C:\\deep\\export\\sub\\deep\\removed.bin",
                "C:\\deep\\export\\sub\\deep\\more\\keep.mp3",
            ] {
                let id = node(&c, path);
                let b = global_treemap::get_bounds(&c, metric, ChartMode::Before, &id, depth)
                    .unwrap()
                    .unwrap();
                let d = global_treemap::get_bounds(&c, metric, ChartMode::Delta, &id, depth)
                    .unwrap()
                    .unwrap();
                assert_eq!(rect_tuple(&b), rect_tuple(&d));
            }
            let highlight =
                global_treemap::get_bounds(&c, metric, ChartMode::Delta, &hidden, depth)
                    .unwrap()
                    .unwrap();
            if depth != 0 {
                let ancestor = global_treemap::get_bounds(
                    &c,
                    metric,
                    ChartMode::Before,
                    if depth == 1 { &root } else { &deep },
                    depth,
                )
                .unwrap()
                .unwrap();
                assert_eq!(rect_tuple(&highlight), rect_tuple(&ancestor));
                let hit = global_treemap::hit_test(
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
                let hit = global_treemap::hit_test(
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
    drop(c);
    let c = store::open_reader(&dir.path().join("comparison.sqlite")).unwrap();
    let restored = global_treemap::get_frame(&c, Metric::Size, ChartMode::Delta, 1).unwrap();
    assert_eq!(
        (
            restored.weight_total.as_str(),
            restored.rendered_block_count
        ),
        ("100", 1)
    );
}

#[test]
fn unchanged_self_comparison_is_the_filled_before_map_at_every_depth() {
    let csv="文件名称,大小,分配\nC:\\r\\,20,20\nC:\\r\\sub\\,20,20\nC:\\r\\sub\\a.txt,8,8\nC:\\r\\sub\\b.mp3,12,12\n";
    let (_dir, c) = build(csv, csv);
    for depth in [1, 3, 0] {
        let b = global_treemap::get_frame(&c, Metric::Size, ChartMode::Before, depth).unwrap();
        let d = global_treemap::get_frame(&c, Metric::Size, ChartMode::Delta, depth).unwrap();
        assert_eq!(d.weight_total, "20");
        assert_eq!(d.net_delta, "0");
        assert_eq!(d.added_file_count, 0);
        assert!(
            b.image_data_url == d.image_data_url,
            "self comparison raster differs at depth {depth}"
        );
    }
}

#[test]
fn root_only_and_new_only_additions_are_counted_and_listed_with_bounded_warnings() {
    let h = "文件名称,大小,分配\n";
    let b = format!("{h}C:\\r\\,10,10\nC:\\r\\old.txt,10,10\n");
    let mut a = format!("{b}C:\\r\\new.txt,2,2\n");
    for i in 0..20 {
        a.push_str(&format!("Z:\\new{i}.bin,1,1\n"));
    }
    let (_dir, c) = build(&b, &a);
    for depth in [1, 3, 0] {
        let d = global_treemap::get_frame(&c, Metric::Size, ChartMode::Delta, depth).unwrap();
        assert_eq!(d.weight_total, "10");
        assert_eq!(d.added_file_count, 21);
        assert!(d.warnings.iter().any(|w| w.contains("20 个新增文件没有")));
        assert_eq!(
            d.warnings.iter().filter(|w| w.starts_with("Z:")).count(),
            16
        );
        assert!(d.warnings.len() <= 19);
    }
    let empty_before = format!("{h}Y:\\empty\\,0,0\n");
    let (_dir, c) = build(&empty_before, &a);
    let d = global_treemap::get_frame(&c, Metric::Size, ChartMode::Delta, 1).unwrap();
    assert_eq!(d.weight_total, "0");
    assert_eq!(d.rendered_block_count, 0);
    assert_eq!(d.added_file_count, 22);
    assert!(d.warnings.iter().any(|w| w.contains("22 个新增文件没有")));
    assert!(
        global_treemap::hit_test(&c, Metric::Size, ChartMode::Delta, 0.5, 0.5, 1)
            .unwrap()
            .is_none()
    );
}

#[test]
fn type_changes_keep_before_own_leaf_and_exclude_after_directory_children_from_delta() {
    let b="文件名称,大小,分配\nC:\\r\\,20,20\nC:\\r\\swap,12,12\nC:\\r\\reverse\\,8,8\nC:\\r\\reverse\\child.bin,8,8\n";
    let a="文件名称,大小,分配\nC:\\r\\,20,20\nC:\\r\\swap\\,12,12\nC:\\r\\swap\\child.txt,12,12\nC:\\r\\reverse,8,8\n";
    let (_dir, c) = build(b, a);
    for depth in [3, 0] {
        let d = global_treemap::get_frame(&c, Metric::Size, ChartMode::Delta, depth).unwrap();
        assert_eq!(d.weight_total, "20");
        assert_eq!(d.rendered_block_count, 2);
        let swap = node(&c, "C:\\r\\swap");
        let rect = global_treemap::get_bounds(&c, Metric::Size, ChartMode::Delta, &swap, depth)
            .unwrap()
            .unwrap();
        assert_eq!(
            rect_tuple(&rect),
            rect_tuple(
                &global_treemap::get_bounds(&c, Metric::Size, ChartMode::Before, &swap, depth)
                    .unwrap()
                    .unwrap()
            )
        );
        let hit = global_treemap::hit_test(
            &c,
            Metric::Size,
            ChartMode::Delta,
            rect.x + rect.width / 2.0,
            rect.y + rect.height / 2.0,
            depth,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.node_id, swap);
        assert_eq!(hit.status, Status::TypeChanged);
        assert_eq!(hit.kind, wiztree_diff_lib::types::NodeKind::File);
        assert!(!hit.collapsed);
        assert!(pixels(&d)
            .chunks_exact(4)
            .any(|p| p[..3] == [148, 114, 204]));
        let after_child = node(&c, "C:\\r\\swap\\child.txt");
        let child =
            global_treemap::get_bounds(&c, Metric::Size, ChartMode::Delta, &after_child, depth)
                .unwrap()
                .unwrap();
        assert_eq!(rect_tuple(&child), rect_tuple(&rect));
        let before_child = node(&c, "C:\\r\\reverse\\child.bin");
        let after = global_treemap::get_frame(&c, Metric::Size, ChartMode::After, depth).unwrap();
        assert_eq!(after.rendered_block_count, 2);
        let child =
            global_treemap::get_bounds(&c, Metric::Size, ChartMode::After, &before_child, depth)
                .unwrap()
                .unwrap();
        let hit = global_treemap::hit_test(
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
    let (dir, c) = build(&before, &after);
    let db = dir.path().join("comparison.sqlite");
    for metric in [Metric::Size, Metric::Allocated] {
        global_treemap::get_frame(&c, metric, ChartMode::Before, 0).unwrap();
        let bytes = std::fs::metadata(&db).unwrap().len();
        let data_version: i64 = c
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .unwrap();
        for mode in [ChartMode::Delta, ChartMode::Before, ChartMode::After] {
            let frame = global_treemap::get_frame(&c, metric, mode, 0).unwrap();
            assert_eq!(frame.rendered_block_count, 1031);
        }
        for i in 0..1030 {
            let id = node(&c, &format!("C:\\tiny{i}.bin"));
            let b = global_treemap::get_bounds(&c, metric, ChartMode::Before, &id, 0)
                .unwrap()
                .unwrap();
            assert!(b.width > 0.0 && b.height > 0.0);
            assert!(b.width * 4096.0 < 1.0 || b.height * 1024.0 < 1.0);
            let d = global_treemap::get_bounds(&c, metric, ChartMode::Delta, &id, 0)
                .unwrap()
                .unwrap();
            assert_eq!(rect_tuple(&b), rect_tuple(&d));
            for mode in [ChartMode::Before, ChartMode::Delta, ChartMode::After] {
                let r = global_treemap::get_bounds(&c, metric, mode, &id, 0)
                    .unwrap()
                    .unwrap();
                let hit = global_treemap::hit_test(
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
        assert_eq!(std::fs::metadata(&db).unwrap().len(), bytes);
        assert_eq!(
            c.query_row("PRAGMA data_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            data_version
        );
    }
}

#[test]
fn revisiting_depths_preserves_exact_frames_and_mode_bounds() {
    let csv="文件名称,大小,分配\nC:\\r\\,1000,2000\nC:\\r\\sub\\,1000,2000\nC:\\r\\sub\\deep\\,1000,2000\nC:\\r\\root.bin,10,20\nC:\\r\\sub\\leaf.txt,30,60\nC:\\r\\sub\\deep\\last.bin,60,120\n";
    let (_dir, c) = build(csv, csv);
    let id = node(&c, "C:\\r\\sub\\deep\\last.bin");
    for depth in [1, 2, 3, 4, 5, 1, 0, 2] {
        let b = global_treemap::get_frame(&c, Metric::Size, ChartMode::Before, depth).unwrap();
        let d = global_treemap::get_frame(&c, Metric::Size, ChartMode::Delta, depth).unwrap();
        let a = global_treemap::get_frame(&c, Metric::Size, ChartMode::After, depth).unwrap();
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
        assert_eq!(b.image_data_url, d.image_data_url);
        assert_eq!(b.image_data_url, a.image_data_url);
        let bounds = global_treemap::get_bounds(&c, Metric::Size, ChartMode::Before, &id, depth)
            .unwrap()
            .unwrap();
        for mode in [ChartMode::After, ChartMode::Delta] {
            assert_eq!(
                rect_tuple(&bounds),
                rect_tuple(
                    &global_treemap::get_bounds(&c, Metric::Size, mode, &id, depth)
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
        let expected = if matches!(mode, ChartMode::After) {
            after_path
        } else {
            before_path
        };
        let frame = global_treemap::get_frame(&c, Metric::Size, mode, 0).unwrap();
        let label = frame.labels.iter().find(|l| l.node_id == id).unwrap();
        assert_eq!(label.path, expected);
        let hit = global_treemap::hit_test(&c, Metric::Size, mode, 0.5, 0.5, 0)
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
        let old = global_treemap::get_frame(&c, metric, ChartMode::Before, 0).unwrap();
        let current = global_treemap::get_frame(&c, metric, ChartMode::After, 0).unwrap();
        assert_eq!(
            (old.weight_total.as_str(), current.weight_total.as_str()),
            ("5100", "600")
        );
        assert!(!old.labels.iter().any(|l| l.node_id == sub));
        let title = current.labels.iter().find(|l| l.node_id == sub).unwrap();
        let ar = global_treemap::get_bounds(&c, metric, ChartMode::After, &a, 0)
            .unwrap()
            .unwrap();
        let br = global_treemap::get_bounds(&c, metric, ChartMode::After, &b, 0)
            .unwrap()
            .unwrap();
        assert!((ar.width * ar.height / (br.width * br.height) - 2.0 / 3.0).abs() < 1e-12);
        for (id, rect, weight) in [(&a, &ar, "40"), (&b, &br, "60")] {
            assert!(rect.y >= title.y + title.height - 1e-12);
            let hit = global_treemap::hit_test(
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
            let previous = global_treemap::get_bounds(&c, metric, ChartMode::Before, id, 0)
                .unwrap()
                .unwrap();
            assert_ne!(rect_tuple(rect), rect_tuple(&previous));
            let delta = global_treemap::get_bounds(&c, metric, ChartMode::Delta, id, 0)
                .unwrap()
                .unwrap();
            assert_eq!(rect_tuple(&previous), rect_tuple(&delta));
        }
    }
}
