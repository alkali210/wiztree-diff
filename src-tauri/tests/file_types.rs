use rusqlite::Connection;
use wiztree_diff_lib::{
    diff, file_extensions,
    import::{self, JobControl},
    store,
    types::{Metric, SnapshotSide, Status},
};
fn build(b: &str, a: &str) -> (tempfile::TempDir, Connection) {
    let dir = tempfile::TempDir::new().unwrap();
    let before = dir.path().join("b.csv");
    let after = dir.path().join("a.csv");
    std::fs::write(&before, b).unwrap();
    std::fs::write(&after, a).unwrap();
    let db = dir.path().join("d.sqlite");
    store::build_comparison(
        &before,
        &after,
        &db,
        "test",
        &JobControl::default(),
        &mut |_| {},
    )
    .unwrap();
    let conn = store::open_reader(&db).unwrap();
    (dir, conn)
}
fn id(c: &Connection, p: &str) -> String {
    format!(
        "n{}",
        c.query_row(
            "SELECT id FROM nodes WHERE path=?1",
            [import::normalize(p).0],
            |r| r.get::<_, i64>(0)
        )
        .unwrap()
    )
}
fn page(
    c: &Connection,
    parent: Option<&str>,
    side: SnapshotSide,
) -> wiztree_diff_lib::types::ExtensionPage {
    file_extensions::list_extensions(c, "test", parent, side, Metric::Size, None).unwrap()
}
#[test]
fn exact_leaf_scopes_hardlinks_zeros_orphans_type_changes_and_metadata() {
    let h = "文件名称,大小,分配,修改时间,属性,MFTRECNO\n";
    let b=format!("{h}C:\\root\\,999,999,root,D\nC:\\root\\sub\\,800,800,sub,D\nC:\\root\\sub\\a.PY,10,16,original,A,42\nC:\\root\\sub\\b.py,10,16,,,42\nC:\\root\\zero.PNG,0,0\nC:\\root\\gap\\lost.TXT,7,8\nC:\\root\\swap,3,4\n");
    let a=format!("{h}C:\\root\\,999,999,root,D\nC:\\root\\sub\\,800,800,sub,D\nC:\\root\\sub\\a.PY,10,16,new,A,42\nC:\\root\\sub\\b.py,10,16,,,42\nC:\\root\\zero.PNG,0,0\nC:\\root\\gap\\lost.TXT,7,8\nC:\\root\\swap\\,50,50\nC:\\root\\swap\\x.MP3,5,8\n");
    let (_dir, c) = build(&b, &a);
    let all = page(&c, None, SnapshotSide::Before);
    assert_eq!(
        (
            all.total.size.as_str(),
            all.total.allocated.as_str(),
            all.total.files
        ),
        ("30", "44", 5)
    );
    let all_after = page(&c, None, SnapshotSide::After);
    assert_eq!(
        (all_after.total.size.as_str(), all_after.total.files),
        ("32", 5)
    );
    let root = id(&c, "C:\\root\\");
    let scoped = page(&c, Some(&root), SnapshotSide::Before);
    assert_eq!((scoped.total.size.as_str(), scoped.total.files), ("23", 4));
    assert!(scoped
        .rows
        .iter()
        .any(|i| i.extension == ".png" && i.files == 1 && i.size == "0"));
    let scoped_after = page(&c, Some(&root), SnapshotSide::After);
    assert_eq!(
        (scoped_after.total.size.as_str(), scoped_after.total.files),
        ("25", 4)
    );
    let leaf = id(&c, "C:\\root\\sub\\a.py");
    let own = page(&c, Some(&leaf), SnapshotSide::Before);
    assert_eq!(own.rows.len(), 1);
    assert_eq!((own.total.size.as_str(), own.total.files), ("10", 1));
    let swap = id(&c, "C:\\root\\swap");
    let swap_before = page(&c, Some(&swap), SnapshotSide::Before);
    let swap_after = page(&c, Some(&swap), SnapshotSide::After);
    assert_eq!(swap_before.total.size, "3");
    assert_eq!(swap_before.rows[0].extension, "");
    assert_eq!(swap_after.total.size, "5");
    assert_eq!(swap_after.rows[0].extension, ".mp3");
    let sub = id(&c, "C:\\root\\sub\\");
    let children = diff::list_children(&c, "test", Some(&sub), false, None).unwrap();
    let row = children.rows.iter().find(|r| r.node_id == leaf).unwrap();
    assert_eq!(row.status, Status::Unchanged);
    assert_eq!(
        row.before.as_ref().unwrap().modified.as_deref(),
        Some("original")
    );
    assert_eq!(row.after.as_ref().unwrap().modified.as_deref(), Some("new"));
    assert_eq!(
        row.before.as_ref().unwrap().attributes.as_deref(),
        Some("A")
    );
    assert!(children
        .rows
        .iter()
        .find(|r| r.name == "b.py")
        .unwrap()
        .before
        .as_ref()
        .unwrap()
        .modified
        .is_none());
    for node in ["n999999", "garbage"] {
        assert_eq!(
            file_extensions::list_extensions(
                &c,
                "test",
                Some(node),
                SnapshotSide::Before,
                Metric::Size,
                None
            )
            .unwrap_err()
            .code,
            "INVALID_NODE"
        );
    }
}
#[test]
fn extension_rules_keep_actual_suffixes_separate() {
    let csv="文件名称,大小,分配\nC:\\a.DLL,1,2\nC:\\b.EXE,2,3\nC:\\c.PY,3,4\nC:\\archive.tar.gz,4,5\nC:\\.env,5,6\nC:\\README,6,7\nC:\\trailing.,0,0\n";
    let (_dir, c) = build(csv, csv);
    let stats = page(&c, None, SnapshotSide::Before);
    assert_eq!(stats.total_extensions, 5);
    for ext in [".dll", ".exe", ".py", ".gz", ""] {
        assert!(stats.rows.iter().any(|r| r.extension == ext));
    }
    assert_eq!(
        stats
            .rows
            .iter()
            .find(|r| r.extension.is_empty())
            .unwrap()
            .files,
        3
    );
    assert_eq!(stats.total.size, "21");
}
#[test]
fn all_extension_pages_are_exact_and_cursors_bind_every_scope() {
    let mut csv = "文件名称,大小,分配\nC:\\root\\,0,0\n".to_owned();
    for i in 0..451 {
        csv.push_str(&format!("C:\\root\\file.e{i:04},{},{}\n", i % 3, i % 5));
    }
    let (_dir, c) = build(&csv, &csv);
    let root = id(&c, "C:\\root\\");
    for side in [SnapshotSide::Before, SnapshotSide::After] {
        for metric in [Metric::Size, Metric::Allocated] {
            let first =
                file_extensions::list_extensions(&c, "test", Some(&root), side, metric, None)
                    .unwrap();
            assert_eq!(first.rows.len(), 200);
            assert_eq!(first.total_extensions, 451);
            assert_eq!(
                first.total.size,
                (0..451).map(|i| i % 3).sum::<i64>().to_string()
            );
            assert_eq!(
                first.total.allocated,
                (0..451).map(|i| i % 5).sum::<i64>().to_string()
            );
            assert_eq!(first.total.files, 451);
            let cursor = first.next_cursor.as_deref().unwrap();
            let other_side = if side == SnapshotSide::Before {
                SnapshotSide::After
            } else {
                SnapshotSide::Before
            };
            let other_metric = match metric {
                Metric::Size => Metric::Allocated,
                Metric::Allocated => Metric::Size,
            };
            for (parent, s, m) in [
                (None, side, metric),
                (Some(root.as_str()), other_side, metric),
                (Some(root.as_str()), side, other_metric),
            ] {
                assert_eq!(
                    file_extensions::list_extensions(&c, "test", parent, s, m, Some(cursor))
                        .unwrap_err()
                        .code,
                    "INVALID_CURSOR"
                );
            }
            let mut tampered: serde_json::Value = serde_json::from_str(cursor).unwrap();
            tampered["weight"] = serde_json::json!(99);
            assert_eq!(
                file_extensions::list_extensions(
                    &c,
                    "test",
                    Some(&root),
                    side,
                    metric,
                    Some(&tampered.to_string())
                )
                .unwrap_err()
                .code,
                "INVALID_CURSOR"
            );
            tampered["metric"] = serde_json::json!("files");
            assert_eq!(
                file_extensions::list_extensions(
                    &c,
                    "test",
                    Some(&root),
                    side,
                    metric,
                    Some(&tampered.to_string())
                )
                .unwrap_err()
                .code,
                "INVALID_CURSOR"
            );
            assert_eq!(
                file_extensions::list_extensions(
                    &c,
                    "stale",
                    Some(&root),
                    side,
                    metric,
                    Some(cursor)
                )
                .unwrap_err()
                .code,
                "STALE_COMPARISON"
            );
            let mut seen = std::collections::BTreeSet::new();
            let mut cursor = None;
            loop {
                let p = file_extensions::list_extensions(
                    &c,
                    "test",
                    Some(&root),
                    side,
                    metric,
                    cursor.as_deref(),
                )
                .unwrap();
                assert!(p.rows.len() <= 200);
                assert_eq!(p.total_extensions, 451);
                for row in p.rows {
                    assert!(seen.insert(row.extension));
                }
                cursor = p.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            assert_eq!(seen.len(), 451);
        }
    }
}
#[test]
fn absent_side_and_empty_directory_have_true_zero_totals() {
    let (_dir, c) = build(
        "文件名称,大小,分配\nC:\\empty\\,999,999\n",
        "文件名称,大小,分配\nC:\\zero.py,0,0\n",
    );
    for side in [SnapshotSide::Before, SnapshotSide::After] {
        let p = page(&c, Some(&id(&c, "C:\\empty\\")), side);
        assert!(p.rows.is_empty());
        assert_eq!(p.total.size, "0");
        assert_eq!(p.total.files, 0);
    }
    let p = page(&c, None, SnapshotSide::After);
    assert_eq!(p.total.size, "0");
    assert_eq!(p.total.files, 1);
    assert_eq!(p.total_extensions, 1);
}
#[test]
fn totals_overflow_and_cancel_do_not_publish_partial_statistics() {
    for csv in [
        "文件名称,大小,分配\nC:\\a.py,9223372036854775807,0\nC:\\b.py,1,0\n",
        "文件名称,大小,分配\nC:\\a.py,9223372036854775807,0\nC:\\b.png,1,0\n",
        "文件名称,大小,分配\nC:\\a.py,0,9223372036854775807\nC:\\b.png,0,1\n",
    ] {
        let d = tempfile::TempDir::new().unwrap();
        let p = d.path().join("input.csv");
        std::fs::write(&p, csv).unwrap();
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(store::SCHEMA).unwrap();
        let result = import::load(&c, &p, 0, &JobControl::default(), &mut |_| {})
            .and_then(|_| file_extensions::materialize(&c, &JobControl::default()));
        assert_eq!(result.unwrap_err().code, "AGGREGATE_OVERFLOW");
        let control = JobControl::default();
        control.cancel();
        assert_eq!(
            file_extensions::materialize(&c, &control).unwrap_err().code,
            "CANCELLED"
        );
    }
}

#[test]
fn a_file_scope_never_inherits_descendants_from_the_directory_side() {
    let (_dir, c) = build(
        "文件名称,大小,分配\nC:\\swap.py,3,4\nC:\\old\\child.exe,20,32\n",
        "文件名称,大小,分配\nc:/SWAP.PY/,999,999\nc:/SWAP.PY/child.exe,20,32\n",
    );
    let node = id(&c, "C:\\swap.py");
    let before = page(&c, Some(&node), SnapshotSide::Before);
    let after = page(&c, Some(&node), SnapshotSide::After);
    assert_eq!((before.total.size.as_str(), before.total.files), ("3", 1));
    assert_eq!(before.rows[0].extension, ".py");
    assert_eq!((after.total.size.as_str(), after.total.files), ("20", 1));
    assert_eq!(after.rows[0].extension, ".exe");
    assert_eq!(page(&c, None, SnapshotSide::Before).total.size, "23");
}

#[test]
fn global_and_directory_extension_totals_survive_bounded_cache_eviction() {
    let mut csv = String::from("文件名称,大小,分配\nC:\\root\\,0,0\n");
    for i in 0..8201 {
        csv.push_str(&format!("C:\\root\\a.type{i},3,4\n"));
    }
    csv.push_str("C:\\root\\revisited.type0,7,9\n");
    let (_dir, c) = build(&csv, &csv);
    let root = id(&c, "C:\\root\\");
    for parent in [None, Some(root.as_str())] {
        let result = page(&c, parent, SnapshotSide::After);
        assert_eq!(result.total_extensions, 8201);
        assert_eq!(
            (
                result.total.size.as_str(),
                result.total.allocated.as_str(),
                result.total.files
            ),
            ("24610", "32813", 8202)
        );
        let revisited = result
            .rows
            .iter()
            .find(|row| row.extension == ".type0")
            .unwrap();
        assert_eq!(
            (
                revisited.size.as_str(),
                revisited.allocated.as_str(),
                revisited.files
            ),
            ("10", "13", 2)
        );
    }
}

#[test]
fn global_totals_include_the_tail_of_a_multi_batch_import() {
    use std::io::Write;
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("multi-batch.csv");
    let mut csv = std::io::BufWriter::new(std::fs::File::create(&path).unwrap());
    writeln!(csv, "文件名称,大小,分配").unwrap();
    writeln!(csv, r"C:\root\,50015001,4096").unwrap();
    for size in 1..=10_001 {
        writeln!(csv, r"C:\root\file_{size}.bin,{size},16").unwrap();
    }
    csv.flush().unwrap();
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(store::SCHEMA).unwrap();
    import::load(&c, &path, 0, &JobControl::default(), &mut |_| {}).unwrap();
    file_extensions::materialize(&c, &JobControl::default()).unwrap();
    let totals: (i64, i64, i64) = c
        .query_row(
            "SELECT size,allocated,files FROM extension_totals WHERE side=0 AND node_id=0",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(totals, (50_015_001, 160_016, 10_001));
}
