use wiztree_diff_lib::{
    file_categories::{self, CATEGORIES},
    file_extensions,
    import::JobControl,
    store::{self, Comparison},
    types::{Metric, SnapshotSide},
};

fn build(before: &str, after: &str) -> (tempfile::TempDir, Comparison) {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("before.csv");
    let a = dir.path().join("after.csv");
    std::fs::write(&b, before).unwrap();
    std::fs::write(&a, after).unwrap();
    let comparison =
        store::build_comparison(&b, &a, "categories", &JobControl::default(), &mut |_| {}).unwrap();
    (dir, comparison)
}

#[test]
fn original_classification_boundaries_and_basename_exceptions() {
    for (name, expected) in [
        ("README", "text"),
        ("rEaDmE", "text"),
        (".ENV", "text"),
        ("README.exe", "code"),
        ("x.env", "other"),
        ("README.old", "other"),
        ("readme2", "other"),
        (".env.local", "other"),
        (".gitignore", "other"),
        ("no-extension", "other"),
        ("trailing.", "other"),
        ("x.DLL", "code"),
        ("x.mui", "code"),
        ("x.PS1", "code"),
        ("x.HEIC", "images"),
        ("x.psd", "images"),
        ("x.MKV", "video"),
        ("x.EPUB", "documents"),
        ("x.rtf", "documents"),
        ("archive.tar.GZ", "archives"),
        ("x.iso", "archives"),
        ("x.RST", "text"),
        ("x.CSV", "text"),
        ("x.AIFF", "audio"),
        ("x.opus", "audio"),
        ("x.bin", "other"),
    ] {
        assert_eq!(file_categories::category(name), expected, "{name}");
    }
}

#[test]
fn global_actual_totals_preserve_hardlinks_orphans_and_readme_env() {
    let h = "文件名称,大小,分配,MFTRECNO\n";
    let b = format!("{h}C:\\root\\,9999,9999\nC:\\root\\a.PY,10,16,42\nC:\\root\\b.py,10,16,42\nC:\\root\\zero.PNG,0,0\nC:\\root\\gap\\lost.TXT,7,8\nC:\\root\\README,11,12\nC:\\root\\.ENV,13,14\nC:\\root\\plain,17,18\nC:\\root\\x.env,19,20\nC:\\root\\.env.local,23,24\nC:\\root\\movie.mp4,29,30\nC:\\root\\paper.pdf,31,32\nC:\\root\\archive.tar.gz,37,38\nC:\\root\\sound.mp3,41,42\n");
    let a = format!("{h}C:\\root\\,8888,8888\nC:\\root\\README,1,2\nC:\\root\\.env,3,4\nC:\\root\\gap\\added.JPG,5,6\n");
    let (_dir, c) = build(&b, &a);
    let stats = file_categories::get_file_categories(&c).unwrap();
    assert_eq!(
        stats
            .items
            .iter()
            .map(|i| i.category.as_str())
            .collect::<Vec<_>>(),
        CATEGORIES
    );
    assert_eq!(
        (
            &*stats.before.size,
            &*stats.before.allocated,
            stats.before.files
        ),
        ("248", "270", 13)
    );
    assert_eq!(
        (
            &*stats.after.size,
            &*stats.after.allocated,
            stats.after.files
        ),
        ("9", "12", 3)
    );
    let expected = [
        ("code", "20", "32", 2),
        ("images", "0", "0", 1),
        ("video", "29", "30", 1),
        ("documents", "31", "32", 1),
        ("archives", "37", "38", 1),
        ("text", "31", "34", 3),
        ("audio", "41", "42", 1),
        ("other", "59", "62", 3),
    ];
    for (category, size, allocated, files) in expected {
        let item = stats.items.iter().find(|i| i.category == category).unwrap();
        assert_eq!(
            (
                &*item.before.size,
                &*item.before.allocated,
                item.before.files
            ),
            (size, allocated, files)
        );
    }
    assert_eq!(stats.items[5].after.size, "4");
    for side in [SnapshotSide::Before, SnapshotSide::After] {
        let ext =
            file_extensions::list_extensions(&c, "categories", None, side, Metric::Size, None)
                .unwrap();
        let total = if side == SnapshotSide::Before {
            &stats.before
        } else {
            &stats.after
        };
        assert_eq!(ext.total.size, total.size);
        assert_eq!(ext.total.allocated, total.allocated);
        assert_eq!(ext.total.files, total.files);
        if side == SnapshotSide::Before {
            assert!(ext
                .rows
                .iter()
                .any(|i| i.extension == "" && i.size == "64" && i.files == 4));
            assert!(ext
                .rows
                .iter()
                .any(|i| i.extension == ".env" && i.size == "19"));
        }
    }
}

#[test]
fn empty_comparison_still_returns_all_eight_zero_categories() {
    let csv = "文件名称,大小,分配\nC:\\root\\,999,1000\n";
    let (_dir, c) = build(csv, csv);
    let stats = file_categories::get_file_categories(&c).unwrap();
    assert_eq!(stats.items.len(), 8);
    assert_eq!(stats.before.files, 0);
    assert_eq!(stats.after.size, "0");
    assert!(stats
        .items
        .iter()
        .all(|i| i.before.size == "0" && i.after.allocated == "0"));
}

#[test]
fn category_aggregation_checks_overflow_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("overflow.csv");
    std::fs::write(
        &path,
        "文件名称,大小,分配\nC:\\first.txt,9223372036854775807,0\nC:\\second.txt,1,0\n",
    )
    .unwrap();
    assert_eq!(
        store::build_comparison(
            &path,
            &path,
            "overflow",
            &JobControl::default(),
            &mut |_| {}
        )
        .err()
        .unwrap()
        .code,
        "AGGREGATE_OVERFLOW"
    );
    let control = JobControl::default();
    control.cancel();
    assert_eq!(
        store::build_comparison(&path, &path, "cancelled", &control, &mut |_| {})
            .err()
            .unwrap()
            .code,
        "CANCELLED"
    );
}
