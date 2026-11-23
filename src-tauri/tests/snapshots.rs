use std::{fs, io::Write};
use tempfile::TempDir;
use wiztree_diff_lib::{
    import::{self, JobControl},
    store::{self, Comparison},
    types::{ChildSort, ChildSortBy, NodeKind, SortDirection},
};

fn source(dir: &TempDir, name: &str, text: &str) -> std::path::PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, text).unwrap();
    path
}
fn import_one(
    text: &str,
) -> Result<
    (TempDir, Comparison, wiztree_diff_lib::types::SourceSummary),
    wiztree_diff_lib::types::ApiError,
> {
    let dir = TempDir::new().unwrap();
    let path = source(&dir, "input.csv", text);
    let comparison =
        store::build_comparison(&path, &path, "test", &JobControl::default(), &mut |_| {})?;
    let summary = comparison.summary.before.clone();
    Ok((dir, comparison, summary))
}
#[test]
fn reordered_headers_bom_optional_trailing_columns_and_quoted_names() {
    let (_, conn, summary) = import_one("\u{feff}说明\r\n大小,文件名称,分配,MFTRECNO,CREATEDDATE,DRIVECAPACITY\r\n00012,\"C:/选定/a,\"\"中文\"\"\n文件\",02150400,000587612,原文\r\n12,C:/选定/,2150400\r\n").unwrap();
    assert_eq!(summary.rows, 2);
    assert_eq!(summary.description.as_deref(), Some("说明"));
    let node = (1..=conn.nodes.len() as u32)
        .find(|&id| conn.entry(id, 0).unwrap().kind == NodeKind::File)
        .unwrap();
    let entry = diff::get_details(&conn, &format!("n{node}"))
        .unwrap()
        .before
        .unwrap();
    assert_eq!(entry.allocated, "2150400");
    assert_eq!(entry.mft.as_deref(), Some("587612"));
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

    assert_eq!(
        import::load(&path, 0, &JobControl::default(), &mut |_| {}, |_| Ok(()))
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
    assert_eq!(conn.canonical_path(1), "c:\\ος");
    assert_eq!(conn.name(1), "ΟΣ");
    let entry = conn.entry(1, 0).unwrap();
    assert_eq!(conn.entry_details(entry)[4], Some("184467440737095516160"));
    assert_eq!(
        conn.entry_details(entry).to_vec(),
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
fn out_of_order_actual_parents_preserve_all_paths_and_exact_children() {
    let mut text = "文件名称,大小,分配\n".to_owned();
    for i in 0..4101 {
        text.push_str(&format!(
            "C:/Root\\Dir{i}/File.TxT,1,2\nC:/Root\\Dir{i}/,1,2\n"
        ));
    }
    text.push_str("C:/Root/,4101,8202\n");
    let (_dir, conn) = comparison(&text, &text);
    let paths: std::collections::HashMap<_, _> = (1..=conn.nodes.len() as u32)
        .map(|id| (conn.canonical_path(id), id))
        .collect();
    for original in text
        .lines()
        .skip(1)
        .map(|line| line.split(',').next().unwrap())
    {
        let node = paths[&import::normalize(original).0];
        assert_eq!(conn.path(node, 0), original);
        assert_eq!(conn.path(node, 1), original);
    }
    assert_eq!(conn.nodes.len(), 8203);
    assert_eq!(
        (
            conn.summary.before.root_count,
            conn.summary.after.root_count
        ),
        (1, 1)
    );
    assert_eq!(
        (&*conn.summary.before.size, &*conn.summary.after.allocated),
        ("4101", "8202")
    );
    let root = id(&conn, "C:/Root/");
    let page =
        diff::list_children(&conn, "test", Some(&root), false, ChildSort::default(), None).unwrap();
    assert_eq!(page.total_children, 4101);
    let file = diff::get_details(&conn, &id(&conn, "C:/Root/Dir4100/File.TxT")).unwrap();
    assert_eq!(file.parent_id, Some(id(&conn, "C:/Root/Dir4100/")));
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
    assert_eq!(conn.nodes.len(), originals.len());
    for original in originals {
        let node = id(&conn, original)[1..].parse::<u32>().unwrap();
        assert_eq!(conn.canonical_path(node), import::normalize(original).0);
        assert_eq!(conn.path(node, 0), original);
    }
}

#[test]
fn identical_snapshots_preserve_large_import_path_totals_and_hardlinks() {
    let mut text = "文件名称,大小,分配,MFTRECNO\nC:\\Root\\,10005,20010,1\n".to_owned();
    for i in 0..10_005 {
        text.push_str(&format!("C:\\Root\\a{i}.TXT,1,2,184467440737095516160\n"));
    }
    let (dir, conn) = comparison(&text, &text);
    for summary in [&conn.summary.before, &conn.summary.after] {
        assert_eq!(
            (summary.rows, summary.files, summary.folders),
            (10_006, 10_005, 1)
        );
    }
    assert_eq!(conn.nodes.len(), 10_006);
    let totals = wiztree_diff_lib::file_categories::get_file_categories(&conn).unwrap();
    assert_eq!(
        (
            &*totals.before.size,
            &*totals.after.allocated,
            totals.after.files
        ),
        ("10005", "20010", 10_005)
    );
    let details = diff::get_details(&conn, &id(&conn, "C:\\Root\\a10004.TXT")).unwrap();
    assert_eq!(details.before.as_ref().unwrap().hardlink_count, 10_005);
    assert_eq!(details.after.as_ref().unwrap().hardlink_count, 10_005);
    assert_eq!(
        fs::read_to_string(dir.path().join("before.csv")).unwrap(),
        text
    );
}
#[test]
fn details_preserve_every_field_original_path_spelling_and_kind() {
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
        let conn =
            store::build_comparison(&before, &after, "test", &JobControl::default(), &mut |_| {})
                .unwrap();
        assert_eq!(conn.nodes.len(), 1);
        let details = diff::get_details(&conn, "n1").unwrap();
        let actual = details.after.unwrap();
        assert_eq!(actual.size, changed[1]);
        assert_eq!(actual.allocated, changed[2]);
        assert_eq!(actual.path, changed[0]);
        assert_eq!(
            actual.kind,
            if changed[0].ends_with('/') {
                "directory"
            } else {
                "file"
            }
        );
        for side in 0..2 {
            let expected = if side == 0 { &base } else { &changed };
            let entry = conn.entry(1, side).unwrap();
            for (i, value) in conn.entry_details(entry).iter().enumerate() {
                let raw = expected[i + 3];
                let normalized = if i == 0 || i == 1 || i == 6 || i == 7 {
                    raw
                } else {
                    let stripped = raw.trim_start_matches('0');
                    if !raw.is_empty() && stripped.is_empty() {
                        "0"
                    } else {
                        stripped
                    }
                };
                assert_eq!(
                    *value,
                    (!raw.is_empty()).then_some(normalized),
                    "column {}: side {side}",
                    i + 3
                );
            }
        }
    }
}

#[test]
fn details_keep_side_specific_hardlinks_and_canonical_numeric_fields() {
    let (_dir, conn) = comparison(
        "文件名称,大小,分配,文件,MFTRECNO\nC:\\r\\,2,4,2,1\nC:\\r\\one.txt,1,2,0000,000184467440737095516160\nC:\\r\\two.txt,1,2,0,184467440737095516160\n",
        "文件名称,大小,分配,文件,MFTRECNO\nC:\\r\\,1,2,1,1\nC:\\r\\one.txt,0001,0002,0,184467440737095516160\n",
    );

    let one = diff::get_details(&conn, &id(&conn, "C:\\r\\one.txt")).unwrap();
    assert_eq!(one.before.as_ref().unwrap().hardlink_count, 2);
    assert_eq!(one.after.as_ref().unwrap().hardlink_count, 1);
    assert_eq!(
        one.before.unwrap().mft.as_deref(),
        Some("184467440737095516160")
    );
    let summaries = &conn.summary;
    assert_eq!(
        (&*summaries.before.size, &*summaries.after.size),
        ("2", "1")
    );
}

#[test]
fn large_imports_match_normalized_ids_and_reject_late_conflicts() {
    let mut text = "文件名称,大小,分配\n".to_owned();
    for i in 0..10_005 {
        text.push_str(&format!("C:\\Root\\a{i}.TXT,1,2\n"));
    }
    let (_dir, conn) = comparison(&text, &text.replace("C:\\Root\\", "c:/root/"));
    assert_eq!(conn.summary.before.rows, 10_005);
    assert_eq!(conn.nodes.len(), 10_005);
    let totals = wiztree_diff_lib::file_categories::get_file_categories(&conn).unwrap();
    assert_eq!(
        (
            &*totals.before.size,
            &*totals.after.allocated,
            totals.after.files
        ),
        ("10005", "20010", 10_005)
    );
    let details = diff::get_details(&conn, &id(&conn, "C:\\Root\\a10004.TXT")).unwrap();
    assert_eq!(details.before.unwrap().path, "C:\\Root\\a10004.TXT");
    assert_eq!(details.after.unwrap().path, "c:/root/a10004.TXT");
    text.push_str("c:/ROOT/A0.txt,1,2\n");
    let error = import_one(&text).err().unwrap();
    assert_eq!(error.code, "INVALID_FIELD");
    assert_eq!(error.record, Some(10_007));
    assert_eq!(conn.summary.comparison_id, "test");
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
    let (_dir, conn) = comparison(
        "文件名称,大小,分配,文件,文件夹\nC:\\selected\\child\\,10,20,0,0\nC:\\selected\\,10,20,99,99\nC:\\orphan,5,6\n",
        "文件名称,大小,分配\nC:\\selected\\,10,20\nC:\\selected\\child\\,10,20\nC:\\orphan,5,6\n",
    );
    let before = &conn.summary.before;
    let warnings = &conn.summary.warnings;
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
        assert_eq!(import_one(text).err().unwrap().code, code);
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

    let control = JobControl::default();
    let error =
        import::load(&path, 0, &control, &mut |_| control.cancel(), |_| Ok(())).unwrap_err();
    assert_eq!(error.code, "CANCELLED");
    let error = import::load(
        &path,
        0,
        &JobControl::default(),
        &mut |_| {
            fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap()
                .write_all(b"\n")
                .unwrap();
        },
        |_| Ok(()),
    )
    .unwrap_err();
    assert_eq!(error.code, "SOURCE_CHANGED");
}

use wiztree_diff_lib::{
    diff, global_treemap,
    types::{ChartMode, Metric, SnapshotSide, Status},
};
fn request_frame(
    comparison: &Comparison,
    metric: Metric,
    mode: ChartMode,
    depth: u32,
) -> wiztree_diff_lib::types::Result<wiztree_diff_lib::types::FullTreemapData> {
    let layout = global_treemap::TreemapLayout::build(
        comparison,
        metric,
        mode,
        depth,
        &JobControl::default(),
    )?;
    layout.frame(comparison, "snapshot-layout")
}
#[test]
fn root_pages_cover_file_only_snapshots_and_preserve_each_side_spelling() {
    let mut before = "文件名称,大小,分配\n".to_owned();
    let mut after = before.clone();
    for i in (0..450).rev() {
        before.push_str(&format!("C:\\Orphans\\Ä{i:03},9007199254740993,2\n"));
        after.push_str(&format!("c:/orphans/ä{i:03},9007199254740994,3\n"));
    }
    let (_dir, conn) = comparison(&before, &after);
    let summary = &conn.summary;
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
    let summary = &conn.summary;
    assert_eq!(summary.before.root_count, 200);
    assert!(!summary.before.roots_truncated);
    let page = diff::list_roots(&conn, "test", SnapshotSide::Before, None).unwrap();
    assert_eq!(page.rows.len(), 200);
    assert!(page.next_cursor.is_none());
    let after = format!("{before}D:\\tail-after\\,3,4\n");
    before.push_str("D:\\tail-before\\,5,6\n");
    let (_dir, conn) = comparison(&before, &after);
    let summary = &conn.summary;
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
    let summary = &conn.summary;
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
    let chart = request_frame(&conn, Metric::Size, ChartMode::Before, 0).unwrap();
    assert_eq!(chart.weight_total, "10");
    assert_eq!(chart.exported_total, "13");
    assert_eq!(chart.file_count, 2);
}

fn comparison(before: &str, after: &str) -> (TempDir, Comparison) {
    let dir = TempDir::new().unwrap();
    let b = source(&dir, "before.csv", before);
    let a = source(&dir, "after.csv", after);
    let comparison =
        store::build_comparison(&b, &a, "test", &JobControl::default(), &mut |_| {}).unwrap();
    (dir, comparison)
}
fn id(comparison: &Comparison, path: &str) -> String {
    let key = import::normalize(path).0;
    let node = (1..=comparison.nodes.len() as u32)
        .find(|&id| comparison.canonical_path(id) == key)
        .unwrap();
    format!("n{node}")
}
#[test]
fn details_preserve_precise_side_bytes_and_metadata() {
    let (_dir, conn) = comparison(
        "文件名称,大小,分配,MFTRECNO\nC:\\Big.TXT,9007199254740993,9223372036854775806,184467440737095516160\n",
        "文件名称,大小,分配,MFTRECNO\nC:\\Big.TXT,9007199254740994,9223372036854775807,184467440737095516160\n",
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

    for changes in [false, true] {
        let mut expected: Vec<_> = (0..451)
            .filter(|i| !changes || i % 2 == 0)
            .map(|i| {
                let name = format!("{}-{i:04}.TxT", stems[i % stems.len()]);
                (id(&conn, &format!("C:\\r\\left\\{name}")), name)
            })
            .collect();
        expected.sort_unstable_by(|a, b| {
            a.1.to_lowercase()
                .cmp(&b.1.to_lowercase())
                .then_with(|| a.0.cmp(&b.0))
        });
        let mut actual = Vec::new();
        let mut cursor = None;
        loop {
            let page = diff::list_children(
                &conn,
                "test",
                Some(&left),
                changes,
                ChildSort::default(),
                cursor.as_deref(),
            )
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
                        ChildSort::default(),
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
}

#[test]
fn exact_statuses_internal_changes_and_signed_charts() {
    let header = "文件名称,大小,分配,修改时间,FOLDERSIZE\n";
    let b=format!("{header}C:\\test\\,1250,1472,old,950\nC:\\test\\cancel\\,300,384,old,300\nC:\\test\\cancel\\a,100,128\nC:\\test\\cancel\\b,200,256\nC:\\test\\keep,100,128,old\nC:\\test\\grow,200,256\nC:\\test\\shrink,500,512\nC:\\test\\remove,50,64\nC:\\test\\allocOnly,100,128\nC:\\test\\zero,0,0\n");
    let a=format!("{header}C:\\test\\cancel\\b,150,192\nC:\\test\\cancel\\a,150,192\nC:\\test\\,1275,1792,new,975\nC:\\test\\cancel\\,300,384,new,300\nC:\\test\\keep,100,128,new\nC:\\test\\grow,300,384\nC:\\test\\shrink,400,512\nC:\\test\\add,75,128\nC:\\test\\allocOnly,100,256\nC:\\test\\zero,0,0\n");
    let (_dir, conn) = comparison(&b, &a);
    let summary = &conn.summary;
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
    let page =
        diff::list_children(&conn, "test", Some(&root), false, ChildSort::default(), None).unwrap();
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
    let filtered =
        diff::list_children(&conn, "test", Some(&root), true, ChildSort::default(), None).unwrap();
    assert_eq!(filtered.rows.len(), 6);
    assert!(filtered.rows.iter().any(|r| r.node_id == cancel));
    assert_eq!(
        diff::list_children(&conn, "test", None, true, ChildSort::default(), None)
            .unwrap()
            .rows
            .len(),
        1
    );
    for (metric, positive, negative, net) in [
        (Metric::Size, "225", "200", "25"),
        (Metric::Allocated, "448", "128", "320"),
    ] {
        let chart = request_frame(&conn, metric, ChartMode::Delta, 0).unwrap();
        assert_eq!(chart.positive_total, positive);
        assert_eq!(chart.negative_total, negative);
        assert_eq!(chart.net_delta, net);
    }
    let cancel_children = diff::list_children(
        &conn,
        "test",
        Some(&cancel),
        true,
        ChildSort::default(),
        None,
    )
    .unwrap();
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
    let page =
        diff::list_children(&conn, "test", Some(&root), false, ChildSort::default(), None).unwrap();
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
    assert!(
        diff::list_children(&conn, "stale", Some(&root), false, ChildSort::default(), None)
            .is_err()
    );
    assert!(diff::get_details(&conn, "n999999").is_err());
    assert!(diff::list_children(
        &conn,
        "test",
        Some(&id(&conn, "c:\\t\\link")),
        false,
        ChildSort::default(),
        None
    )
    .is_err());
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
        let page = diff::list_children(
            &conn,
            "test",
            Some(&root),
            false,
            ChildSort::default(),
            cursor.as_deref(),
        )
        .unwrap();
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
    assert!(diff::list_children(
        &conn,
        "test",
        None,
        false,
        ChildSort::default(),
        first_cursor.as_deref()
    )
    .is_err());
    assert!(diff::list_children(
        &conn,
        "test",
        Some(&root),
        true,
        ChildSort::default(),
        first_cursor.as_deref()
    )
    .is_err());
    assert!(diff::list_children(
        &conn,
        "test",
        Some(&root),
        false,
        ChildSort::default(),
        Some("garbage")
    )
    .is_err());
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
            let layout = global_treemap::TreemapLayout::build(
                &conn,
                metric,
                mode,
                0,
                &JobControl::default(),
            )
            .unwrap();
            let chart = layout.frame(&conn, "snapshot-layout").unwrap();
            assert_eq!(chart.file_count, 450);
            assert_eq!(chart.visible_file_count, 450);
            for i in 0..450 {
                assert!(layout
                    .get_bounds(&conn, &id(&conn, &format!("C:\\wide\\f{i:04}")))
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
fn failed_and_cancelled_builds_leave_the_old_comparison_untouched() {
    let dir = TempDir::new().unwrap();
    let valid_text = "文件名称,大小,分配\nC:\\x\\,0,0\n";
    let valid = source(&dir, "valid.csv", valid_text);
    let ready = store::build_comparison(&valid, &valid, "old", &JobControl::default(), &mut |_| {})
        .unwrap();
    for text in [
        "文件名称,大小,分配\nC:\\a,0,0\nc:\\A,0,0\n",
        "文件名称,大小,分配\nC:\\a,-1,0\n",
        "文件名称,大小,分配\nC:\\x\\,0,0\nC:\\x\\a,9223372036854775807,0\nC:\\x\\b,1,0\n",
    ] {
        let bad = source(&dir, "bad.csv", text);
        assert!(
            store::build_comparison(&bad, &valid, "new", &JobControl::default(), &mut |_| {})
                .is_err()
        );
        assert_eq!(ready.summary.comparison_id, "old");
        assert_eq!(
            diff::list_roots(&ready, "old", SnapshotSide::Before, None)
                .unwrap()
                .rows[0]
                .path,
            "C:\\x\\"
        );
    }
    for cancelled_in_advance in [false, true] {
        let control = JobControl::default();
        if cancelled_in_advance {
            control.cancel();
        }
        let error =
            store::build_comparison(&valid, &valid, "new", &control, &mut |_| control.cancel())
                .err()
                .unwrap();
        assert_eq!(error.code, "CANCELLED");
        assert_eq!(ready.summary.comparison_id, "old");
    }
    assert_eq!(fs::read_to_string(&valid).unwrap(), valid_text);
}
#[test]
fn missing_intermediate_roots_remain_real_roots_without_fabricated_children() {
    let before = "文件名称,大小,分配,文件,文件夹\nC:\\r\\,10,20,1,2\nC:\\r\\gap\\child\\,10,20,1,0\nC:\\r\\gap\\child\\f,10,20\nD:\\other\\,5,8,0,0\n";
    let after = before.replace("child\\f,10,20", "child\\f,11,21");
    let (_dir, conn) = comparison(before, &after);
    let summary = &conn.summary;
    assert_eq!(summary.before.roots.len(), 3);
    assert_eq!(summary.before.size, "15");
    assert_eq!(summary.before.allocated, "28");
    assert!(summary.warnings.iter().any(|s| s.contains("中间父目录")));
    assert!(summary.warnings.iter().any(|s| s.contains("计数")));
    let root = id(&conn, "C:\\r\\");
    let child = id(&conn, "C:\\r\\gap\\child\\");
    let roots = diff::list_children(&conn, "test", None, false, ChildSort::default(), None).unwrap();
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
        diff::list_children(&conn, "test", Some(&root), false, ChildSort::default(), None)
            .unwrap()
            .total_children,
        0
    );
    assert_eq!(
        diff::list_children(&conn, "test", Some(&child), false, ChildSort::default(), None)
            .unwrap()
            .total_children,
        1
    );
    for metric in [Metric::Size, Metric::Allocated] {
        for mode in [ChartMode::Before, ChartMode::After] {
            let layout = global_treemap::TreemapLayout::build(
                &conn,
                metric,
                mode,
                0,
                &JobControl::default(),
            )
            .unwrap();
            let chart = layout.frame(&conn, "snapshot-layout").unwrap();
            assert_eq!(chart.file_count, 1);
            assert_ne!(chart.exported_total, chart.weight_total);
            assert!(layout.get_bounds(&conn, &child).unwrap().is_some());
            assert!(layout.get_bounds(&conn, &root).unwrap().is_none());
        }
        let delta = request_frame(&conn, metric, ChartMode::Delta, 0).unwrap();
        assert_eq!(delta.weight_total, "1");
        assert_eq!(delta.net_delta, "1");
    }
}
#[test]
fn workspace_scope_eligibility_never_changes_raw_node_deltas() {
    let before = "文件名称,大小,分配\nC:\\r\\child\\,10,20\nD:\\other\\,5,8\n";
    let after = "文件名称,大小,分配\nC:\\r\\,100,200\nC:\\r\\child\\,10,20\nD:\\other\\,7,11\n";
    let (_dir, conn) = comparison(before, after);
    let summary = &conn.summary;
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
        let before_frame = request_frame(&conn, metric, ChartMode::Before, 0).unwrap();
        let after_frame = request_frame(&conn, metric, ChartMode::After, 0).unwrap();
        assert_eq!(before_frame.exported_total, b);
        assert_eq!(after_frame.exported_total, a);
        for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
            let frame = request_frame(&conn, metric, mode, 0).unwrap();
            assert_eq!(frame.weight_total, "0");
            assert_eq!(frame.file_count, 0);
        }
        assert_eq!(
            request_frame(&conn, metric, ChartMode::Delta, 0)
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
    let frame = request_frame(&conn, Metric::Size, ChartMode::Delta, 0).unwrap();
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
        let page = diff::list_children(
            &conn,
            "test",
            Some(&root),
            false,
            ChildSort::default(),
            cursor.as_deref(),
        )
        .unwrap();
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
    let summary = &conn.summary;
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
            let chart = request_frame(&conn, metric, mode, 0).unwrap();
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
fn absolute_delta_total_preserves_values_beyond_signed_integer() {
    let dir = TempDir::new().unwrap();
    let b=source(&dir,"before.csv","文件名称,大小,分配\nC:\\x\\,9223372036854775807,0\nC:\\x\\a,9223372036854775807,0\nC:\\x\\b,0,0\n");
    let a=source(&dir,"after.csv","文件名称,大小,分配\nC:\\x\\,9223372036854775807,0\nC:\\x\\a,0,0\nC:\\x\\b,9223372036854775807,0\n");
    let conn =
        store::build_comparison(&b, &a, "overflow", &JobControl::default(), &mut |_| {}).unwrap();
    // Each signed side fits i64; their gross change is exactly twice i64::MAX.
    for depth in [1, 3, 0] {
        let frame = request_frame(&conn, Metric::Size, ChartMode::Delta, depth).unwrap();
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
        let page = diff::list_children(
            &conn,
            "test",
            Some(&root),
            false,
            ChildSort::default(),
            cursor.as_deref(),
        )
        .unwrap();
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

fn chronology_csv(description: Option<&str>, size: u64) -> String {
    let mut text = String::new();
    if let Some(description) = description {
        text.push_str(description);
        text.push('\n');
    }
    text.push_str(&format!("文件名称,大小,分配,修改时间\nC:\\r\\,{size},{size},2099/01/01 00:00:00\nC:\\r\\a.txt,{size},{size},2099/01/01 00:00:00\n"));
    text
}
#[test]
fn reversed_export_times_warn_without_reordering_or_scope_confirmation() {
    let before = chronology_csv(
        Some("\u{feff}生成由 WizTree 4.33 2026/10/7 12:30:40 (您可以通过捐赠隐藏此信息)"),
        10,
    );
    let after = chronology_csv(Some("Generated by WizTree 4.33 2026/10/6 23:59:59"), 20);
    let (_dir, c) = comparison(&before, &after);
    let warning = c.summary.order_warning.as_ref().unwrap();
    assert_eq!(warning.before_time, "2026-10-07 12:30:40");
    assert_eq!(warning.after_time, "2026-10-06 23:59:59");
    assert!(c.summary.warnings.is_empty());
    let details = diff::get_details(&c, &id(&c, "C:\\r\\a.txt")).unwrap();
    assert_eq!(details.before.as_ref().unwrap().size, "10");
    assert_eq!(details.after.as_ref().unwrap().size, "20");
    assert_eq!(details.size_delta, "10");
}
#[test]
fn export_order_uses_calendar_values_and_strictly_newer_times() {
    for (before, after, warning) in [
        ("2026/9/30 23:59:59", "2026/10/1 00:00:00", false),
        ("2026/10/1 00:00:00", "2026/9/30 23:59:59", true),
        ("2026/12/31 23:59:59", "2027/1/1 00:00:00", false),
        ("2000/2/29 00:00:00", "2000/2/28 23:59:59", true),
        ("2024/2/29 23:59:59", "2024/3/1 00:00:00", false),
        ("2026/10/7 12:00:01", "2026/10/7 12:00:00", true),
        ("2026/10/7 12:00:00", "2026/10/7 12:00:00", false),
    ] {
        let before = chronology_csv(Some(&format!("生成由 WizTree 4.33 {before}")), 10);
        let after = chronology_csv(Some(&format!("生成由 WizTree 4.33 {after}")), 20);
        let (_dir, c) = comparison(&before, &after);
        assert_eq!(c.summary.order_warning.is_some(), warning);
    }
}
#[test]
fn unknown_or_invalid_export_times_are_not_inferred_from_other_dates() {
    let known = chronology_csv(Some("生成由 WizTree 4.33 2026/1/1 00:00:00"), 20);
    for description in [
        None,
        Some("说明 2026/10/7 12:00:00"),
        Some("生成由 WizTree 4.33"),
        Some("生成由 WizTree 4.33 2026/2/29 12:00:00"),
        Some("生成由 WizTree 4.33 1900/2/29 12:00:00"),
        Some("生成由 WizTree 4.33 2026/4/31 12:00:00"),
        Some("生成由 WizTree 4.33 0000/10/7 12:00:00"),
        Some("生成由 WizTree 4.33 2026/13/1 12:00:00"),
        Some("生成由 WizTree 4.33 2026/10/0 12:00:00"),
        Some("生成由 WizTree 4.33 2026/10/7/8 12:00:00"),
        Some("生成由 WizTree 4.33 2026/10/7 24:00:00"),
        Some("生成由 WizTree 4.33 2026/10/7 12:60:00"),
        Some("生成由 WizTree 4.33 2026/10/7 12:00:60"),
        Some("生成由 WizTree 4.33 2026/10/7 12:00:00Z"),
    ] {
        let unknown = chronology_csv(description, 10);
        for (before, after) in [(&unknown, &known), (&known, &unknown)] {
            let (_dir, c) = comparison(before, after);
            assert!(
                c.summary.order_warning.is_none(),
                "description={description:?}"
            );
        }
    }
}
#[test]
fn copied_mtimes_and_timestamp_filenames_do_not_define_export_order() {
    let dir = TempDir::new().unwrap();
    let before = source(
        &dir,
        "WizTree_20991231235959.csv",
        &chronology_csv(Some("生成由 WizTree 4.33 2026/10/6 00:00:00"), 10),
    );
    let after = source(
        &dir,
        "WizTree_20000101000000.csv",
        &chronology_csv(Some("生成由 WizTree 4.33 2026/10/7 00:00:00"), 20),
    );
    let set_modified = |path: &std::path::Path, seconds| {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_times(
                fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds)),
            )
            .unwrap();
    };
    for with_description in [true, false] {
        if !with_description {
            fs::write(&before, chronology_csv(None, 10)).unwrap();
            fs::write(&after, chronology_csv(None, 20)).unwrap();
        }
        set_modified(&before, 200);
        set_modified(&after, 100);
        let c = store::build_comparison(&before, &after, "dates", &JobControl::new(), &mut |_| {})
            .unwrap();
        assert!(c.summary.order_warning.is_none());
    }
}

#[test]
fn list_children_sorts_all_columns_with_exact_precision_nulls_last_and_cursor_binding() {
    let header = "文件名称,大小,分配,修改时间,属性\n";
    let before = format!(
        "{header}\
C:\\s\\,9007199254741294,1000,2026/10/1 00:00:00,D\n\
C:\\s\\dir_b\\,50,100,2026/9/30 23:59:59,D\n\
C:\\s\\dir_a\\,200,50,2026/10/1 00:00:00,DH\n\
C:\\s\\swap\\,10,10,2026/1/1 00:00:00,D\n\
C:\\s\\big.bin,9007199254740993,100,2026/10/10 08:00:00,A\n\
C:\\s\\removed.txt,30,400,2026/2/3 09:05:00,R\n\
C:\\s\\plain,100,200,,\n"
    );
    let after = format!(
        "{header}\
C:\\s\\,9007199254741595,1200,2026/10/2 00:00:00,D\n\
C:\\s\\dir_b\\,50,100,2026/9/30 23:59:59,D\n\
C:\\s\\dir_a\\,200,50,2026/10/2 00:00:00,DH\n\
C:\\s\\swap,20,20,2026/1/2 00:00:00,A\n\
C:\\s\\big.bin,9007199254740995,50,2026/10/9 08:00:00,A\n\
C:\\s\\added.dll,500,900,2026/10/11 12:00:00,S\n\
C:\\s\\plain,100,200,,\n"
    );
    let (_dir, conn) = comparison(&before, &after);
    let root = id(&conn, "C:\\s\\");
    let names = |by, direction, metric| {
        diff::list_children(
            &conn,
            "test",
            Some(&root),
            false,
            ChildSort {
                by,
                direction,
                metric,
            },
            None,
        )
        .unwrap()
        .rows
        .into_iter()
        .map(|r| r.name)
        .collect::<Vec<_>>()
    };

    // Name keeps directories first in both directions.
    assert_eq!(
        names(ChildSortBy::Name, SortDirection::Asc, Metric::Size),
        vec!["dir_a", "dir_b", "swap", "added.dll", "big.bin", "plain", "removed.txt"]
    );
    assert_eq!(
        names(ChildSortBy::Name, SortDirection::Desc, Metric::Size),
        vec!["swap", "dir_b", "dir_a", "removed.txt", "plain", "big.bin", "added.dll"]
    );

    // Before/After size & allocated: missing side sorts last in both directions.
    assert_eq!(
        names(ChildSortBy::Before, SortDirection::Desc, Metric::Size),
        vec!["big.bin", "dir_a", "plain", "dir_b", "removed.txt", "swap", "added.dll"]
    );
    assert_eq!(
        names(ChildSortBy::Before, SortDirection::Asc, Metric::Size),
        vec!["swap", "removed.txt", "dir_b", "plain", "dir_a", "big.bin", "added.dll"]
    );
    assert_eq!(
        names(ChildSortBy::After, SortDirection::Desc, Metric::Allocated),
        vec!["added.dll", "plain", "dir_b", "dir_a", "big.bin", "swap", "removed.txt"]
    );

    // Delta respects signed i128 precision and metric switch (size vs allocated).
    // Size deltas: added.dll(+500), swap(+10), big.bin(+2), dir_a/dir_b/plain(0), removed.txt(-30).
    assert_eq!(
        names(ChildSortBy::Delta, SortDirection::Desc, Metric::Size),
        vec!["added.dll", "swap", "big.bin", "dir_a", "dir_b", "plain", "removed.txt"]
    );
    assert_eq!(
        names(ChildSortBy::Delta, SortDirection::Asc, Metric::Size),
        vec!["removed.txt", "dir_a", "dir_b", "plain", "big.bin", "swap", "added.dll"]
    );
    // Allocated deltas: added.dll(+900), swap(+10), dir_a/dir_b/plain(0), big.bin(-50), removed.txt(-400).
    assert_eq!(
        names(ChildSortBy::Delta, SortDirection::Asc, Metric::Allocated),
        vec!["removed.txt", "big.bin", "dir_a", "dir_b", "plain", "swap", "added.dll"]
    );

    // Kind groups typeChanged, folders, plain files, then file extensions.
    assert_eq!(
        names(ChildSortBy::Kind, SortDirection::Asc, Metric::Size),
        vec!["swap", "dir_a", "dir_b", "plain", "big.bin", "added.dll", "removed.txt"]
    );

    // Modified compares dates chronologically (2026/9/30 < 2026/10/1) with missing last.
    assert_eq!(
        names(ChildSortBy::BeforeModified, SortDirection::Desc, Metric::Size),
        vec!["big.bin", "dir_a", "dir_b", "removed.txt", "swap", "added.dll", "plain"]
    );
    assert_eq!(
        names(ChildSortBy::AfterModified, SortDirection::Asc, Metric::Size),
        vec!["swap", "dir_b", "dir_a", "big.bin", "added.dll", "plain", "removed.txt"]
    );

    // Attributes use after ?? before with missing last.
    assert_eq!(
        names(ChildSortBy::Attributes, SortDirection::Asc, Metric::Size),
        vec!["swap", "big.bin", "dir_b", "dir_a", "removed.txt", "added.dll", "plain"]
    );

    // Pagination under non-default sort spans pages without gaps/duplicates and rejects mismatched sort cursors.
    let mut wide_b = "文件名称,大小,分配\nC:\\w\\,0,0\n".to_owned();
    let mut wide_a = wide_b.clone();
    for i in 0..250u64 {
        wide_b.push_str(&format!("C:\\w\\f{i:03},{},{}\n", i * 10, i * 20));
        wide_a.push_str(&format!("C:\\w\\f{i:03},{},{}\n", (249 - i) * 10, i * 20));
    }
    let (_dir2, wide) = comparison(&wide_b, &wide_a);
    let wroot = id(&wide, "C:\\w\\");
    let sort = ChildSort {
        by: ChildSortBy::Delta,
        direction: SortDirection::Desc,
        metric: Metric::Size,
    };
    let page1 = diff::list_children(&wide, "test", Some(&wroot), false, sort, None).unwrap();
    assert_eq!(page1.rows.len(), 200);
    assert_eq!(page1.rows[0].name, "f000");
    let cursor1 = page1.next_cursor.as_deref().unwrap();
    let page2 = diff::list_children(&wide, "test", Some(&wroot), false, sort, Some(cursor1)).unwrap();
    assert_eq!(page2.rows.len(), 50);
    assert_eq!(page2.rows[49].name, "f249");
    assert!(page2.next_cursor.is_none());

    // Changing direction, column, or metric invalidates the cursor.
    for other in [
        ChildSort { direction: SortDirection::Asc, ..sort },
        ChildSort { by: ChildSortBy::After, ..sort },
        ChildSort { metric: Metric::Allocated, ..sort },
    ] {
        assert_eq!(
            diff::list_children(&wide, "test", Some(&wroot), false, other, Some(cursor1))
                .unwrap_err()
                .code,
            "INVALID_CURSOR"
        );
    }
}

#[test]
fn varint_long_detail_fields_mixed_mft_widths_and_unicode_paths_stay_exact() {
    let long_modified = "2026/10/1 12:00:00 ".to_owned() + &"x".repeat(300);
    let long_attr = "A".repeat(150);
    let header = "文件名称,大小,分配,修改时间,属性,MFTRECNO\n";
    let csv = format!(
        "{header}\
C:\\中文目录\\子目录\\,100,200,{long_modified},{long_attr},1\n\
C:\\中文目录\\子目录\\Ä文件.TXT,10,16,{long_modified},{long_attr},00042\n\
C:\\中文目录\\子目录\\link.txt,10,16,,,42\n\
C:\\中文目录\\子目录\\max64a.bin,10,16,,,18446744073709551615\n\
C:\\中文目录\\子目录\\max64b.bin,10,16,,,018446744073709551615\n\
C:\\中文目录\\子目录\\big1.bin,10,16,,,18446744073709551616\n\
C:\\中文目录\\子目录\\big2.bin,10,16,,,00018446744073709551616\n\
D:\\中文目录\\other.txt,10,16,,,42\n\
\\\\Server\\Share\\ΟΣ,10,16,,,18446744073709551616\n"
    );
    let (_dir, conn) = comparison(&csv, &csv);

    // Multi-byte varint cold detail fields round-trip identically.
    let ä = diff::get_details(&conn, &id(&conn, "C:\\中文目录\\子目录\\Ä文件.TXT")).unwrap();
    let before_ä = ä.before.as_ref().unwrap();
    assert_eq!(before_ä.modified.as_deref(), Some(long_modified.as_str()));
    assert_eq!(before_ä.attributes.as_deref(), Some(long_attr.as_str()));
    assert_eq!(before_ä.mft.as_deref(), Some("42"));
    assert_eq!(before_ä.hardlink_count, 2);

    // u64::MAX and u64::MAX + 1 hardlink groups stay separate and volume-scoped.
    assert_eq!(
        diff::get_details(&conn, &id(&conn, "C:\\中文目录\\子目录\\max64a.bin"))
            .unwrap()
            .before
            .unwrap()
            .hardlink_count,
        2
    );
    assert_eq!(
        diff::get_details(&conn, &id(&conn, "C:\\中文目录\\子目录\\big1.bin"))
            .unwrap()
            .before
            .unwrap()
            .hardlink_count,
        2
    );
    assert_eq!(
        diff::get_details(&conn, &id(&conn, "D:\\中文目录\\other.txt"))
            .unwrap()
            .before
            .unwrap()
            .hardlink_count,
        1
    );
    let unc = diff::get_details(&conn, &id(&conn, "\\\\Server\\Share\\ΟΣ")).unwrap();
    assert_eq!(unc.before.unwrap().hardlink_count, 1);
    assert_eq!(
        import::normalize("\\\\Server\\Share\\ΟΣ").0,
        "\\\\server\\share\\ος"
    );
    assert_eq!(
        import::normalize("C:\\中文目录\\ÄÖÜİ").0,
        "c:\\中文目录\\äöüi\u{307}"
    );
}

#[test]
fn parallel_chunked_import_preserves_multiline_quotes_error_precedence_and_cancellation() {
    let dir = TempDir::new().unwrap();
    let header = "\u{feff}生成由 WizTree 4.33 2026/10/1 12:00:00\r\n文件名称,大小,分配,修改时间,属性,MFTRECNO\r\n";

    // Build a >600 KiB CSV spanning multiple 256 KiB chunks, with quoted multiline fields across boundaries.
    let mut valid = String::from(header);
    valid.push_str("C:\\中文根\\,0,0,2026/10/1 00:00:00,D,1\r\n");
    for i in 0..8000u64 {
        if i % 250 == 0 {
            valid.push_str(&format!(
                "\"C:\\中文根\\多行\r\n名称,\"\"引\"\"_{i:05}.txt\",{},{},2026/10/1 08:00:00,A,{}\r\n",
                i + 1,
                (i + 1) * 2,
                100 + ((i / 250) % 8)
            ));
        } else {
            valid.push_str(&format!(
                "C:\\中文根\\子目录_{:02}\\文件_{i:05}.bin,{},{},2026/10/1 08:00:00,A,{}\r\n",
                i % 32,
                i + 1,
                (i + 1) * 2,
                1000 + i
            ));
        }
    }
    assert!(valid.len() > 512 * 1024);
    let valid_path = source(&dir, "valid.csv", &valid);
    let conn = store::build_comparison(
        &valid_path,
        &valid_path,
        "parallel",
        &JobControl::new(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(conn.summary.before.rows, 8001);
    assert_eq!(conn.summary.before.files, 8000);
    assert_eq!(conn.summary.before.folders, 1);
    let quoted_details = diff::get_details(
        &conn,
        &id(&conn, "C:\\中文根\\多行\r\n名称,\"引\"_00000.txt"),
    )
    .unwrap();
    assert_eq!(quoted_details.before.as_ref().unwrap().size, "1");
    assert_eq!(quoted_details.before.as_ref().unwrap().hardlink_count, 4);

    // Deterministic error precedence: errors in both chunk 0 and chunk 2 must always report chunk 0's record.
    let mut two_errors = String::from(header);
    for i in 0..8000u64 {
        let size = if i == 100 || i == 7000 {
            "-1".to_owned()
        } else {
            "1".to_owned()
        };
        two_errors.push_str(&format!(
            "C:\\中文根\\dir_{:02}\\file_{i:05}.bin,{size},2,2026/10/1 08:00:00,A,{}\r\n",
            i % 32,
            1000 + i
        ));
    }
    let err_path = source(&dir, "two_errors.csv", &two_errors);
    for _ in 0..4 {
        let err = store::build_comparison(
            &err_path,
            &valid_path,
            "err",
            &JobControl::new(),
            &mut |_| {},
        )
        .err()
        .unwrap();
        assert_eq!(err.code, "INVALID_FIELD");
        // description=1, header=2, i=0 is record 3, so i=100 is record 103.
        assert_eq!(err.record, Some(103));
        assert_eq!(err.column.as_deref(), Some("大小"));
    }

    // Late bad UTF-8 in chunk 2 reports exact CSV_ERROR record number.
    let mut bad_utf8 = valid.as_bytes().to_vec();
    bad_utf8.extend_from_slice(b"C:\\bad\xff.bin,1,2,2026/10/1 08:00:00,A,99999\r\n");
    let bad_utf8_path = dir.path().join("bad_utf8.csv");
    fs::write(&bad_utf8_path, &bad_utf8).unwrap();
    let err = store::build_comparison(
        &bad_utf8_path,
        &valid_path,
        "utf8",
        &JobControl::new(),
        &mut |_| {},
    )
    .err()
    .unwrap();
    assert_eq!(err.code, "CSV_ERROR");
    assert_eq!(err.record, Some(8004));

    // Late malformed quote in chunk 2 reports exact CSV_ERROR record number.
    let mut bad_quote = valid.clone();
    bad_quote.push_str("\"C:\\unclosed,1,2\r\n");
    let bad_quote_path = source(&dir, "bad_quote.csv", &bad_quote);
    let err = store::build_comparison(
        &bad_quote_path,
        &valid_path,
        "quote",
        &JobControl::new(),
        &mut |_| {},
    )
    .err()
    .unwrap();
    assert_eq!(err.code, "CSV_ERROR");
    assert_eq!(err.record, Some(8004));

    // Mid-import cancellation during multi-chunk parallel parsing exits cleanly with CANCELLED.
    let control = JobControl::new();
    let err = store::build_comparison(&valid_path, &valid_path, "cancel", &control, &mut |p| {
        if p.rows > 0 {
            control.cancel();
        }
    })
    .err()
    .unwrap();
    assert_eq!(err.code, "CANCELLED");

    // Source mutation during multi-chunk parallel parsing returns SOURCE_CHANGED.
    let mutable_path = source(&dir, "mutable.csv", &valid);
    let mut mutated = false;
    let err = store::build_comparison(
        &mutable_path,
        &valid_path,
        "mutated",
        &JobControl::new(),
        &mut |p| {
            if !mutated && p.rows > 0 {
                mutated = true;
                fs::OpenOptions::new()
                    .append(true)
                    .open(&mutable_path)
                    .unwrap()
                    .write_all(b"\r\n")
                    .unwrap();
            }
        },
    )
    .err()
    .unwrap();
    assert_eq!(err.code, "SOURCE_CHANGED");
}

#[test]
fn locate_child_page_jumps_directly_to_target_page_and_preserves_cursor_continuity() {
    let mut before = "文件名称,大小,分配\nC:\\wide\\,0,0\nC:\\other\\,0,0\nC:\\other\\alien.bin,10,10\n".to_owned();
    let mut after = before.clone();
    for i in 0..450u64 {
        let size_b = i * 10;
        let size_a = if i == 420 { i * 10 + 5 } else { i * 10 };
        before.push_str(&format!("C:\\wide\\item_{i:03},{size_b},{size_b}\n"));
        after.push_str(&format!("C:\\wide\\item_{i:03},{size_a},{size_a}\n"));
    }
    let (_dir, conn) = comparison(&before, &after);
    let wide = id(&conn, "C:\\wide\\");
    let target = id(&conn, "C:\\wide\\item_420");
    let unchanged = id(&conn, "C:\\wide\\item_005");
    let alien = id(&conn, "C:\\other\\alien.bin");

    // Default sort (Name Asc): item_420 is at index 420 -> page starting at 400.
    let default_sort = ChildSort::default();
    let located = diff::locate_child_page(&conn, "test", Some(&wide), &target, false, default_sort)
        .unwrap()
        .unwrap();
    assert_eq!(located.start, 400);
    assert_eq!(located.total_children, 450);
    assert_eq!(located.rows.len(), 50);
    assert!(located.rows.iter().any(|r| r.node_id == target));
    assert!(located.next_cursor.is_none());

    // Non-default sort (After Desc): item_210 is at index 239 (449 - 210) -> start 200, with valid next_cursor.
    let mid_target = id(&conn, "C:\\wide\\item_210");
    let desc_sort = ChildSort {
        by: ChildSortBy::After,
        direction: SortDirection::Desc,
        metric: Metric::Size,
    };
    let page2 = diff::locate_child_page(&conn, "test", Some(&wide), &mid_target, false, desc_sort)
        .unwrap()
        .unwrap();
    assert_eq!(page2.start, 200);
    assert_eq!(page2.rows.len(), 200);
    assert!(page2.rows.iter().any(|r| r.node_id == mid_target));
    let next_cursor = page2.next_cursor.as_deref().unwrap();
    let page3 = diff::list_children(&conn, "test", Some(&wide), false, desc_sort, Some(next_cursor))
        .unwrap();
    assert_eq!(page3.rows.len(), 50);
    assert_eq!(page3.rows[0].name, "item_049");

    // Filtered out under changes_only, or belonging to a different parent, returns None.
    assert!(
        diff::locate_child_page(&conn, "test", Some(&wide), &unchanged, true, default_sort)
            .unwrap()
            .is_none()
    );
    assert!(
        diff::locate_child_page(&conn, "test", Some(&wide), &alien, false, default_sort)
            .unwrap()
            .is_none()
    );
    let filtered = diff::locate_child_page(&conn, "test", Some(&wide), &target, true, default_sort)
        .unwrap()
        .unwrap();
    assert_eq!(filtered.start, 0);
    assert_eq!(filtered.total_children, 1);
    assert_eq!(filtered.rows[0].node_id, target);
}
