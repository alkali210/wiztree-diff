use crate::{
    store::{self, Comparison},
    types::*,
};

pub const CATEGORIES: [&str; 8] = [
    "code",
    "images",
    "video",
    "documents",
    "archives",
    "text",
    "audio",
    "other",
];

/// Original broad classification uses the actual basename, not extension stats.
pub fn category(name: &str) -> &'static str {
    if name.eq_ignore_ascii_case("README") || name.eq_ignore_ascii_case(".env") {
        return "text";
    }
    let Some((_, extension)) = name.rsplit_once('.') else {
        return "other";
    };
    const GROUPS: [(&str, &[&str]); 7] = [
        (
            "code",
            &[
                "exe", "dll", "sys", "ocx", "pdb", "mui", "rs", "py", "js", "jsx", "ts", "tsx",
                "c", "h", "cpp", "hpp", "cs", "java", "go", "rb", "php", "swift", "kt", "scala",
                "vue", "svelte", "html", "css", "scss", "json", "xml", "yaml", "yml", "toml",
                "sql", "sh", "ps1",
            ],
        ),
        (
            "images",
            &[
                "png", "jpg", "jpeg", "gif", "bmp", "webp", "svg", "ico", "tif", "tiff", "heic",
                "avif", "raw", "psd",
            ],
        ),
        (
            "video",
            &[
                "mp4", "mkv", "avi", "mov", "wmv", "webm", "m4v", "mpeg", "mpg", "flv",
            ],
        ),
        (
            "documents",
            &[
                "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "rtf",
                "epub",
            ],
        ),
        (
            "archives",
            &[
                "zip", "7z", "rar", "tar", "gz", "bz2", "xz", "zst", "cab", "iso", "tgz",
            ],
        ),
        (
            "text",
            &["txt", "md", "csv", "log", "ini", "cfg", "conf", "rst"],
        ),
        (
            "audio",
            &[
                "mp3", "wav", "flac", "aac", "ogg", "m4a", "wma", "opus", "aiff",
            ],
        ),
    ];
    for (category, extensions) in GROUPS {
        if extensions.iter().any(|e| extension.eq_ignore_ascii_case(e)) {
            return category;
        }
    }
    "other"
}

fn value(v: [u64; 3]) -> TypeValue {
    TypeValue {
        size: v[0].to_string(),
        allocated: v[1].to_string(),
        files: v[2],
    }
}

pub fn get_file_categories(comparison: &Comparison) -> Result<FileCategoriesData> {
    let items = CATEGORIES
        .iter()
        .enumerate()
        .map(|(index, category)| FileCategoryItem {
            category: (*category).into(),
            before: value(comparison.categories[0][index]),
            after: value(comparison.categories[1][index]),
        })
        .collect();
    let mut totals = [[0u64; 3]; 2];
    for (side, categories) in comparison.categories.iter().enumerate() {
        for values in categories {
            for (total, value) in totals[side].iter_mut().zip(values) {
                *total = store::checked_add(*total, *value, "文件类别汇总")?;
            }
        }
    }
    Ok(FileCategoriesData {
        items,
        before: value(totals[0]),
        after: value(totals[1]),
        warnings: vec![
            "按实际文件路径汇总；不使用目录导出汇总，不按 MFT 去重；包含缺失父目录的文件".into(),
        ],
    })
}
