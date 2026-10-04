use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<String>,
}
impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            source: None,
            record: None,
            column: None,
        }
    }
}
impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        Self::new("DATABASE_ERROR", e.to_string())
    }
}
impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        Self::new("IO_ERROR", e.to_string())
    }
}
pub type Result<T> = std::result::Result<T, ApiError>;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    Added,
    Removed,
    Modified,
    Unchanged,
    TypeChanged,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Metric {
    Size,
    Allocated,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ChartMode {
    Before,
    After,
    Delta,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SideValue {
    pub kind: String,
    pub size: String,
    pub allocated: String,
    pub modified: Option<String>,
    pub attributes: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildRow {
    pub node_id: String,
    pub name: String,
    pub expandable: bool,
    pub status: Status,
    pub has_changes: bool,
    pub changed_descendant_count: u64,
    pub child_count: u64,
    pub before: Option<SideValue>,
    pub after: Option<SideValue>,
    pub size_delta: String,
    pub allocated_delta: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildPage {
    pub rows: Vec<ChildRow>,
    pub next_cursor: Option<String>,
    pub total_children: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: String,
    pub kind: String,
    pub size: String,
    pub allocated: String,
    pub modified: Option<String>,
    pub attributes: Option<String>,
    pub files: Option<String>,
    pub folders: Option<String>,
    pub mft: Option<String>,
    pub parent_mft: Option<String>,
    pub accessed: Option<String>,
    pub created: Option<String>,
    pub direct_size: Option<String>,
    pub direct_allocated: Option<String>,
    pub drive_capacity: Option<String>,
    pub free_space: Option<String>,
    pub used_space: Option<String>,
    pub reserved_space: Option<String>,
    pub hardlink_count: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeDetails {
    pub node_id: String,
    pub parent_id: Option<String>,
    pub before: Option<Entry>,
    pub after: Option<Entry>,
    pub size_delta: String,
    pub allocated_delta: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Root {
    pub node_id: String,
    pub path: String,
    pub kind: String,
    pub size: String,
    pub allocated: String,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum SnapshotSide {
    Before,
    After,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RootPage {
    pub rows: Vec<Root>,
    pub next_cursor: Option<String>,
    pub total_roots: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceSummary {
    pub path: String,
    pub description: Option<String>,
    pub rows: u64,
    pub files: u64,
    pub folders: u64,
    pub roots: Vec<Root>,
    pub root_count: u64,
    pub roots_truncated: bool,
    pub size: String,
    pub allocated: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StatusCounts {
    pub added: u64,
    pub removed: u64,
    pub modified: u64,
    pub unchanged: u64,
    pub type_changed: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Counts {
    pub files: StatusCounts,
    pub folders: StatusCounts,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonSummary {
    pub comparison_id: String,
    pub before: SourceSummary,
    pub after: SourceSummary,
    pub statuses: Counts,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobSummary {
    pub job_id: String,
    pub state: String,
    pub phase: String,
    pub bytes_read: String,
    pub total_bytes: String,
    pub rows: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comparison_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TypeValue {
    pub size: String,
    pub allocated: String,
    pub files: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionItem {
    pub extension: String,
    pub size: String,
    pub allocated: String,
    pub files: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPage {
    pub rows: Vec<ExtensionItem>,
    pub next_cursor: Option<String>,
    pub total_extensions: u64,
    pub total: TypeValue,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreemapRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreemapLabel {
    pub node_id: String,
    pub name: String,
    pub path: String,
    pub kind: NodeKind,
    pub weight: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FullTreemapData {
    pub image_data_url: String,
    pub atlas_width: u32,
    pub atlas_height: u32,
    pub file_count: u64,
    pub visible_file_count: u64,
    pub rendered_block_count: u64,
    pub max_depth: u32,
    pub added_file_count: u64,
    pub weight_total: String,
    pub positive_total: String,
    pub negative_total: String,
    pub net_delta: String,
    pub exported_total: String,
    pub labels: Vec<TreemapLabel>,
    pub warnings: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub enum NodeKind {
    File,
    Directory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TreemapHit {
    pub node_id: String,
    pub name: String,
    pub path: String,
    pub extension: String,
    pub weight: String,
    pub value: String,
    pub status: Status,
    pub kind: NodeKind,
    pub collapsed: bool,
    pub rect: TreemapRect,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCategoryItem {
    pub category: String,
    pub before: TypeValue,
    pub after: TypeValue,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCategoriesData {
    pub items: Vec<FileCategoryItem>,
    pub before: TypeValue,
    pub after: TypeValue,
    pub warnings: Vec<String>,
}
