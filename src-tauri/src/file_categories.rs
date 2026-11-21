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
    CATEGORIES[category_index(name)]
}

#[inline]
pub(crate) fn category_index(name: &str) -> usize {
    if name.eq_ignore_ascii_case("README") || name.eq_ignore_ascii_case(".env") {
        return 5;
    }
    let Some((_, extension)) = name.rsplit_once('.') else {
        return 7;
    };
    category_index_for_ext(extension)
}

#[inline]
pub(crate) fn category_index_for_ext(extension: &str) -> usize {
    let bytes = extension.as_bytes();
    if bytes.is_empty() || bytes.len() > 6 {
        return 7;
    }
    let mut lower = [0u8; 6];
    for (dst, &b) in lower.iter_mut().zip(bytes) {
        *dst = b.to_ascii_lowercase();
    }
    match &lower[..bytes.len()] {
        b"exe" | b"dll" | b"sys" | b"ocx" | b"pdb" | b"mui" | b"rs" | b"py" | b"js" | b"jsx"
        | b"ts" | b"tsx" | b"c" | b"h" | b"cpp" | b"hpp" | b"cs" | b"java" | b"go" | b"rb"
        | b"php" | b"swift" | b"kt" | b"scala" | b"vue" | b"svelte" | b"html" | b"css"
        | b"scss" | b"json" | b"xml" | b"yaml" | b"yml" | b"toml" | b"sql" | b"sh" | b"ps1" => 0,
        b"png" | b"jpg" | b"jpeg" | b"gif" | b"bmp" | b"webp" | b"svg" | b"ico" | b"tif"
        | b"tiff" | b"heic" | b"avif" | b"raw" | b"psd" => 1,
        b"mp4" | b"mkv" | b"avi" | b"mov" | b"wmv" | b"webm" | b"m4v" | b"mpeg" | b"mpg"
        | b"flv" => 2,
        b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"odt" | b"ods"
        | b"odp" | b"rtf" | b"epub" => 3,
        b"zip" | b"7z" | b"rar" | b"tar" | b"gz" | b"bz2" | b"xz" | b"zst" | b"cab" | b"iso"
        | b"tgz" => 4,
        b"txt" | b"md" | b"csv" | b"log" | b"ini" | b"cfg" | b"conf" | b"rst" => 5,
        b"mp3" | b"wav" | b"flac" | b"aac" | b"ogg" | b"m4a" | b"wma" | b"opus" | b"aiff" => 6,
        _ => 7,
    }
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
