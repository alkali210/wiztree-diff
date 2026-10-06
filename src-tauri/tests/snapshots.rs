use rusqlite::Connection;
use std::{fs, io::Write, sync::Arc, time::Duration};
use tempfile::TempDir;
use wiztree_diff_lib::{
    import::{self, JobControl},
    store,
};

fn source(dir: &TempDir, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, text).unwrap();
    path
}
fn import_one(
    text: &str,
) -> Result<
    (TempDir, Connection, wiztree_diff_lib::types::SourceSummary),
    wiztree_diff_lib::types::ApiError,
> {
    let dir = TempDir::new().unwrap();
    let path = source(&dir, "input.csv", text);
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(store::SCHEMA).unwrap();
    let summary = import::load(&conn, &path, 0, &JobControl::default(), &mut |_| {})?;
    Ok((dir, conn, summary))
}
#[test]
fn reordered_headers_bom_optional_trailing_columns_and_quoted_names() {
    let (_, conn, summary) = import_one("\u{feff}说明\r\n大小,文件名称,分配,MFTRECNO,CREATEDDATE,DRIVECAPACITY\r\n00012,\"C:/选定/a,\"\"中文\"\"\n文件\",02150400,000587612,原文\r\n12,C:/选定/,2150400\r\n").unwrap();
    assert_eq!(summary.rows, 2);
    assert_eq!(summary.description.as_deref(), Some("说明"));
    let (allocated, mft): (i64, String) = conn
        .query_row(
            "SELECT allocated,mft FROM entries WHERE kind='file'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(allocated, 2150400);
    assert_eq!(mft, "587612");
    let (_, _, summary) = import_one("分配,大小,文件名称\n0,0,C:\\empty\\\n").unwrap();
    assert_eq!((summary.files, summary.folders), (0, 1));
}
#[test]
fn rejects_invalid_fields_duplicate_paths_and_malformed_csv() {
    for (text, code) in [
        ("文件名称,大小,分配\nC:\\a,-1,0\n", "INVALID_FIELD"),
        ("文件名称,大小,分配\nC:\\a,1.0,0\n", "INVALID_FIELD"),
        (
            "文件名称,大小,分配\nC:\\a,9223372036854775808,0\n",
            "INVALID_FIELD",
        ),
        ("文件名称,大小,分配\nC:\\a,0\n", "INVALID_FIELD"),
        ("文件名称,大小,分配\n,0,0\n", "INVALID_FIELD"),
        ("文件名称,大小,分配\nC:\\a,0,0,extra\n", "INVALID_FIELD"),
        ("文件名称,大小,分配\nC:\\Ä,0,0\nc:/ä,0,0\n", "INVALID_FIELD"),
        ("文件名称,大小,分配\n\"C:\\a,0,0\n", "CSV_ERROR"),
        ("文件名称,大小,分配\n\"C:\\a\"suffix,0,0\n", "CSV_ERROR"),
        ("文件名称,大小,分配\nC:\\a\"bad,0,0\n", "CSV_ERROR"),
        ("文件名称,大小,分配\n\"C:\\a\" ,0,0\n", "CSV_ERROR"),
        ("文件名称,大小,分配\n///,0,0\n", "INVALID_FIELD"),
        ("文件名称,大小,分配,大小\nC:\\a,0,0,0\n", "INVALID_FIELD"),
        ("文件名称,分配\nC:\\a,0\n", "UNSUPPORTED_HEADER"),
        (
            "文件名称,大小,分配,MFTRECNO\nC:\\a,0,0,1x\n",
            "INVALID_FIELD",
        ),
        (
            "文件名称,大小,分配,MFTPARENTRECNO\nC:\\a,0,0,-1\n",
            "INVALID_FIELD",
        ),
        ("文件名称,大小,分配\n", "EMPTY_SNAPSHOT"),
        ("", "EMPTY_SNAPSHOT"),
        ("Name,Size,Allocated\nC:\\a,0,0\n", "UNSUPPORTED_HEADER"),
    ] {
        let error = match import_one(text) {
            Err(e) => e,
            Ok(_) => panic!("accepted {text:?}"),
        };
        assert_eq!(error.code, code, "{text:?}: {error:?}");
        assert!(error.source.is_some());
        if code == "INVALID_FIELD" || code == "CSV_ERROR" {
            assert!(error.record.is_some());
            assert!(error.column.is_some());
        }
    }
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad-utf8.csv");
    fs::write(
        &path,
        ["文件名称,大小,分配\n".as_bytes(), b"C:\\a\xff,0,0\n"].concat(),
    )
    .unwrap();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(store::SCHEMA).unwrap();
    assert_eq!(
        import::load(&conn, &path, 0, &JobControl::default(), &mut |_| {})
            .unwrap_err()
            .code,
        "CSV_ERROR"
    );
}
#[test]
fn importer_canonicalizes_optional_numbers_without_losing_text_or_precision() {
    let text = concat!(
        "文件名称,大小,分配,修改时间,属性,文件,文件夹,MFTRECNO,MFTPARENTRECNO,LASTACCESSDATE,CREATEDDATE,FOLDERSIZE,FOLDERALLOCATED,DRIVECAPACITY,FREESPACE,USEDSPACE,RESERVEDSPACE\n",
        "C:/ΟΣ,0001,0002,原样,00009,0000,0003,000184467440737095516160,0000,访问,创建,0009223372036854775807,0000,0004,0005,0006,0007\n"
    );
    let (_, conn, summary) = import_one(text).unwrap();
    assert_eq!((summary.rows, summary.files, summary.folders), (1, 1, 0));
    let (key, name, details, mft): (String, String, String, String) = conn
        .query_row(
            "SELECT n.path,n.name,e.details,e.mft FROM nodes n JOIN entries e ON e.node_id=n.id",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(key, "c:\\ος");
    assert_eq!(name, "ΟΣ");
    assert_eq!(mft, "184467440737095516160");
    let values: Vec<Option<String>> = serde_json::from_str(&details).unwrap();
    assert_eq!(
        values.iter().map(|v| v.as_deref()).collect::<Vec<_>>(),
        vec![
            Some("原样"),
            Some("00009"),
            Some("0"),
            Some("3"),
            Some("184467440737095516160"),
            Some("0"),
            Some("访问"),
            Some("创建"),
            Some("9223372036854775807"),
            Some("0"),
            Some("4"),
            Some("5"),
            Some("6"),
            Some("7"),
        ]
    );
    for column in [
        "文件",
        "文件夹",
        "FOLDERSIZE",
        "FOLDERALLOCATED",
        "DRIVECAPACITY",
        "FREESPACE",
        "USEDSPACE",
        "RESERVEDSPACE",
    ] {
        for value in ["-1", "1x", "9223372036854775808"] {
            let text = format!("文件名称,大小,分配,{column}\nC:\\a,0,0,{value}\n");
            let error = import_one(&text).err().unwrap();
            assert_eq!(error.code, "INVALID_FIELD");
            assert_eq!(error.column.as_deref(), Some(column));
            assert_eq!(error.record, Some(2));
        }
    }
}

#[test]
fn prefix_interning_survives_eviction_and_resolves_out_of_order_actual_parents() {
    let dir = TempDir::new().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(store::SCHEMA).unwrap();
    let mut text = "文件名称,大小,分配\n".to_owned();
    for i in 0..4101 {
        text.push_str(&format!(
            "C:/Root\\Dir{i}/File.TxT,1,2\nC:/Root\\Dir{i}/,1,2\n"
        ));
    }
    text.push_str("C:/Root/,4101,8202\n");
    let csv = source(&dir, "prefixes.csv", &text);
    let mut before = import::load(&conn, &csv, 0, &JobControl::default(), &mut |_| {}).unwrap();
    let mut after = import::load(&conn, &csv, 1, &JobControl::default(), &mut |_| {}).unwrap();
    for side in [0, 1] {
        let mut query = conn
            .prepare("SELECT path FROM entries WHERE side=?1 ORDER BY node_id")
            .unwrap();
        let mut rows = query.query([side]).unwrap();
        for original in text
            .lines()
            .skip(1)
            .map(|line| line.split(',').next().unwrap())
        {
            assert_eq!(
                rows.next().unwrap().unwrap().get::<_, String>(0).unwrap(),
                original
            );
        }
        assert!(rows.next().unwrap().is_none());
    }
    assert_eq!(
        conn.query_row("SELECT count(*) FROM entry_values", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        8203
    );
    store::finish_import(&conn, &mut before, &mut after, &JobControl::default()).unwrap();
    assert_eq!((before.root_count, after.root_count), (1, 1));
    assert_eq!((&*before.size, &*after.allocated), ("4101", "8202"));
    let root = id(&conn, "C:/Root/");
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM node_records WHERE parent_id=?1",
            [root[1..].parse::<i64>().unwrap()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        4101
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM node_records WHERE parent_path IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        8203
    );
}

#[test]
fn prefix_views_preserve_roots_unc_and_mixed_separators_without_inventing_nodes() {
    let originals = [
        "C:/",
        "//Server\\Share/",
        "//Server\\Share/MiXeD\\File.TXT",
        "//Server\\Share/MiXeD/",
        "relative\\FiLe",
        "plain",
    ];
    let text = format!(
        "文件名称,大小,分配\n{}",
        originals
            .iter()
            .map(|path| format!("{path},0,0\n"))
            .collect::<String>()
    );
    let (_dir, conn, _) = import_one(&text).unwrap();
    let mut statement = conn
        .prepare("SELECT n.path,e.path FROM nodes n JOIN entries e ON e.node_id=n.id ORDER BY n.id")
        .unwrap();
    let mut rows = statement.query([]).unwrap();
    for original in originals {
        let row = rows.next().unwrap().unwrap();
        assert_eq!(
            row.get::<_, String>(0).unwrap(),
            import::normalize(original).0
        );
        assert_eq!(row.get::<_, String>(1).unwrap(), original);
    }
    assert!(rows.next().unwrap().is_none());
}

#[test]
fn identical_snapshots_share_values_across_batches_without_changing_path_totals() {
    let dir = TempDir::new().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(store::SCHEMA).unwrap();
    let mut text = "文件名称,大小,分配,MFTRECNO\nC:\\Root\\,10005,20010,1\n".to_owned();
    for i in 0..10_005 {
        text.push_str(&format!("C:\\Root\\a{i}.TXT,1,2,184467440737095516160\n"));
    }
    let csv = source(&dir, "identical.csv", &text);
    for side in [0, 1] {
        let summary = import::load(&conn, &csv, side, &JobControl::default(), &mut |_| {}).unwrap();
        assert_eq!(
            (summary.rows, summary.files, summary.folders),
            (10_006, 10_005, 1)
        );
    }
    let counts: (i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM entry_values),(SELECT count(*) FROM snapshot_entries),(SELECT count(*) FROM snapshot_entries b JOIN snapshot_entries a ON a.node_id=b.node_id AND a.value_id=b.value_id WHERE b.side=0 AND a.side=1)", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(counts, (10_006, 20_012, 10_006));
    let totals: (i64, i64, i64) = conn.query_row(
        "SELECT sum(size),sum(allocated),sum(files) FROM file_category_stats WHERE category='text'", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(totals, (20_010, 40_020, 20_010));
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM nodes WHERE parent_id IS NOT NULL AND parent_path IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(fs::read_to_string(csv).unwrap(), text);
}

#[test]
fn sharing_compares_every_detail_and_preserves_original_path_spelling_and_kind() {
    let header = import::COLUMNS.join(",");
    let base = [
        "C:/Root/File.TXT",
        "1",
        "2",
        "modified",
        "00009",
        "0",
        "3",
        "184467440737095516160",
        "0",
        "accessed",
        "created",
        "4",
        "5",
        "6",
        "7",
        "8",
        "9",
    ];
    for (column, replacement) in [
        (0, "c:\\root\\FILE.txt"),
        (0, "C:/Root/File.TXT/"),
        (1, "10"),
        (2, "20"),
        (3, "other"),
        (4, "9"),
        (5, "1"),
        (6, "4"),
        (7, "184467440737095516161"),
        (8, "1"),
        (9, "other"),
        (10, "other"),
        (11, "40"),
        (12, "50"),
        (13, "60"),
        (14, "70"),
        (15, "80"),
        (16, "90"),
        (3, ""),
        (5, ""),
        (7, ""),
    ] {
        let dir = TempDir::new().unwrap();
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(store::SCHEMA).unwrap();
        let before = source(
            &dir,
            "before.csv",
            &format!("{header}\n{}\n", base.join(",")),
        );
        let mut changed = base;
        changed[column] = replacement;
        let after = source(
            &dir,
            "after.csv",
            &format!("{header}\n{}\n", changed.join(",")),
        );
        import::load(&conn, &before, 0, &JobControl::default(), &mut |_| {}).unwrap();
        import::load(&conn, &after, 1, &JobControl::default(), &mut |_| {}).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM nodes", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let expected_cold_values = if column == 1 || column == 2 { 1 } else { 2 };
        assert_eq!(
            conn.query_row("SELECT count(*) FROM entry_values", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            expected_cold_values,
            "column {column}: {replacement}"
        );
        let bytes: (i64, i64) = conn
            .query_row("SELECT size,allocated FROM entries WHERE side=1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(
            bytes,
            (
                changed[1].parse::<i64>().unwrap(),
                changed[2].parse::<i64>().unwrap()
            )
        );
        assert_eq!(
            conn.query_row("SELECT path FROM entries WHERE side=1", [], |r| r
                .get::<_, String>(0))
                .unwrap(),
            changed[0]
        );
    }
}

#[test]
fn shared_values_keep_side_specific_hardlinks_and_canonical_numeric_details() {
    let (_dir, conn) = comparison(
        "文件名称,大小,分配,文件,MFTRECNO\nC:\\r\\,2,4,2,1\nC:\\r\\one.txt,1,2,0000,000184467440737095516160\nC:\\r\\two.txt,1,2,0,184467440737095516160\n",
        "文件名称,大小,分配,文件,MFTRECNO\nC:\\r\\,1,2,1,1\nC:\\r\\one.txt,0001,0002,0,184467440737095516160\n",
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM entry_values", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        4
    );
    let one = diff::get_details(&conn, &id(&conn, "C:\\r\\one.txt")).unwrap();
    assert_eq!(one.before.as_ref().unwrap().hardlink_count, 2);
    assert_eq!(one.after.as_ref().unwrap().hardlink_count, 1);
    assert_eq!(
        one.before.unwrap().mft.as_deref(),
        Some("184467440737095516160")
    );
    let summaries = store::get_summary(&conn).unwrap();
    assert_eq!(
        (&*summaries.before.size, &*summaries.after.size),
        ("2", "1")
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM nodes WHERE parent_path IS NOT NULL",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}

#[test]
fn importer_reuses_ids_across_committed_batches_and_rejects_late_conflicts() {
    let dir = TempDir::new().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(store::SCHEMA).unwrap();
    let mut text = "文件名称,大小,分配\n".to_owned();
    for i in 0..10_005 {
        text.push_str(&format!("C:\\Root\\a{i}.TXT,1,2\n"));
    }
    let before = source(&dir, "before.csv", &text);
    let summary = import::load(&conn, &before, 0, &JobControl::default(), &mut |_| {}).unwrap();
    assert_eq!(summary.rows, 10_005);
    let after = source(&dir, "after.csv", &text.replace("C:\\Root\\", "c:/root/"));
    import::load(&conn, &after, 1, &JobControl::default(), &mut |_| {}).unwrap();
    let counts: (i64, i64, i64) = conn.query_row(
        "SELECT (SELECT count(*) FROM nodes),(SELECT count(*) FROM entries),(SELECT count(*) FROM entries a JOIN entries b ON a.node_id=b.node_id WHERE a.side=0 AND b.side=1)", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(counts, (10_005, 20_010, 10_005));
    let category_counts: (i64, i64, i64) = conn.query_row(
        "SELECT sum(size),sum(allocated),sum(files) FROM file_category_stats WHERE category='text'", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    ).unwrap();
    assert_eq!(category_counts, (20_010, 40_020, 20_010));
    assert!(conn.is_autocommit());
    text.push_str("c:/ROOT/A0.txt,1,2\n");
    let conflict = source(&dir, "conflict.csv", &text);
    let fresh = Connection::open_in_memory().unwrap();
    fresh.execute_batch(store::SCHEMA).unwrap();
    let error =
        import::load(&fresh, &conflict, 0, &JobControl::default(), &mut |_| {}).unwrap_err();
    assert_eq!(error.code, "INVALID_FIELD");
    assert_eq!(error.record, Some(10_007));
    assert!(fresh.is_autocommit());
    assert_eq!(
        fresh
            .query_row("SELECT count(*) FROM entries", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        10_000
    );
    assert_eq!(
        fresh
            .query_row("SELECT count(*) FROM file_category_stats", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn roots_are_windows_unc_and_nonnested_with_count_warnings() {
    for (raw, key, parent) in [
        ("C:/", "c:\\", None),
        ("C:\\A\\", "c:\\a", Some("c:\\")),
        ("\\\\Server\\Share\\", "\\\\server\\share\\", None),
        (
            "\\\\SERVER\\SHARE\\A",
            "\\\\server\\share\\a",
            Some("\\\\server\\share\\"),
        ),
    ] {
        let normalized = import::normalize(raw);
        assert_eq!(normalized.0, key);
        assert_eq!(normalized.1.as_deref(), parent);
    }
    assert_eq!(import::volume("\\\\server\\share\\a"), "\\\\server\\share");
    let (_, conn, mut before) = import_one("文件名称,大小,分配,文件,文件夹\nC:\\selected\\child\\,10,20,0,0\nC:\\selected\\,10,20,99,99\nC:\\orphan,5,6\n").unwrap();
    let dir = TempDir::new().unwrap();
    let after_path = source(
        &dir,
        "after.csv",
        "文件名称,大小,分配\nC:\\selected\\,10,20\nC:\\selected\\child\\,10,20\nC:\\orphan,5,6\n",
    );
    let mut after =
        import::load(&conn, &after_path, 1, &JobControl::default(), &mut |_| {}).unwrap();
    let warnings =
        store::finish_import(&conn, &mut before, &mut after, &JobControl::default()).unwrap();
    assert_eq!(before.size, "15");
    assert_eq!(before.allocated, "26");
    assert_eq!(before.roots.len(), 2);
    assert!(warnings.iter().any(|s| s.contains("1 个文件")));
    assert!(warnings.iter().any(|s| s.contains("计数")));
    assert!(!warnings.iter().any(|s| s.contains("根范围不同")));
}
#[test]
fn rejects_file_parent_and_root_sum_overflow() {
    for (text, code) in [
        (
            "文件名称,大小,分配\nC:\\a,1,1\nC:\\a\\b,1,1\n",
            "INVALID_HIERARCHY",
        ),
        (
            "文件名称,大小,分配\nC:\\a\\,9223372036854775807,0\nD:\\b\\,1,0\n",
            "AGGREGATE_OVERFLOW",
        ),
    ] {
        let (_, conn, mut before) = import_one(text).unwrap();
        let mut after = before.clone();
        assert_eq!(
            store::finish_import(&conn, &mut before, &mut after, &JobControl::default())
                .unwrap_err()
                .code,
            code
        );
    }
}
#[test]
fn cancellation_and_source_mutation_are_detected() {
    let dir = TempDir::new().unwrap();
    let mut text = "文件名称,大小,分配\n".to_owned();
    for i in 0..2000 {
        text.push_str(&format!("C:\\a{i},0,0\n"));
    }
    let path = source(&dir, "source.csv", &text);
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(store::SCHEMA).unwrap();
    let control = JobControl::default();
    let error = import::load(&conn, &path, 0, &control, &mut |_| control.cancel()).unwrap_err();
    assert_eq!(error.code, "CANCELLED");
    let error = import::load(&conn, &path, 0, &JobControl::default(), &mut |_| {
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"\n")
            .unwrap();
    })
    .unwrap_err();
    assert_eq!(error.code, "SOURCE_CHANGED");
}
#[test]
fn cancellation_interrupts_long_sql() {
    let conn = Connection::open_in_memory().unwrap();
    let control = Arc::new(JobControl::default());
    *control.interrupt.lock().unwrap() = Some(conn.get_interrupt_handle());
    let signal = control.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        signal.cancel();
    });
    let error = conn.query_row("WITH RECURSIVE n(x) AS (VALUES(0) UNION ALL SELECT x+1 FROM n WHERE x<1000000000) SELECT sum(x) FROM n", [], |r| r.get::<_,i64>(0)).unwrap_err();
    thread.join().unwrap();
    assert!(
        matches!(error, rusqlite::Error::SqliteFailure(ref e, _) if e.code == rusqlite::ErrorCode::OperationInterrupted)
    );
    assert_eq!(control.check().unwrap_err().code, "CANCELLED");
}
use wiztree_diff_lib::{
    diff, global_treemap,
    types::{ChartMode, Metric, SnapshotSide, Status},
};
#[test]
fn root_pages_cover_file_only_snapshots_and_preserve_each_side_spelling() {
    let mut before = "文件名称,大小,分配\n".to_owned();
    let mut after = before.clone();
    for i in (0..450).rev() {
        before.push_str(&format!("C:\\Orphans\\Ä{i:03},9007199254740993,2\n"));
        after.push_str(&format!("c:/orphans/ä{i:03},9007199254740994,3\n"));
    }
    let (_dir, conn) = comparison(&before, &after);
    let summary = store::get_summary(&conn).unwrap();
    assert!(!summary.warnings.iter().any(|w| w.contains("根范围不同")));
    for side in [SnapshotSide::Before, SnapshotSide::After] {
        let source = if side == SnapshotSide::Before {
            &summary.before
        } else {
            &summary.after
        };
        assert_eq!(source.root_count, 450);
        assert_eq!(source.roots.len(), 200);
        assert!(source.roots_truncated);
        let mut cursor = None;
        let mut seen = std::collections::BTreeSet::new();
        let mut previous = None;
        let mut pages = Vec::new();
        loop {
            let page = diff::list_roots(&conn, "test", side, cursor.as_deref()).unwrap();
            assert_eq!(page.total_roots, 450);
            pages.push(page.rows.len());
            for row in page.rows {
                assert!(seen.insert(row.node_id));
                let key = import::normalize(&row.path).0;
                if let Some(last) = previous {
                    assert!(last < key);
                }
                previous = Some(key);
                assert_eq!(row.kind, "file");
                if side == SnapshotSide::Before {
                    assert!(row.path.starts_with("C:\\Orphans\\Ä"));
                    assert_eq!(
                        (row.size.as_str(), row.allocated.as_str()),
                        ("9007199254740993", "2")
                    );
                } else {
                    assert!(row.path.starts_with("c:/orphans/ä"));
                    assert_eq!(
                        (row.size.as_str(), row.allocated.as_str()),
                        ("9007199254740994", "3")
                    );
                }
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(seen.len(), 450);
        assert_eq!(pages, vec![200, 200, 50]);
    }
    let first = diff::list_roots(&conn, "test", SnapshotSide::Before, None).unwrap();
    let cursor = first.next_cursor.unwrap();
    assert_eq!(
        diff::list_roots(&conn, "stale", SnapshotSide::Before, None)
            .unwrap_err()
            .code,
        "STALE_COMPARISON"
    );
    assert_eq!(
        diff::list_roots(&conn, "test", SnapshotSide::After, Some(&cursor))
            .unwrap_err()
            .code,
        "INVALID_CURSOR"
    );
    for (field, value) in [
        ("comparison", serde_json::json!("different")),
        ("path", serde_json::json!("nonexistent")),
        ("node", serde_json::json!(0)),
        ("node", serde_json::json!(999999)),
        ("extra", serde_json::json!(true)),
    ] {
        let mut invalid: serde_json::Value = serde_json::from_str(&cursor).unwrap();
        invalid[field] = value;
        assert_eq!(
            diff::list_roots(
                &conn,
                "test",
                SnapshotSide::Before,
                Some(&invalid.to_string())
            )
            .unwrap_err()
            .code,
            "INVALID_CURSOR"
        );
    }
    for malformed in ["garbage", "{}", "[]", "null"] {
        assert_eq!(
            diff::list_roots(&conn, "test", SnapshotSide::Before, Some(malformed))
                .unwrap_err()
                .code,
            "INVALID_CURSOR"
        );
    }
}

#[test]
fn root_set_difference_after_preview_and_exact_page_boundary_are_not_hidden() {
    let mut before = "文件名称,大小,分配\n".to_owned();
    for i in 0..200 {
        before.push_str(&format!("C:\\root{i:03}\\,1,2\n"));
    }
    let (_dir, conn) = comparison(&before, &before);
    let summary = store::get_summary(&conn).unwrap();
    assert_eq!(summary.before.root_count, 200);
    assert!(!summary.before.roots_truncated);
    let page = diff::list_roots(&conn, "test", SnapshotSide::Before, None).unwrap();
    assert_eq!(page.rows.len(), 200);
    assert!(page.next_cursor.is_none());
    let after = format!("{before}D:\\tail-after\\,3,4\n");
    before.push_str("D:\\tail-before\\,5,6\n");
    let (_dir, conn) = comparison(&before, &after);
    let summary = store::get_summary(&conn).unwrap();
    assert_eq!(summary.before.root_count, 201);
    assert_eq!(summary.after.root_count, 201);
    assert!(summary.before.roots_truncated && summary.after.roots_truncated);
    assert_eq!(
        summary
            .before
            .roots
            .iter()
            .map(|r| &r.path)
            .collect::<Vec<_>>(),
        summary
            .after
            .roots
            .iter()
            .map(|r| &r.path)
            .collect::<Vec<_>>()
    );
    assert!(summary.warnings.iter().any(|w| w.contains("根范围不同")));
    for (side, path) in [
        (SnapshotSide::Before, "D:\\tail-before\\"),
        (SnapshotSide::After, "D:\\tail-after\\"),
    ] {
        let first = diff::list_roots(&conn, "test", side, None).unwrap();
        let tail = diff::list_roots(&conn, "test", side, first.next_cursor.as_deref()).unwrap();
        assert_eq!(tail.total_roots, 201);
        assert_eq!(tail.rows.len(), 1);
        assert_eq!(tail.rows[0].path, path);
        assert!(tail.next_cursor.is_none());
    }
}

#[test]
fn unc_nested_roots_are_listed_but_not_double_counted() {
    let before = "文件名称,大小,分配\n\\\\Server\\Share\\,10,20\n\\\\Server\\Share\\gap\\中间\\child\\,10,20\n\\\\Server\\Share\\gap\\orphan,7,8\nD:\\legit-file,3,4\n";
    let after = before.replace("Server\\Share", "SERVER\\SHARE");
    let (_dir, conn) = comparison(before, &after);
    let summary = store::get_summary(&conn).unwrap();
    for source in [&summary.before, &summary.after] {
        assert_eq!(source.root_count, 4);
        assert!(!source.roots_truncated);
        assert_eq!(
            (source.size.as_str(), source.allocated.as_str()),
            ("13", "24")
        );
    }
    for side in [SnapshotSide::Before, SnapshotSide::After] {
        let roots = diff::list_roots(&conn, "test", side, None).unwrap();
        assert_eq!(roots.total_roots, 4);
        assert_eq!(roots.rows.len(), 4);
        assert!(roots.rows.iter().any(|r| r.path.contains("child")));
    }
    assert!(!summary.warnings.iter().any(|w| w.contains("根范围不同")));
    assert!(summary.warnings.iter().any(|w| w.contains("2 个嵌套")));
    let chart = global_treemap::get_frame(&conn, Metric::Size, ChartMode::Before, 0).unwrap();
    assert_eq!(chart.weight_total, "10");
    assert_eq!(chart.exported_total, "13");
    assert_eq!(chart.file_count, 2);
}

fn comparison(before: &str, after: &str) -> (TempDir, Connection) {
    let dir = TempDir::new().unwrap();
    let b = source(&dir, "before.csv", before);
    let a = source(&dir, "after.csv", after);
    let db = dir.path().join("comparison.sqlite");
    store::build_comparison(&b, &a, &db, "test", &JobControl::default(), &mut |_| {}).unwrap();
    let conn = store::open_reader(&db).unwrap();
    (dir, conn)
}
fn id(conn: &Connection, path: &str) -> String {
    let key = import::normalize(path).0;
    let parts = import::canonical_parts(&key);
    format!(
        "n{}",
        conn.query_row(
            "SELECT n.id FROM path_prefixes p JOIN node_records n ON n.prefix_id=p.id WHERE p.path=?1 AND n.basename_key=?2",
            parts,
            |r| r.get::<_, i64>(0)
        )
        .unwrap()
    )
}
#[test]
fn compact_hot_fields_preserve_precise_side_bytes_while_sharing_cold_metadata() {
    let (_dir, conn) = comparison(
        "文件名称,大小,分配,MFTRECNO\nC:\\Big.TXT,9007199254740993,9223372036854775806,184467440737095516160\n",
        "文件名称,大小,分配,MFTRECNO\nC:\\Big.TXT,9007199254740994,9223372036854775807,184467440737095516160\n",
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM entry_values", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let details = diff::get_details(&conn, &id(&conn, "C:\\Big.TXT")).unwrap();
    assert_eq!(
        (&*details.size_delta, &*details.allocated_delta),
        ("1", "1")
    );
    assert_eq!(details.before.as_ref().unwrap().size, "9007199254740993");
    assert_eq!(details.after.as_ref().unwrap().size, "9007199254740994");
    assert_eq!(
        details.after.as_ref().unwrap().allocated,
        "9223372036854775807"
    );
    for entry in [details.before.unwrap(), details.after.unwrap()] {
        assert_eq!(entry.kind, "file");
        assert_eq!(entry.path, "C:\\Big.TXT");
        assert_eq!(entry.mft.as_deref(), Some("184467440737095516160"));
        assert_eq!(entry.hardlink_count, 1);
    }
}

#[test]
fn ranked_tree_pages_preserve_binary_unicode_order_and_validate_ranked_cursors() {
    let header = "文件名称,大小,分配\nC:\\r\\,0,0\nC:\\r\\left\\,0,0\nC:\\r\\right\\,0,0\n";
    let mut before = header.to_owned();
    let mut after = header.to_owned();
    let stems = ["Ä", "Σ", "é", "Ａ", "😀", "中", "Z", "a"];
    for i in (0..451).rev() {
        let name = format!("{}-{i:04}.TxT", stems[i % stems.len()]);
        for parent in ["left", "right"] {
            before.push_str(&format!("C:\\r\\{parent}\\{name},1,1\n"));
            after.push_str(&format!(
                "C:\\r\\{parent}\\{name},{},1\n",
                if parent == "left" && i % 2 == 0 { 2 } else { 1 }
            ));
        }
    }
    let (_dir, conn) = comparison(&before, &after);
    let left = id(&conn, "C:\\r\\left\\");
    let parent = left[1..].parse::<i64>().unwrap();
    for changes in [false, true] {
        let mut expected_query = conn.prepare("SELECT n.id,n.name FROM node_records n JOIN comparison_nodes c ON c.node_id=n.id WHERE n.parent_id=?1 AND (?2=0 OR c.has_changes=1) ORDER BY n.basename_key COLLATE BINARY,n.id").unwrap();
        let expected = expected_query
            .query_map((parent, changes), |r| {
                Ok((format!("n{}", r.get::<_, i64>(0)?), r.get::<_, String>(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut actual = Vec::new();
        let mut cursor = None;
        loop {
            let page = diff::list_children(&conn, "test", Some(&left), changes, cursor.as_deref())
                .unwrap();
            if cursor.is_none() {
                let mut corrupt: serde_json::Value =
                    serde_json::from_str(page.next_cursor.as_deref().unwrap()).unwrap();
                corrupt["rank"] = 0.into();
                assert_eq!(
                    diff::list_children(
                        &conn,
                        "test",
                        Some(&left),
                        changes,
                        Some(&corrupt.to_string())
                    )
                    .unwrap_err()
                    .code,
                    "INVALID_CURSOR"
                );
            }
            actual.extend(page.rows.into_iter().map(|r| (r.node_id, r.name)));
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
        }
        assert_eq!(actual, expected);
    }
    let mismatch: i64 = conn.query_row("SELECT count(*) FROM comparison_nodes a JOIN node_records na ON na.id=a.node_id JOIN node_records nb ON nb.basename_key=na.basename_key JOIN comparison_nodes b ON b.node_id=nb.id WHERE a.sort_rank!=b.sort_rank", [], |r| r.get(0)).unwrap();
    assert_eq!(mismatch, 0);
}

#[test]
fn exact_statuses_internal_changes_and_signed_charts() {
    let header = "文件名称,大小,分配,修改时间,FOLDERSIZE\n";
    let b=format!("{header}C:\\test\\,1250,1472,old,950\nC:\\test\\cancel\\,300,384,old,300\nC:\\test\\cancel\\a,100,128\nC:\\test\\cancel\\b,200,256\nC:\\test\\keep,100,128,old\nC:\\test\\grow,200,256\nC:\\test\\shrink,500,512\nC:\\test\\remove,50,64\nC:\\test\\allocOnly,100,128\nC:\\test\\zero,0,0\n");
    let a=format!("{header}C:\\test\\cancel\\b,150,192\nC:\\test\\cancel\\a,150,192\nC:\\test\\,1275,1792,new,975\nC:\\test\\cancel\\,300,384,new,300\nC:\\test\\keep,100,128,new\nC:\\test\\grow,300,384\nC:\\test\\shrink,400,512\nC:\\test\\add,75,128\nC:\\test\\allocOnly,100,256\nC:\\test\\zero,0,0\n");
    let (_dir, conn) = comparison(&b, &a);
    let summary = store::get_summary(&conn).unwrap();
    assert_eq!(
        (summary.before.size.as_str(), summary.after.size.as_str()),
        ("1250", "1275")
    );
    assert_eq!(
        (
            summary.before.allocated.as_str(),
            summary.after.allocated.as_str()
        ),
        ("1472", "1792")
    );
    assert_eq!(
        (
            summary.statuses.files.added,
            summary.statuses.files.removed,
            summary.statuses.files.modified,
            summary.statuses.files.unchanged
        ),
        (1, 1, 5, 2)
    );
    let root = id(&conn, "C:\\test\\");
    let cancel = id(&conn, "C:\\test\\cancel\\");
    let page = diff::list_children(&conn, "test", Some(&root), false, None).unwrap();
    assert_eq!(page.total_children, 8);
    for row in &page.rows {
        match row.name.as_str() {
            "keep" | "zero" => assert_eq!(row.status, Status::Unchanged),
            "cancel" => {
                assert_eq!(row.status, Status::Unchanged);
                assert!(row.has_changes);
                assert_eq!(row.changed_descendant_count, 2);
            }
            "add" => assert_eq!(row.status, Status::Added),
            "remove" => assert_eq!(row.status, Status::Removed),
            _ => assert_eq!(row.status, Status::Modified),
        }
    }
    let filtered = diff::list_children(&conn, "test", Some(&root), true, None).unwrap();
    assert_eq!(filtered.rows.len(), 6);
    assert!(filtered.rows.iter().any(|r| r.node_id == cancel));
    assert_eq!(
        diff::list_children(&conn, "test", None, true, None)
            .unwrap()
            .rows
            .len(),
        1
    );
    for (metric, positive, negative, net) in [
        (Metric::Size, "225", "200", "25"),
        (Metric::Allocated, "448", "128", "320"),
    ] {
        let chart = global_treemap::get_frame(&conn, metric, ChartMode::Delta, 0).unwrap();
        assert_eq!(chart.positive_total, positive);
        assert_eq!(chart.negative_total, negative);
        assert_eq!(chart.net_delta, net);
    }
    let cancel_children = diff::list_children(&conn, "test", Some(&cancel), true, None).unwrap();
    assert_eq!(cancel_children.total_children, 2);
    for field in [true, false] {
        let sum: i64 = cancel_children
            .rows
            .iter()
            .map(|r| {
                if field {
                    &r.size_delta
                } else {
                    &r.allocated_delta
                }
            })
            .map(|v| v.parse::<i64>().unwrap())
            .sum();
        assert_eq!(sum, 0);
    }
    let keep = diff::get_details(&conn, &id(&conn, "C:\\test\\keep")).unwrap();
    assert_eq!(keep.size_delta, "0");
    assert_eq!(keep.before.unwrap().modified.as_deref(), Some("old"));
}
#[test]
fn type_replacement_exact_large_integers_and_mft_are_path_based() {
    let (_dir,conn)=comparison("文件名称,大小,分配,MFTRECNO\nC:\\t\\,9007199254740993,0\nC:\\t\\swap,0,0\nC:\\t\\Ä,9007199254740993,0,018446744073709551615\nC:\\t\\link,0,0,18446744073709551615\nD:\\other,0,0,18446744073709551615\n", "文件名称,大小,分配,MFTRECNO\nC:\\t\\,9007199254740994,0\nC:\\t\\swap\\,0,0\nC:\\t\\ä,9007199254740994,0,2\nC:\\t\\link,0,0,3\nD:\\other,0,0,3\n");
    let root = id(&conn, "c:\\t\\");
    let page = diff::list_children(&conn, "test", Some(&root), false, None).unwrap();
    let swap = page.rows.iter().find(|r| r.name == "swap").unwrap();
    assert_eq!(swap.status, Status::TypeChanged);
    assert!(swap.expandable);
    let details = diff::get_details(&conn, &id(&conn, "c:\\t\\ä")).unwrap();
    assert_eq!(details.size_delta, "1");
    assert_eq!(details.before.as_ref().unwrap().size, "9007199254740993");
    assert_eq!(
        details.before.as_ref().unwrap().mft.as_deref(),
        Some("18446744073709551615")
    );
    assert_eq!(details.before.unwrap().hardlink_count, 2);
    assert_eq!(
        diff::get_details(&conn, &id(&conn, "D:\\other"))
            .unwrap()
            .before
            .unwrap()
            .hardlink_count,
        1
    );
    assert_eq!(
        page.rows.iter().find(|r| r.name == "link").unwrap().status,
        Status::Unchanged
    );
    assert!(diff::list_children(&conn, "stale", Some(&root), false, None).is_err());
    assert!(diff::get_details(&conn, "n999999").is_err());
    assert!(
        diff::list_children(&conn, "test", Some(&id(&conn, "c:\\t\\link")), false, None).is_err()
    );
}
#[test]
fn tree_keyset_pagination_and_complete_signed_leaf_geometry_are_exact() {
    let mut b = "文件名称,大小,分配\nC:\\wide\\,1000,1000\n".to_owned();
    let mut a = b.clone();
    for i in 0..450 {
        b.push_str(&format!("C:\\wide\\f{i:04},10,10\n"));
        a.push_str(&format!(
            "C:\\wide\\f{i:04},{0},{0}\n",
            if i % 2 == 0 { 11 } else { 9 }
        ));
    }
    let (_dir, conn) = comparison(&b, &a);
    let root = id(&conn, "C:\\wide\\");
    let mut cursor = None;
    let mut names = std::collections::HashSet::new();
    let mut first_cursor = None;
    loop {
        let page =
            diff::list_children(&conn, "test", Some(&root), false, cursor.as_deref()).unwrap();
        assert_eq!(page.total_children, 450);
        assert!(page.rows.len() <= 200);
        for row in page.rows {
            assert!(names.insert(row.name));
        }
        if first_cursor.is_none() {
            first_cursor = page.next_cursor.clone();
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(names.len(), 450);
    assert!(diff::list_children(&conn, "test", None, false, first_cursor.as_deref()).is_err());
    assert!(
        diff::list_children(&conn, "test", Some(&root), true, first_cursor.as_deref()).is_err()
    );
    assert!(diff::list_children(&conn, "test", Some(&root), false, Some("garbage")).is_err());
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
            let chart = global_treemap::get_frame(&conn, metric, mode, 0).unwrap();
            assert_eq!(chart.file_count, 450);
            assert_eq!(chart.visible_file_count, 450);
            for i in 0..450 {
                assert!(global_treemap::get_bounds(
                    &conn,
                    metric,
                    mode,
                    &id(&conn, &format!("C:\\wide\\f{i:04}")),
                    0
                )
                .unwrap()
                .is_some());
            }
            if matches!(mode, ChartMode::Delta) {
                assert_eq!(chart.positive_total, "225");
                assert_eq!(chart.negative_total, "225");
                assert_eq!(chart.net_delta, "0");
            } else {
                assert_eq!(chart.weight_total, "4500");
            }
        }
    }
}
#[test]
fn failed_and_cancelled_builds_remove_only_their_derived_database() {
    let dir = TempDir::new().unwrap();
    let valid = source(&dir, "valid.csv", "文件名称,大小,分配\nC:\\x\\,0,0\n");
    let ready = dir.path().join("ready.sqlite");
    store::build_comparison(
        &valid,
        &valid,
        &ready,
        "old",
        &JobControl::default(),
        &mut |_| {},
    )
    .unwrap();
    for text in [
        "文件名称,大小,分配\nC:\\a,0,0\nc:\\A,0,0\n",
        "文件名称,大小,分配\nC:\\a,-1,0\n",
        "文件名称,大小,分配\nC:\\x\\,0,0\nC:\\x\\a,9223372036854775807,0\nC:\\x\\b,1,0\n",
    ] {
        let bad = source(&dir, "bad.csv", text);
        let failed = dir.path().join("failed.sqlite");
        assert!(store::build_comparison(
            &bad,
            &valid,
            &failed,
            "new",
            &JobControl::default(),
            &mut |_| {}
        )
        .is_err());
        assert!(!failed.exists());
        assert_eq!(
            store::get_summary(&store::open_reader(&ready).unwrap())
                .unwrap()
                .comparison_id,
            "old"
        );
    }
    for phase in ["before", "indexing", "comparing"] {
        let control = JobControl::default();
        let failed = dir.path().join(format!("cancel-{phase}.sqlite"));
        assert_eq!(
            store::build_comparison(&valid, &valid, &failed, "new", &control, &mut |p| {
                if p.phase == phase {
                    control.cancel();
                }
            })
            .unwrap_err()
            .code,
            "CANCELLED"
        );
        assert!(!failed.exists());
    }
    let unfinished = dir.path().join("unfinished.sqlite");
    Connection::open(&unfinished)
        .unwrap()
        .execute_batch(store::SCHEMA)
        .unwrap();
    assert!(store::open_reader(&unfinished).is_err());
}
#[test]
fn missing_intermediate_roots_remain_real_roots_without_fabricated_children() {
    let before = "文件名称,大小,分配,文件,文件夹\nC:\\r\\,10,20,1,2\nC:\\r\\gap\\child\\,10,20,1,0\nC:\\r\\gap\\child\\f,10,20\nD:\\other\\,5,8,0,0\n";
    let after = before.replace("child\\f,10,20", "child\\f,11,21");
    let (_dir, conn) = comparison(before, &after);
    let summary = store::get_summary(&conn).unwrap();
    assert_eq!(summary.before.roots.len(), 3);
    assert_eq!(summary.before.size, "15");
    assert_eq!(summary.before.allocated, "28");
    assert!(summary.warnings.iter().any(|s| s.contains("中间父目录")));
    assert!(summary.warnings.iter().any(|s| s.contains("计数")));
    let root = id(&conn, "C:\\r\\");
    let child = id(&conn, "C:\\r\\gap\\child\\");
    let roots = diff::list_children(&conn, "test", None, false, None).unwrap();
    assert_eq!(roots.total_children, 3);
    assert!(
        !roots
            .rows
            .iter()
            .find(|r| r.node_id == root)
            .unwrap()
            .has_changes
    );
    assert!(
        roots
            .rows
            .iter()
            .find(|r| r.node_id == child)
            .unwrap()
            .has_changes
    );
    assert_eq!(
        diff::list_children(&conn, "test", Some(&root), false, None)
            .unwrap()
            .total_children,
        0
    );
    assert_eq!(
        diff::list_children(&conn, "test", Some(&child), false, None)
            .unwrap()
            .total_children,
        1
    );
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::Before, ChartMode::After] {
            let chart = global_treemap::get_frame(&conn, metric, mode, 0).unwrap();
            assert_eq!(chart.file_count, 1);
            assert_ne!(chart.exported_total, chart.weight_total);
            assert!(global_treemap::get_bounds(&conn, metric, mode, &child, 0)
                .unwrap()
                .is_some());
            assert!(global_treemap::get_bounds(&conn, metric, mode, &root, 0)
                .unwrap()
                .is_none());
        }
        let delta = global_treemap::get_frame(&conn, metric, ChartMode::Delta, 0).unwrap();
        assert_eq!(delta.weight_total, "1");
        assert_eq!(delta.net_delta, "1");
    }
}
#[test]
fn workspace_scope_eligibility_never_changes_raw_node_deltas() {
    let before = "文件名称,大小,分配\nC:\\r\\child\\,10,20\nD:\\other\\,5,8\n";
    let after = "文件名称,大小,分配\nC:\\r\\,100,200\nC:\\r\\child\\,10,20\nD:\\other\\,7,11\n";
    let (_dir, conn) = comparison(before, after);
    let summary = store::get_summary(&conn).unwrap();
    assert_eq!(summary.before.roots.len(), 2);
    assert_eq!(summary.after.roots.len(), 2);
    let child = id(&conn, "C:\\r\\child\\");
    let details = diff::get_details(&conn, &child).unwrap();
    assert_eq!(details.size_delta, "0");
    assert_eq!(details.allocated_delta, "0");
    for (metric, b, a, net) in [
        (Metric::Size, "15", "107", "92"),
        (Metric::Allocated, "28", "211", "183"),
    ] {
        let before_frame = global_treemap::get_frame(&conn, metric, ChartMode::Before, 0).unwrap();
        let after_frame = global_treemap::get_frame(&conn, metric, ChartMode::After, 0).unwrap();
        assert_eq!(before_frame.exported_total, b);
        assert_eq!(after_frame.exported_total, a);
        for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
            let frame = global_treemap::get_frame(&conn, metric, mode, 0).unwrap();
            assert_eq!(frame.weight_total, "0");
            assert_eq!(frame.file_count, 0);
        }
        assert_eq!(
            global_treemap::get_frame(&conn, metric, ChartMode::Delta, 0)
                .unwrap()
                .exported_total,
            net
        );
    }
    let (_dir, conn) = comparison(after, before);
    assert_eq!(
        diff::get_details(&conn, &id(&conn, "C:\\r\\child\\"))
            .unwrap()
            .size_delta,
        "0"
    );
    let frame = global_treemap::get_frame(&conn, Metric::Size, ChartMode::Delta, 0).unwrap();
    assert_eq!(frame.net_delta, "0");
    assert_eq!(frame.exported_total, "-92");
}
#[test]
fn paged_direct_children_and_complete_global_leaves_respect_exact_edges() {
    let mut before = "文件名称,大小,分配\nC:\\r\\,450,900\n".to_owned();
    let mut after = before.clone();
    for i in 0..450 {
        before.push_str(&format!(
            "C:\\r\\f{i:03},1,2\nC:\\r\\gap\\nested{i:03}\\,1,2\nD:\\root{i:03}\\,1,2\n"
        ));
        after.push_str(&format!(
            "C:\\r\\f{i:03},{},{}\nC:\\r\\gap\\nested{i:03}\\,1,2\nD:\\root{i:03}\\,{},{}\n",
            i % 2 * 2,
            i % 2 * 4,
            i % 2 * 2,
            i % 2 * 4
        ));
    }
    let (_dir, conn) = comparison(&before, &after);
    let root = id(&conn, "C:\\r\\");
    let mut cursor = None;
    let mut seen = std::collections::BTreeSet::new();
    loop {
        let page =
            diff::list_children(&conn, "test", Some(&root), false, cursor.as_deref()).unwrap();
        assert_eq!(page.total_children, 450);
        for row in page.rows {
            assert!(row.name.starts_with('f'));
            assert!(seen.insert(row.node_id));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen.len(), 450);
    let summary = store::get_summary(&conn).unwrap();
    assert_eq!(summary.before.root_count, 901);
    assert_eq!(summary.before.roots.len(), 200);
    assert!(summary.before.roots_truncated);
    let mut cursor = None;
    let mut all_roots = std::collections::BTreeSet::new();
    loop {
        let page =
            diff::list_roots(&conn, "test", SnapshotSide::Before, cursor.as_deref()).unwrap();
        assert_eq!(page.total_roots, 901);
        assert!(page.rows.len() <= 200);
        for row in page.rows {
            assert!(all_roots.insert(row.node_id));
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(all_roots.len(), 901);
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
            let chart = global_treemap::get_frame(&conn, metric, mode, 0).unwrap();
            assert_eq!(chart.file_count, 450);
            if matches!(mode, ChartMode::Delta) {
                assert_eq!(chart.net_delta, "0");
                assert_eq!(chart.visible_file_count, 450);
            } else {
                assert_ne!(chart.exported_total, chart.weight_total);
            }
        }
    }
}
#[test]
fn absolute_delta_total_preserves_values_beyond_signed_sqlite_integer() {
    let dir = TempDir::new().unwrap();
    let b=source(&dir,"before.csv","文件名称,大小,分配\nC:\\x\\,9223372036854775807,0\nC:\\x\\a,9223372036854775807,0\nC:\\x\\b,0,0\n");
    let a=source(&dir,"after.csv","文件名称,大小,分配\nC:\\x\\,9223372036854775807,0\nC:\\x\\a,0,0\nC:\\x\\b,9223372036854775807,0\n");
    let db = dir.path().join("overflow.sqlite");
    store::build_comparison(&b, &a, &db, "overflow", &JobControl::default(), &mut |_| {}).unwrap();
    let conn = store::open_reader(&db).unwrap();
    // Each signed side fits i64; their gross change is exactly twice i64::MAX.
    for depth in [1, 3, 0] {
        let frame =
            global_treemap::get_frame(&conn, Metric::Size, ChartMode::Delta, depth).unwrap();
        assert_eq!(frame.weight_total, (2u64 * i64::MAX as u64).to_string());
        assert_eq!(frame.positive_total, i64::MAX.to_string());
        assert_eq!(frame.negative_total, i64::MAX.to_string());
        assert_eq!(frame.net_delta, "0");
        assert_eq!(frame.visible_file_count, 2);
        assert_eq!(frame.rendered_block_count, if depth == 1 { 1 } else { 2 });
    }
}
#[test]
fn directory_first_cursor_crosses_the_file_boundary_without_repeats() {
    let mut csv = "文件名称,大小,分配\nC:\\r\\,0,0\n".to_owned();
    for i in 0..205 {
        csv.push_str(&format!("C:\\r\\z{i:03}\\,0,0\n"));
    }
    for i in 0..205 {
        csv.push_str(&format!("C:\\r\\a{i:03},0,0\n"));
    }
    let (_dir, conn) = comparison(&csv, &csv);
    let root = id(&conn, "C:\\r\\");
    let mut cursor = None;
    let mut all = Vec::new();
    loop {
        let page =
            diff::list_children(&conn, "test", Some(&root), false, cursor.as_deref()).unwrap();
        all.extend(page.rows);
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(all.len(), 410);
    assert!(all[..205].iter().all(|r| r.expandable));
    assert!(all[205..].iter().all(|r| !r.expandable));
    assert_eq!(
        all.into_iter()
            .map(|r| r.node_id)
            .collect::<std::collections::HashSet<_>>()
            .len(),
        410
    );
}
#[test]
fn leaf_details_preserve_displayed_side_spelling() {
    let (_dir, conn) = comparison(
        "文件名称,大小,分配\nC:\\Mixed.PY,10,20\n",
        "文件名称,大小,分配\nc:/MIXED.py,11,21\n",
    );
    let details = diff::get_details(&conn, &id(&conn, "C:\\Mixed.PY")).unwrap();
    assert_eq!(details.before.as_ref().unwrap().path, "C:\\Mixed.PY");
    assert_eq!(details.after.as_ref().unwrap().path, "c:/MIXED.py");
    assert_eq!(details.size_delta, "1");
}
