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
    format!(
        "n{}",
        conn.query_row(
            "SELECT id FROM nodes WHERE path=?1",
            [import::normalize(path).0],
            |r| r.get::<_, i64>(0)
        )
        .unwrap()
    )
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
    let mut q=c.prepare("SELECT r.node_id,r.x,r.y,r.width,r.height,p.y+p.header_height FROM treemap_rects r JOIN treemap_weights w ON w.key=r.key JOIN treemap_rects p ON p.view=r.view AND p.key=w.parent_id WHERE r.view=6 AND r.leaf=1").unwrap();
    let mut rows = q.query([]).unwrap();
    while let Some(r) = rows.next().unwrap() {
        let (id, x, y, w, h): (i64, f64, f64, f64, f64) = (
            r.get(0).unwrap(),
            r.get(1).unwrap(),
            r.get(2).unwrap(),
            r.get(3).unwrap(),
            r.get(4).unwrap(),
        );
        assert!(y >= r.get::<_, f64>(5).unwrap() - 1e-12);
        assert!(w > 0.0 && h > 0.0);
        let hit = global_treemap::hit_test(
            &c,
            Metric::Size,
            ChartMode::Before,
            x + w / 2.0,
            y + h / 2.0,
            0,
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.node_id, format!("n{id}"));
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
    for (v, (metric, mode, files, visible, weight)) in expected.into_iter().enumerate() {
        let v = v + 6;
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
        let mut query=c.prepare("SELECT node_id,x,y,width,height,weight FROM treemap_rects WHERE view=?1 AND leaf=1 ORDER BY node_id").unwrap();
        let rects: Vec<(i64, f64, f64, f64, f64, i64)> = query
            .query_map([v as i64], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
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
        let mut shares=c.prepare("SELECT r.width*r.height,r.weight,p.weight,(p.width-CASE WHEN p.header_height>0 THEN 16.0/4096 ELSE 0 END)*(p.height-p.header_height-CASE WHEN p.header_height>0 THEN 8.0/1024 ELSE 0 END) FROM treemap_rects r JOIN treemap_weights w ON w.key=r.key JOIN treemap_rects p ON p.view=r.view AND p.key=w.parent_id WHERE r.view=?1 AND r.leaf=1").unwrap();
        let mut shares = shares.query([v as i64]).unwrap();
        while let Some(row) = shares.next().unwrap() {
            let area: f64 = row.get(0).unwrap();
            let weight: i64 = row.get(1).unwrap();
            let parent_weight: i64 = row.get(2).unwrap();
            let content: f64 = row.get(3).unwrap();
            assert!((area / content - weight as f64 / parent_weight as f64).abs() < 1e-10);
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
        let bad:i64=c.query_row("SELECT count(*) FROM treemap_rects r JOIN treemap_weights w ON w.key=r.key JOIN treemap_rects p ON p.view=r.view AND p.key=w.parent_id WHERE r.view=?1 AND (r.x<p.x-1e-10 OR r.y<p.y-1e-10 OR r.x+r.width>p.x+p.width+1e-10 OR r.y+r.height>p.y+p.height+1e-10)",[v as i64],|r|r.get(0)).unwrap();
        assert_eq!(bad, 0);
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
    let orphan_parent: i64 = c
        .query_row(
            "SELECT parent_id FROM treemap_weights WHERE node_id=?1 AND leaf=0",
            [orphan[1..].parse::<i64>().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(orphan_parent, 0);
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM nodes WHERE path=?1",
            [import::normalize("C:\\root\\gap\\").0],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    let swap = node(&c, "C:\\root\\swap");
    let own=c.query_row("SELECT x,y,width,height,weight,value FROM treemap_rects WHERE view=8 AND leaf=1 AND node_id=?1",[swap[1..].parse::<i64>().unwrap()],|r|Ok((r.get::<_,f64>(0)?,r.get::<_,f64>(1)?,r.get::<_,f64>(2)?,r.get::<_,f64>(3)?,r.get::<_,i64>(4)?,r.get::<_,i64>(5)?))).unwrap();
    assert_eq!((own.4, own.5), (31, -31));
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
    assert_eq!(
        c.query_row("SELECT count(*) FROM treemap_rects", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn equal_siblings_have_deterministic_shared_edge_and_outer_edge_hits() {
    let csv = "文件名称,大小,分配\nC:\\a.bin,1,1\nC:\\b.bin,1,1\n";
    let (_dir, c) = build(csv, csv);
    let mut q = c
        .prepare(
            "SELECT node_id,x,y,width,height FROM treemap_rects WHERE view=7 AND leaf=1 ORDER BY x",
        )
        .unwrap();
    let r: Vec<(i64, f64, f64, f64, f64)> = q
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    assert_eq!(r.len(), 2);
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

fn raw_connection() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(store::SCHEMA).unwrap();
    c.execute_batch("CREATE TABLE export_roots(side INTEGER,node_id INTEGER,path TEXT,eligible INTEGER);CREATE TABLE comparison_nodes(node_id INTEGER PRIMARY KEY,status TEXT,expandable INTEGER);").unwrap();
    c
}
#[test]
fn leaf_aggregate_overflow_fails_and_cancelled_build_writes_nothing() {
    let c = raw_connection();
    for id in 1..=2 {
        c.execute("INSERT INTO nodes(id,path,parent_path,parent_id,name,depth,basename_key) VALUES(?1,?2,NULL,NULL,?2,0,?2)",params![id,format!("c:\\{id}.bin")]).unwrap();
        c.execute("INSERT INTO entries(side,node_id,path,kind,size,allocated,details,volume,extension) VALUES(0,?1,?2,'file',?3,0,'{}','c:','.bin')",params![id,format!("c:\\{id}.bin"),i64::MAX]).unwrap();
    }
    let error = global_treemap::materialize(&c, &JobControl::default()).unwrap_err();
    assert_eq!(error.code, "AGGREGATE_OVERFLOW");
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='treemap_frames'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
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
    let c = raw_connection();
    let tx = c.unchecked_transaction().unwrap();
    let mut n=c.prepare("INSERT INTO nodes(id,path,parent_path,parent_id,name,depth,basename_key) VALUES(?1,?2,NULL,?3,?2,?4,?2)").unwrap();
    let mut e=c.prepare("INSERT INTO entries(side,node_id,path,kind,size,allocated,details,volume,extension) VALUES(?1,?2,?3,?4,?5,?6,'{}','c:',?7)").unwrap();
    let mut status = c
        .prepare("INSERT INTO comparison_nodes VALUES(?1,'unchanged',?2)")
        .unwrap();
    for i in 0..100001i64 {
        let parent = 2 * i + 1;
        let leaf = parent + 1;
        n.execute(params![
            parent,
            format!("c:\\actual{i}"),
            Option::<i64>::None,
            0
        ])
        .unwrap();
        n.execute(params![leaf, format!("c:\\actual{i}\\f.bin"), parent, 1])
            .unwrap();
        status.execute(params![parent, 1]).unwrap();
        status.execute(params![leaf, 0]).unwrap();
        for side in 0..=1 {
            e.execute(params![
                side,
                parent,
                format!("c:\\actual{i}"),
                "directory",
                1000000,
                1000000,
                ""
            ])
            .unwrap();
            e.execute(params![
                side,
                leaf,
                format!("c:\\actual{i}\\f.bin"),
                "file",
                1,
                2,
                ".bin"
            ])
            .unwrap();
        }
    }
    drop(n);
    drop(e);
    drop(status);
    tx.commit().unwrap();
    global_treemap::materialize(&c, &JobControl::default()).unwrap();
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
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM treemap_rects WHERE view=7 AND leaf=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        100001
    );
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM treemap_rects WHERE view=7 AND leaf=0",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        200002
    );
}

#[test]
fn active_job_cancellation_interrupts_sql_and_rolls_back_materialization() {
    let c = raw_connection();
    c.execute_batch("WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id+1 FROM ids WHERE id<10000) INSERT INTO nodes(id,path,parent_id,name,depth,basename_key) SELECT id,'c:\\f'||id||'.bin',NULL,'f'||id,0,'f'||id FROM ids;INSERT INTO entries(side,node_id,path,kind,size,allocated,details,volume,extension) SELECT 0,id,path,'file',1,2,'{}','c:','.bin' FROM nodes;").unwrap();
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
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='treemap_frames'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
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
            let base = if matches!(metric, Metric::Size) { 0 } else { 3 };
            let v = if depth == 3 {
                base
            } else {
                (depth as i64 + 1) * 6 + base
            };
            let different:i64=c.query_row("SELECT count(*) FROM (SELECT key,leaf,x,y,width,height,weight,header_height FROM treemap_rects WHERE view=?1 EXCEPT SELECT key,leaf,x,y,width,height,weight,header_height FROM treemap_rects WHERE view=?2)",params![v,v+2],|r|r.get(0)).unwrap();
            assert_eq!(different, 0);
            let (blocks, sum): (i64, i64) = c
                .query_row(
                    "SELECT count(*),sum(weight) FROM treemap_rects WHERE view=?1 AND leaf=1",
                    [v],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!((blocks as u64, sum), (before.rendered_block_count, 100));
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
                let deeper:i64=c.query_row("SELECT count(*) FROM treemap_rects r JOIN treemap_weights w ON w.key=r.key WHERE r.view=?1 AND w.leaf=0 AND w.depth>?2",params![v,depth],|r|r.get(0)).unwrap();
                assert_eq!(deeper, 0);
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
        let v = if depth == 3 { 2 } else { 8 };
        let after_child = node(&c, "C:\\r\\swap\\child.txt");
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM treemap_rects WHERE view=?1 AND leaf=1 AND node_id=?2",
                params![v, after_child[1..].parse::<i64>().unwrap()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let before_child = node(&c, "C:\\r\\reverse\\child.bin");
        let after = global_treemap::get_frame(&c, Metric::Size, ChartMode::After, depth).unwrap();
        assert_eq!(after.rendered_block_count, 2);
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM treemap_rects WHERE view=?1 AND leaf=1 AND node_id=?2",
                params![v - 1, before_child[1..].parse::<i64>().unwrap()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
}
