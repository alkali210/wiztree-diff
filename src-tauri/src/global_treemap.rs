//! Global leaf-weighted treemaps with persisted, depth-limited geometry. SQLite
//! owns hierarchy, aggregates, and layout; Rust retains no sibling/path/tree lists.
use crate::{import::JobControl, store, types::*};
use base64::Engine;
use rusqlite::{blob::Blob, params, Connection, DatabaseName, OptionalExtension, Statement};
use std::collections::{HashMap, VecDeque};

pub const ATLAS_WIDTH: u32 = 4096;
pub const ATLAS_HEIGHT: u32 = 1024;
// 144 records fit one 8 KiB SQLite leaf page without overflow-page slack.
const SPATIAL_BLOCK: usize = 144;
const GEOMETRY_BYTES: usize = 56;
const LOCATOR_KEYS: i64 = 2048;
const LOCATOR_CACHE: usize = 64;
const DIRECTORY_HEADER: f64 = 64.0;
const DIRECTORY_GUTTER: f64 = 8.0;
const PALETTE: [[u8; 3]; 16] = [
    [70, 124, 227],
    [152, 72, 209],
    [35, 166, 124],
    [207, 126, 26],
    [226, 68, 88],
    [173, 173, 29],
    [30, 163, 202],
    [191, 72, 162],
    [86, 166, 75],
    [102, 90, 205],
    [206, 106, 44],
    [45, 153, 160],
    [193, 68, 55],
    [105, 141, 30],
    [125, 125, 139],
    [41, 114, 180],
];
pub fn extension_color(extension: &str) -> [u8; 3] {
    let mut hash = 2166136261u32;
    for byte in extension.bytes() {
        hash = (hash ^ byte as u32).wrapping_mul(16777619);
    }
    PALETTE[(hash & 15) as usize]
}
fn view(metric: Metric, mode: ChartMode) -> usize {
    let metric = match metric {
        Metric::Size => 0,
        Metric::Allocated => 3,
    };
    metric
        + match mode {
            ChartMode::Before => 0,
            ChartMode::After => 1,
            ChartMode::Delta => 2,
        }
}
const COLUMNS: [&str; 6] = ["sb", "sa", "sb", "ab", "aa", "ab"];

pub fn materialize(conn: &Connection, control: &JobControl) -> Result<()> {
    control.check()?;
    *control
        .interrupt
        .lock()
        .map_err(|_| ApiError::new("LOCK_ERROR", "取消锁不可用"))? =
        Some(conn.get_interrupt_handle());
    let result = build(conn, control);
    // SQLite interruption is a cancellation, not an opaque database failure.
    control.check()?;
    result
}
fn build(conn: &Connection, control: &JobControl) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    conn.execute_batch("CREATE TEMP TABLE treemap_palette(extension TEXT PRIMARY KEY,color INTEGER NOT NULL) WITHOUT ROWID;")?;
    {
        let mut scan = conn
            .prepare("SELECT extension FROM extension_stats WHERE node_id=0 GROUP BY extension")?;
        let mut insert = conn.prepare("INSERT INTO treemap_palette VALUES(?1,?2)")?;
        let mut rows = scan.query([])?;
        while let Some(row) = rows.next()? {
            control.check()?;
            let extension = store::root_text(row, 0)?;
            let [r, g, b] = extension_color(extension);
            let color = (r as u32) << 16 | (g as u32) << 8 | b as u32;
            insert.execute(params![extension, color])?;
        }
    }
    // Ordinary files are their own leaves. Directory/file type changes and
    // sparse exports with children under an opposite-side file retain both
    // a container and an own-file leaf. Positive sibling keys stay unchanged.
    conn.execute_batch("CREATE INDEX IF NOT EXISTS nodes_parent ON node_records(parent_id);
        CREATE TABLE treemap_weights(
        key INTEGER PRIMARY KEY,node_id INTEGER NOT NULL,parent_id INTEGER NOT NULL,depth INTEGER NOT NULL,leaf INTEGER NOT NULL,
        sb INTEGER NOT NULL CHECK(typeof(sb)='integer'),sa INTEGER NOT NULL CHECK(typeof(sa)='integer'),
        ab INTEGER NOT NULL CHECK(typeof(ab)='integer'),aa INTEGER NOT NULL CHECK(typeof(aa)='integer'),
        size_value INTEGER NOT NULL CHECK(typeof(size_value)='integer'),allocated_value INTEGER NOT NULL CHECK(typeof(allocated_value)='integer'),
        before_file INTEGER NOT NULL,after_file INTEGER NOT NULL,
        added INTEGER NOT NULL,removed INTEGER NOT NULL,type_changed INTEGER NOT NULL,modified INTEGER NOT NULL,color INTEGER NOT NULL);
        WITH RECURSIVE parents(id) AS MATERIALIZED (SELECT DISTINCT parent_id FROM node_records WHERE parent_id IS NOT NULL),
        levels(id,level) AS (
            SELECT n.id,1 FROM node_records n JOIN parents p ON p.id=n.id WHERE n.parent_id IS NULL
            UNION ALL SELECT n.id,l.level+1 FROM levels l JOIN node_records n ON n.parent_id=l.id JOIN parents p ON p.id=n.id)
        , own AS (SELECT n.id,coalesce(n.parent_id,0) parent_id,coalesce(l.level+1,1) level,
            CASE WHEN c.before_kind='directory' OR c.after_kind='directory' OR child.id IS NOT NULL THEN 0 ELSE 1 END leaf,
            CASE WHEN c.before_kind='file' THEN c.before_size ELSE 0 END sb,CASE WHEN c.after_kind='file' THEN c.after_size ELSE 0 END sa,
            CASE WHEN c.before_kind='file' THEN c.before_allocated ELSE 0 END ab,CASE WHEN c.after_kind='file' THEN c.after_allocated ELSE 0 END aa,
            coalesce(c.before_kind='file',0) bf,coalesce(c.after_kind='file',0) af,
            coalesce(c.after_kind='file' AND (c.before_kind IS NULL OR c.before_kind!='file'),0) added,
            coalesce(c.status='removed',0) removed,coalesce(c.status='typeChanged',0) changed,coalesce(c.status='modified',0) modified,
            CASE WHEN c.before_kind='file' OR c.after_kind='file' THEN p.color ELSE 0 END color
            FROM node_records n NOT INDEXED CROSS JOIN comparison_nodes c ON c.node_id=n.id
            LEFT JOIN levels l ON l.id=n.parent_id LEFT JOIN parents child ON child.id=n.id LEFT JOIN treemap_palette p ON p.extension=c.extension)
        INSERT INTO treemap_weights SELECT id,id,parent_id,level,leaf,
            sb*leaf,sa*leaf,ab*leaf,aa*leaf,(sa-sb)*leaf,(aa-ab)*leaf,
            bf*leaf,af*leaf,added*leaf,removed*leaf,changed*leaf,modified*leaf,color FROM own;
        CREATE INDEX treemap_depth ON treemap_weights(depth,key) WHERE leaf=0;
        INSERT INTO treemap_weights
        SELECT -c.node_id,c.node_id,c.node_id,w.depth+1,1,
        CASE WHEN c.before_kind='file' THEN c.before_size ELSE 0 END,CASE WHEN c.after_kind='file' THEN c.after_size ELSE 0 END,
        CASE WHEN c.before_kind='file' THEN c.before_allocated ELSE 0 END,CASE WHEN c.after_kind='file' THEN c.after_allocated ELSE 0 END,
        (CASE WHEN c.after_kind='file' THEN c.after_size ELSE 0 END)-(CASE WHEN c.before_kind='file' THEN c.before_size ELSE 0 END),
        (CASE WHEN c.after_kind='file' THEN c.after_allocated ELSE 0 END)-(CASE WHEN c.before_kind='file' THEN c.before_allocated ELSE 0 END),
        coalesce(c.before_kind='file',0),coalesce(c.after_kind='file',0),
        coalesce(c.after_kind='file' AND (c.before_kind IS NULL OR c.before_kind!='file'),0),coalesce(c.status='removed',0),
        coalesce(c.status='typeChanged',0),coalesce(c.status='modified',0),p.color
        FROM treemap_weights w INDEXED BY treemap_depth CROSS JOIN comparison_nodes c ON c.node_id=w.node_id
        JOIN treemap_palette p ON p.extension=c.extension
        WHERE w.leaf=0 AND (c.before_kind='file' OR c.after_kind='file');
        DROP TABLE treemap_palette;
        CREATE INDEX treemap_parent ON treemap_weights(parent_id);
        CREATE TABLE treemap_rects(id INTEGER PRIMARY KEY,view INTEGER NOT NULL,key INTEGER NOT NULL,node_id INTEGER NOT NULL,leaf INTEGER NOT NULL,
            x REAL NOT NULL,y REAL NOT NULL,width REAL NOT NULL,height REAL NOT NULL,weight INTEGER NOT NULL,value INTEGER NOT NULL,
            header_height REAL NOT NULL DEFAULT 0, UNIQUE(view,key));
        CREATE TABLE treemap_reuse(id INTEGER PRIMARY KEY,view INTEGER NOT NULL,parent_id INTEGER NOT NULL,source_view INTEGER NOT NULL,
            sx REAL NOT NULL,sy REAL NOT NULL,tx REAL NOT NULL,ty REAL NOT NULL,UNIQUE(view,parent_id));
        CREATE TABLE treemap_blocks(id INTEGER PRIMARY KEY,view INTEGER NOT NULL,data BLOB,source INTEGER,start INTEGER,count INTEGER,reuse_id INTEGER);
        CREATE INDEX treemap_block_view ON treemap_blocks(view,id);
        CREATE TABLE treemap_key_pages(id INTEGER PRIMARY KEY,view INTEGER NOT NULL,page INTEGER NOT NULL,data BLOB NOT NULL,UNIQUE(view,page));
        CREATE TABLE treemap_aliases(view INTEGER PRIMARY KEY,geometry INTEGER NOT NULL);
        CREATE TABLE treemap_families(id INTEGER PRIMARY KEY,view INTEGER NOT NULL UNIQUE);
        CREATE VIRTUAL TABLE treemap_spatial USING rtree(id,x0,x1,y0,y1);
        CREATE TABLE treemap_frames(view INTEGER PRIMARY KEY,png BLOB NOT NULL,file_count INTEGER NOT NULL,visible_file_count INTEGER NOT NULL,
            weight_total INTEGER NOT NULL,positive_total INTEGER NOT NULL,negative_total INTEGER NOT NULL,net_delta INTEGER NOT NULL,exported_total INTEGER NOT NULL,rendered_block_count INTEGER NOT NULL,added_file_count INTEGER NOT NULL,labels TEXT NOT NULL,warnings TEXT NOT NULL);").map_err(store::aggregate_error)?;
    let depth: i64 = conn.query_row(
        "SELECT coalesce(max(depth),0)+1 FROM treemap_weights WHERE leaf=0",
        [],
        |r| r.get(0),
    )?;
    let mut propagate = conn.prepare("UPDATE treemap_weights SET (sb,sa,ab,aa,size_value,allocated_value,before_file,after_file,added,removed,type_changed,modified)=(SELECT coalesce(sum(c.sb),0),coalesce(sum(c.sa),0),coalesce(sum(c.ab),0),coalesce(sum(c.aa),0),coalesce(sum(c.size_value),0),coalesce(sum(c.allocated_value),0),coalesce(sum(c.before_file),0),coalesce(sum(c.after_file),0),coalesce(sum(c.added),0),coalesce(sum(c.removed),0),coalesce(sum(c.type_changed),0),coalesce(sum(c.modified),0) FROM treemap_weights c INDEXED BY treemap_parent WHERE c.parent_id=treemap_weights.key) WHERE depth=?1 AND leaf=0")?;
    for d in (1..depth).rev() {
        control.check()?;
        propagate.execute([d]).map_err(store::aggregate_error)?;
    }
    // Prepare all modes of the initial metric, not unvisited metric families.
    let mut pixels = vec![0u8; ATLAS_WIDTH as usize * ATLAS_HEIGHT as usize * 4];
    generate_family(conn, control, Metric::Size, 0, &mut pixels)?;
    transaction.commit()?;
    Ok(())
}

fn cache_view(metric: Metric, mode: ChartMode, max_depth: u32) -> i64 {
    max_depth as i64 * 6 + view(metric, mode) as i64
}

fn ensure_frame(conn: &Connection, metric: Metric, mode: ChartMode, max_depth: u32) -> Result<()> {
    let v = cache_view(metric, mode, max_depth);
    if conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM treemap_frames WHERE view=?1)",
        [v],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    // Readers remain read-only. A short-lived sibling owns the persistent cache transaction.
    let writer = conn
        .path()
        .filter(|p| !p.is_empty())
        .map(Connection::open)
        .transpose()?;
    let conn = writer.as_ref().unwrap_or(conn);
    conn.busy_timeout(std::time::Duration::from_secs(30))?;
    conn.execute_batch("PRAGMA cache_size=-65536; PRAGMA temp_store=FILE; PRAGMA temp.cache_size=-32768; PRAGMA mmap_size=536870912;")?;
    let transaction = conn.unchecked_transaction()?;
    let control = JobControl::default();
    let mut pixels = vec![0u8; ATLAS_WIDTH as usize * ATLAS_HEIGHT as usize * 4];
    generate_family(conn, &control, metric, max_depth, &mut pixels)?;
    transaction.commit()?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

fn generate_family(
    conn: &Connection,
    control: &JobControl,
    metric: Metric,
    depth: u32,
    pixels: &mut [u8],
) -> Result<()> {
    for mode in [ChartMode::Before, ChartMode::After, ChartMode::Delta] {
        generate(conn, control, metric, mode, depth, pixels)?;
    }
    conn.execute_batch("DROP TABLE IF EXISTS temp.treemap_runs;")?;
    let base = cache_view(metric, ChartMode::Before, depth);
    conn.execute(
        "INSERT OR IGNORE INTO treemap_families(view) VALUES(?1)",
        [base],
    )?;
    let pinned = cache_view(metric, ChartMode::Before, 0);
    // Keep the default family and two on-demand depths for each metric. Eviction
    // only happens in the writer transaction, never during a mode switch.
    let mut oldest=conn.prepare("SELECT view FROM treemap_families WHERE view%6=?1 AND view!=?2 ORDER BY id DESC LIMIT -1 OFFSET 2")?;
    let mut rows = oldest.query(params![base % 6, pinned])?;
    while let Some(row) = rows.next()? {
        let old: i64 = row.get(0)?;
        conn.execute("DELETE FROM treemap_spatial WHERE id IN (SELECT id FROM treemap_blocks WHERE view>=?1 AND view<?1+3)",[old])?;
        for table in [
            "treemap_rects",
            "treemap_key_pages",
            "treemap_blocks",
            "treemap_reuse",
            "treemap_aliases",
            "treemap_frames",
        ] {
            conn.execute(
                &format!("DELETE FROM {table} WHERE view>=?1 AND view<?1+3"),
                [old],
            )?;
        }
        conn.execute("DELETE FROM treemap_families WHERE view=?1", [old])?;
    }
    Ok(())
}

fn geometry_view(conn: &Connection, v: i64) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT geometry FROM treemap_aliases WHERE view=?1",
        [v],
        |r| r.get(0),
    )?)
}

fn generate(
    conn: &Connection,
    control: &JobControl,
    metric: Metric,
    mode: ChartMode,
    max_depth: u32,
    pixels: &mut [u8],
) -> Result<()> {
    let v = cache_view(metric, mode, max_depth);
    if conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM treemap_frames WHERE view=?1)",
        [v],
        |r| r.get::<_, bool>(0),
    )? {
        return Ok(());
    }
    let totals = totals(conn, metric, mode)?;
    let before = cache_view(metric, ChartMode::Before, max_depth);
    // A Delta frame is a different rendering of the same immutable geometry.
    let equal = matches!(mode, ChartMode::After) && conn.query_row(
        &format!("SELECT NOT EXISTS(SELECT 1 FROM treemap_weights w JOIN comparison_nodes c ON c.node_id=w.node_id WHERE w.{}!=w.{} OR (w.leaf=0 AND coalesce(c.before_kind='directory',0)!=coalesce(c.after_kind='directory',0)))", COLUMNS[view(metric, ChartMode::Before)], COLUMNS[view(metric, mode)]), [], |r| r.get::<_, bool>(0))?;
    let geometry = if matches!(mode, ChartMode::Delta) || equal {
        geometry_view(conn, before)?
    } else {
        v
    };
    conn.execute(
        "INSERT INTO treemap_aliases VALUES(?1,?2)",
        params![v, geometry],
    )?;
    if geometry == v && totals.weight > 0 {
        let mut layout = Layout::new(
            conn,
            control,
            v,
            view(metric, mode),
            COLUMNS[view(metric, mode)],
        )?;
        layout.parent(
            0,
            BoxRect {
                x: 0.0,
                y: 0.0,
                w: ATLAS_WIDTH as f64,
                h: ATLAS_HEIGHT as f64,
            },
            totals.weight,
        )?;
        let depth: i64 = conn.query_row(
            "SELECT coalesce(max(depth),0)+1 FROM treemap_weights WHERE leaf=0",
            [],
            |r| r.get(0),
        )?;
        let side = if matches!(mode, ChartMode::Before) {
            0
        } else {
            1
        };
        let mut parents = conn.prepare("SELECT w.key,r.x,r.y,r.width,r.height,r.weight,coalesce(CASE WHEN ?3=0 THEN c.before_kind ELSE c.after_kind END='directory',0) FROM treemap_weights w INDEXED BY treemap_depth JOIN treemap_rects r ON r.view=?1 AND r.key=w.key JOIN comparison_nodes c ON c.node_id=w.node_id WHERE w.depth=?2 AND w.leaf=0 ORDER BY w.key")?;
        let end = if max_depth == 0 {
            depth
        } else {
            depth.min(max_depth as i64 + 1)
        };
        for d in 1..end {
            control.check()?;
            let mut rows = parents.query(params![v, d, side])?;
            while let Some(row) = rows.next()? {
                layout.tick()?;
                let key: i64 = row.get(0)?;
                let is_directory: bool = row.get(6)?;
                let mut content = BoxRect {
                    x: row.get::<_, f64>(1)? * ATLAS_WIDTH as f64,
                    y: row.get::<_, f64>(2)? * ATLAS_HEIGHT as f64,
                    w: row.get::<_, f64>(3)? * ATLAS_WIDTH as f64,
                    h: row.get::<_, f64>(4)? * ATLAS_HEIGHT as f64,
                };
                if is_directory && content.w >= 256.0 && content.h >= 192.0 {
                    conn.execute(
                        "UPDATE treemap_rects SET header_height=?3 WHERE view=?1 AND key=?2",
                        params![v, key, DIRECTORY_HEADER / ATLAS_HEIGHT as f64],
                    )?;
                    content.x += DIRECTORY_GUTTER;
                    content.y += DIRECTORY_HEADER;
                    content.w -= DIRECTORY_GUTTER * 2.0;
                    content.h -= DIRECTORY_HEADER + DIRECTORY_GUTTER;
                }
                if max_depth != 0 && d >= max_depth as i64 && is_directory {
                    conn.execute(
                        "UPDATE treemap_rects SET leaf=1 WHERE view=?1 AND key=?2",
                        params![v, key],
                    )?;
                } else {
                    layout.parent(key, content, row.get(5)?)?;
                }
            }
        }
        layout.writer.finish()?;
        index_geometry(conn, control, v)?;
    }
    let png = if equal {
        conn.query_row(
            "SELECT png FROM treemap_frames WHERE view=?1",
            [before],
            |r| r.get::<_, Vec<u8>>(0),
        )?
    } else {
        paint(conn, control, geometry, mode, pixels)?;
        let mut png = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut png, ATLAS_WIDTH, ATLAS_HEIGHT);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            encoder
                .write_header()
                .map_err(png_error)?
                .write_image_data(pixels)
                .map_err(png_error)?;
        }
        png
    };
    control.check()?;
    let (labels, rendered) = frame_labels(conn, control, geometry, mode)?;
    let warnings = frame_warnings(conn, metric, mode, totals.added)?;
    conn.execute(
        "INSERT INTO treemap_frames VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
        params![
            v,
            png,
            totals.files,
            totals.visible,
            totals.weight,
            totals.positive,
            totals.negative,
            totals.net,
            totals.exported,
            rendered,
            totals.added,
            serde_json::to_string(&labels).map_err(json_error)?,
            serde_json::to_string(&warnings).map_err(json_error)?
        ],
    )?;
    Ok(())
}
fn png_error(e: png::EncodingError) -> ApiError {
    ApiError::new("IMAGE_ERROR", e.to_string())
}
struct Totals {
    files: i64,
    visible: i64,
    weight: i64,
    positive: i64,
    negative: i64,
    net: i64,
    exported: i64,
    added: i64,
}
fn totals(conn: &Connection, metric: Metric, mode: ChartMode) -> Result<Totals> {
    let v = view(metric, mode);
    let value = match metric {
        Metric::Size => "size_value",
        Metric::Allocated => "allocated_value",
    };
    let files = match mode {
        ChartMode::Before => "sum(before_file)",
        ChartMode::After => "sum(after_file)",
        ChartMode::Delta => "count(*)",
    };
    let visible = if matches!(mode, ChartMode::Delta) {
        "added>0 OR removed>0 OR type_changed>0 OR modified>0"
    } else {
        "1"
    };
    let sql = format!("SELECT coalesce({files},0),coalesce(sum({w}>0 AND ({visible})),0),coalesce(sum({w}),0),coalesce(sum(CASE WHEN {value}>0 THEN {value} ELSE 0 END),0),coalesce(sum(CASE WHEN {value}<0 THEN -{value} ELSE 0 END),0),coalesce(sum({value}),0),coalesce(sum(added),0) FROM treemap_weights WHERE leaf=1", w=COLUMNS[v]);
    let mut t = conn
        .query_row(&sql, [], |r| {
            Ok(Totals {
                files: r.get(0)?,
                visible: r.get(1)?,
                weight: r.get(2)?,
                positive: r.get(3)?,
                negative: r.get(4)?,
                net: r.get(5)?,
                exported: 0,
                added: r.get(6)?,
            })
        })
        .map_err(store::aggregate_error)?;
    let col = match metric {
        Metric::Size => "size",
        Metric::Allocated => "allocated",
    };
    let exported = |side: i64| -> Result<i64> {
        conn.query_row(&format!("SELECT coalesce(sum(e.{col}),0) FROM export_roots r JOIN entries e ON e.node_id=r.node_id AND e.side=r.side WHERE r.eligible=1 AND r.side=?1"), [side], |r| r.get(0)).map_err(store::aggregate_error)
    };
    t.exported = match mode {
        ChartMode::Before => exported(0)?,
        ChartMode::After => exported(1)?,
        ChartMode::Delta => exported(1)?
            .checked_sub(exported(0)?)
            .ok_or_else(|| ApiError::new("AGGREGATE_OVERFLOW", "导出总量溢出"))?,
    };
    Ok(t)
}
#[derive(Clone, Copy)]
struct BoxRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}
#[derive(Clone, Copy)]
struct Key {
    weight: i64,
    key: i64,
}
// Each immutable record is key + four f64 bounds + i64 weight + flags/RGB.
// Directory decoration is tiny mutable metadata, never a rewrite of file blocks.
#[derive(Clone, Copy)]
struct Geometry {
    key: i64,
    rect: BoxRect,
    weight: i64,
    leaf: bool,
    header: f64,
    color: [u8; 3],
    marks: u8,
}
impl Geometry {
    fn decode(data: &[u8]) -> Self {
        let word = |n: usize| <[u8; 8]>::try_from(&data[n..n + 8]).unwrap();
        Self {
            key: i64::from_le_bytes(word(0)),
            rect: BoxRect {
                x: f64::from_le_bytes(word(8)),
                y: f64::from_le_bytes(word(16)),
                w: f64::from_le_bytes(word(24)),
                h: f64::from_le_bytes(word(32)),
            },
            weight: i64::from_le_bytes(word(40)),
            leaf: data[48] != 0,
            header: 0.0,
            color: [data[49], data[50], data[51]],
            marks: data[52],
        }
    }
    fn encode(self, data: &mut Vec<u8>) {
        data.extend_from_slice(&self.key.to_le_bytes());
        for n in [self.rect.x, self.rect.y, self.rect.w, self.rect.h] {
            data.extend_from_slice(&n.to_le_bytes());
        }
        data.extend_from_slice(&self.weight.to_le_bytes());
        data.extend_from_slice(&[
            self.leaf as u8,
            self.color[0],
            self.color[1],
            self.color[2],
            self.marks,
            0,
            0,
            0,
        ]);
    }
    fn node(self) -> i64 {
        self.key.abs()
    }
    fn visible(self, mode: ChartMode) -> bool {
        !matches!(mode, ChartMode::Delta) || self.marks != 0
    }
    fn bounds(self) -> TreemapRect {
        TreemapRect {
            x: self.rect.x,
            y: self.rect.y,
            width: self.rect.w,
            height: self.rect.h,
        }
    }
}
// Dense key pages avoid an indexed SQL row and random B-tree insertion for
// every rectangle. Only 64 SQLite blob handles are retained while building.
struct LocatorWriter<'a> {
    conn: &'a Connection,
    view: i64,
    pages: HashMap<i64, Blob<'a>>,
    order: VecDeque<i64>,
    lookup: Statement<'a>,
    create: Statement<'a>,
}
impl<'a> LocatorWriter<'a> {
    fn new(conn: &'a Connection, view: i64) -> Result<Self> {
        Ok(Self {
            conn,
            view,
            pages: HashMap::with_capacity(LOCATOR_CACHE),
            order: VecDeque::with_capacity(LOCATOR_CACHE),
            lookup: conn.prepare("SELECT id FROM treemap_key_pages WHERE view=?1 AND page=?2")?,
            create: conn.prepare(
                "INSERT INTO treemap_key_pages(view,page,data) VALUES(?1,?2,zeroblob(?3))",
            )?,
        })
    }
    fn put(&mut self, key: i64, block: i64, slot: usize) -> Result<()> {
        let page = key.div_euclid(LOCATOR_KEYS);
        let offset = key.rem_euclid(LOCATOR_KEYS) as usize * 8;
        let location = (block as u64)
            .checked_mul(SPATIAL_BLOCK as u64)
            .and_then(|n| n.checked_add(slot as u64 + 1))
            .ok_or_else(|| ApiError::new("AGGREGATE_OVERFLOW", "图形定位索引溢出"))?
            .to_le_bytes();
        if let Some(blob) = self.pages.get_mut(&page) {
            blob.write_at(&location, offset)?;
            return Ok(());
        }
        if self.pages.len() == LOCATOR_CACHE {
            let oldest = self.order.pop_front().unwrap();
            self.pages.remove(&oldest).unwrap().close()?;
        }
        let id = match self
            .lookup
            .query_row(params![self.view, page], |r| r.get(0))
            .optional()?
        {
            Some(id) => id,
            None => {
                self.create
                    .execute(params![self.view, page, LOCATOR_KEYS * 8])?;
                self.conn.last_insert_rowid()
            }
        };
        let mut blob =
            self.conn
                .blob_open(DatabaseName::Main, "treemap_key_pages", "data", id, false)?;
        blob.write_at(&location, offset)?;
        self.pages.insert(page, blob);
        self.order.push_back(page);
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        for (_, blob) in self.pages.drain() {
            blob.close()?;
        }
        self.order.clear();
        Ok(())
    }
}
struct BlockWriter<'a> {
    view: i64,
    id: i64,
    data: Vec<u8>,
    blocks: Statement<'a>,
    keys: LocatorWriter<'a>,
}
impl<'a> BlockWriter<'a> {
    fn new(conn: &'a Connection, view: i64) -> Result<Self> {
        Ok(Self {
            view,
            id: conn.query_row(
                "SELECT coalesce(max(id),0)+1 FROM treemap_blocks",
                [],
                |r| r.get(0),
            )?,
            data: Vec::with_capacity(SPATIAL_BLOCK * GEOMETRY_BYTES),
            blocks: conn.prepare("INSERT INTO treemap_blocks(id,view,data) VALUES(?1,?2,?3)")?,
            keys: LocatorWriter::new(conn, view)?,
        })
    }
    fn push(&mut self, g: Geometry) -> Result<()> {
        self.keys
            .put(g.key, self.id, self.data.len() / GEOMETRY_BYTES)?;
        g.encode(&mut self.data);
        if self.data.len() == SPATIAL_BLOCK * GEOMETRY_BYTES {
            self.flush()?;
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if !self.data.is_empty() {
            self.blocks
                .execute(params![self.id, self.view, &self.data])?;
            self.data.clear();
            self.id += 1;
        }
        Ok(())
    }
    fn finish(&mut self) -> Result<()> {
        self.flush()?;
        self.keys.finish()
    }
}
fn decorate(g: &mut Geometry, directory: &mut Statement<'_>, view: i64) -> Result<()> {
    if !g.leaf {
        let (leaf, header): (bool, f64) =
            directory.query_row(params![view, g.key], |r| Ok((r.get(0)?, r.get(1)?)))?;
        g.leaf = leaf;
        g.header = header;
    }
    Ok(())
}
#[derive(Clone, Copy)]
struct Transform {
    sx: f64,
    sy: f64,
    tx: f64,
    ty: f64,
}
impl Transform {
    const IDENTITY: Self = Self {
        sx: 1.0,
        sy: 1.0,
        tx: 0.0,
        ty: 0.0,
    };
    fn rect(self, r: BoxRect) -> BoxRect {
        BoxRect {
            x: r.x * self.sx + self.tx,
            y: r.y * self.sy + self.ty,
            w: r.w * self.sx,
            h: r.h * self.sy,
        }
    }
}
// Alias blocks read just their bounded byte range through one reusable SQLite
// blob handle; small directory runs never copy an entire shared source block.
fn read_blocks(
    conn: &Connection,
    rows: &mut rusqlite::Rows<'_>,
    mut visit: impl FnMut(i64, &[u8], Transform) -> Result<()>,
) -> Result<()> {
    let mut buffer = [0u8; SPATIAL_BLOCK * GEOMETRY_BYTES];
    let mut source: Option<(i64, Blob<'_>)> = None;
    while let Some(row) = rows.next()? {
        let (data, transform) = match row.get_ref(1)? {
            rusqlite::types::ValueRef::Blob(data) => (data, Transform::IDENTITY),
            rusqlite::types::ValueRef::Null => {
                let id: i64 = row.get(2)?;
                let start: usize = row.get(3)?;
                let count: usize = row.get(4)?;
                match source.as_mut() {
                    Some((previous, blob)) => {
                        if *previous != id {
                            blob.reopen(id)?;
                            *previous = id;
                        }
                    }
                    None => {
                        source = Some((
                            id,
                            conn.blob_open(DatabaseName::Main, "treemap_blocks", "data", id, true)?,
                        ))
                    }
                }
                let data = &mut buffer[..count * GEOMETRY_BYTES];
                source
                    .as_ref()
                    .unwrap()
                    .1
                    .read_at_exact(data, start * GEOMETRY_BYTES)?;
                (
                    &*data,
                    Transform {
                        sx: row.get(5)?,
                        sy: row.get(6)?,
                        tx: row.get(7)?,
                        ty: row.get(8)?,
                    },
                )
            }
            _ => return Err(ApiError::new("CACHE_INVALID", "图形块损坏，请重新导入")),
        };
        visit(row.get(0)?, data, transform)?;
    }
    Ok(())
}
fn scan_geometry(
    conn: &Connection,
    control: &JobControl,
    view: i64,
    mut visit: impl FnMut(Geometry) -> Result<()>,
) -> Result<()> {
    let mut blocks=conn.prepare("SELECT b.id,b.data,b.source,b.start,b.count,r.sx,r.sy,r.tx,r.ty FROM treemap_blocks b LEFT JOIN treemap_reuse r ON r.id=b.reuse_id WHERE b.view=?1 ORDER BY b.id")?;
    let mut directory =
        conn.prepare("SELECT leaf,header_height FROM treemap_rects WHERE view=?1 AND key=?2")?;
    let mut rows = blocks.query([view])?;
    read_blocks(conn, &mut rows, |_, data, transform| {
        control.check()?;
        for record in data.chunks_exact(GEOMETRY_BYTES) {
            let mut g = Geometry::decode(record);
            g.rect = transform.rect(g.rect);
            decorate(&mut g, &mut directory, view)?;
            visit(g)?;
        }
        Ok(())
    })
}
fn index_geometry(conn: &Connection, control: &JobControl, view: i64) -> Result<()> {
    let mut blocks=conn.prepare("SELECT b.id,b.data,b.source,b.start,b.count,r.sx,r.sy,r.tx,r.ty FROM treemap_blocks b LEFT JOIN treemap_reuse r ON r.id=b.reuse_id WHERE b.view=?1 ORDER BY b.id")?;
    let mut directory =
        conn.prepare("SELECT leaf,header_height FROM treemap_rects WHERE view=?1 AND key=?2")?;
    let mut insert = conn.prepare("INSERT INTO treemap_spatial VALUES(?1,?2,?3,?4,?5)")?;
    let mut rows = blocks.query([view])?;
    read_blocks(conn, &mut rows, |id, data, transform| {
        control.check()?;
        let mut bbox = [
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
        ];
        for record in data.chunks_exact(GEOMETRY_BYTES) {
            let mut g = Geometry::decode(record);
            g.rect = transform.rect(g.rect);
            decorate(&mut g, &mut directory, view)?;
            if g.leaf || g.header > 0.0 {
                bbox[0] = bbox[0].min(g.rect.x);
                bbox[1] = bbox[1].max(g.rect.x + g.rect.w);
                bbox[2] = bbox[2].min(g.rect.y);
                bbox[3] = bbox[3].max(g.rect.y + if g.leaf { g.rect.h } else { g.header });
            }
        }
        if bbox[0].is_finite() {
            insert.execute(params![id, bbox[0], bbox[1], bbox[2], bbox[3]])?;
        }
        Ok(())
    })
}
fn cached_geometry(conn: &Connection, view: i64, key: i64) -> Result<Option<Geometry>> {
    if let Some(g) = local_geometry(conn, view, key)? {
        return Ok(Some(g));
    }
    let reused:Option<(i64,bool,Transform)>=conn.query_row("SELECT r.source_view,w.leaf,r.sx,r.sy,r.tx,r.ty FROM treemap_weights w JOIN treemap_reuse r ON r.parent_id=w.parent_id AND r.view=?1 WHERE w.key=?2",params![view,key],|r|Ok((r.get(0)?,r.get(1)?,Transform{sx:r.get(2)?,sy:r.get(3)?,tx:r.get(4)?,ty:r.get(5)?}))).optional()?;
    let Some((source, leaf, transform)) = reused else {
        return Ok(None);
    };
    let Some(mut g) = local_geometry(conn, source, key)? else {
        return Ok(None);
    };
    g.rect = transform.rect(g.rect);
    g.leaf = leaf;
    g.header = 0.0;
    let mut directory =
        conn.prepare("SELECT leaf,header_height FROM treemap_rects WHERE view=?1 AND key=?2")?;
    decorate(&mut g, &mut directory, view)?;
    Ok(Some(g))
}
fn local_geometry(conn: &Connection, view: i64, key: i64) -> Result<Option<Geometry>> {
    let page = key.div_euclid(LOCATOR_KEYS);
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM treemap_key_pages WHERE view=?1 AND page=?2",
            params![view, page],
            |r| r.get(0),
        )
        .optional()?;
    let Some(id) = id else { return Ok(None) };
    let mut location = [0u8; 8];
    {
        let blob = conn.blob_open(DatabaseName::Main, "treemap_key_pages", "data", id, true)?;
        blob.read_at_exact(&mut location, key.rem_euclid(LOCATOR_KEYS) as usize * 8)?;
    }
    let location = u64::from_le_bytes(location);
    if location == 0 {
        return Ok(None);
    }
    let block = ((location - 1) / SPATIAL_BLOCK as u64) as i64;
    let slot = ((location - 1) % SPATIAL_BLOCK as u64) as usize;
    let mut query = conn.prepare("SELECT data FROM treemap_blocks WHERE id=?1")?;
    let mut g = query.query_row([block], |r| {
        let data = r.get_ref(0)?.as_blob().map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e))
        })?;
        Ok(Geometry::decode(
            &data[slot * GEOMETRY_BYTES..(slot + 1) * GEOMETRY_BYTES],
        ))
    })?;
    let mut directory =
        conn.prepare("SELECT leaf,header_height FROM treemap_rects WHERE view=?1 AND key=?2")?;
    decorate(&mut g, &mut directory, view)?;
    Ok(Some(g))
}
struct Layout<'a> {
    control: &'a JobControl,
    v: i64,
    scan: Statement<'a>,
    range: Statement<'a>,
    equal_range: Statement<'a>,
    insert: Statement<'a>,
    writer: BlockWriter<'a>,
    ticks: u64,
    after: bool,
    same: Statement<'a>,
    old_content: Statement<'a>,
    runs: Statement<'a>,
    run: Statement<'a>,
    copy: Statement<'a>,
    reuse: Statement<'a>,
    aliases: Statement<'a>,
}
impl<'a> Layout<'a> {
    fn new(
        conn: &'a Connection,
        control: &'a JobControl,
        v: i64,
        order: usize,
        col: &str,
    ) -> Result<Self> {
        conn.execute_batch("CREATE TEMP TABLE IF NOT EXISTS treemap_runs(parent_id INTEGER PRIMARY KEY,first INTEGER,last INTEGER,start INTEGER,end INTEGER);")?;
        let after = order % 3 == 1;
        if !after {
            conn.execute_batch("DELETE FROM temp.treemap_runs;")?;
        }
        let (before_col, after_col) = if order < 3 {
            ("sb", "sa")
        } else {
            ("ab", "aa")
        };
        let base = format!(
            "FROM treemap_weights INDEXED BY treemap_order_{order} WHERE parent_id=?1 AND {col}>0"
        );
        let value = col;
        conn.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS treemap_order_{order} ON treemap_weights(parent_id,{col} DESC,key DESC) WHERE {col}>0"
        ))?;
        Ok(Self {control,v,
            scan:conn.prepare(&format!("SELECT key,{col} {base} ORDER BY {col} DESC,key DESC"))?,
            range:conn.prepare(&format!("SELECT key,node_id,leaf,{col},{value},color,added,removed,type_changed,modified {base} AND ({col},key)<=(?2,?3) AND ({col},key)>=(?4,?5) ORDER BY {col} DESC,key DESC"))?,
            equal_range:conn.prepare(&format!("SELECT key,node_id,leaf,{col},{value},color,added,removed,type_changed,modified {base} AND {col}=?2 AND key<=?3 AND key>=?4 ORDER BY key DESC"))?,
            insert:conn.prepare("INSERT INTO treemap_rects(view,key,node_id,leaf,x,y,width,height,weight,value) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?,
            writer:BlockWriter::new(conn,v)?,ticks:0,after,
            same:conn.prepare(&format!("SELECT NOT EXISTS(SELECT 1 FROM treemap_weights INDEXED BY treemap_parent WHERE parent_id=?1 AND {before_col}!={after_col})"))?,
            old_content:conn.prepare("SELECT x,y,width,height,header_height FROM treemap_rects WHERE view=?1 AND key=?2")?,
            runs:conn.prepare("INSERT INTO temp.treemap_runs VALUES(?1,?2,?3,?4,?5)")?,
            run:conn.prepare("SELECT first,last,start,end FROM temp.treemap_runs WHERE parent_id=?1")?,
            copy:conn.prepare("SELECT id,data FROM treemap_blocks WHERE id>=?1 AND id<=?2 ORDER BY id")?,
            reuse:conn.prepare("INSERT INTO treemap_reuse(view,parent_id,source_view,sx,sy,tx,ty) VALUES(?1,?2,?3,?4,?5,?6,?7) RETURNING id")?,
            aliases:conn.prepare("INSERT INTO treemap_blocks(id,view,source,start,count,reuse_id) VALUES(?1,?2,?3,?4,?5,?6)")?,
        })
    }
    fn tick(&mut self) -> Result<()> {
        self.ticks += 1;
        if self.ticks % 512 == 0 {
            self.control.check()?;
        }
        Ok(())
    }
    fn parent(&mut self, parent: i64, mut rect: BoxRect, mut remaining: i64) -> Result<()> {
        self.control.check()?;
        if self.after && self.same.query_row([parent], |r| r.get::<_, bool>(0))? {
            let old = if parent == 0 {
                BoxRect {
                    x: 0.0,
                    y: 0.0,
                    w: ATLAS_WIDTH as f64,
                    h: ATLAS_HEIGHT as f64,
                }
            } else {
                self.old_content
                    .query_row(params![self.v - 1, parent], |r| {
                        let header = r.get::<_, f64>(4)? * ATLAS_HEIGHT as f64;
                        let mut old = BoxRect {
                            x: r.get::<_, f64>(0)? * ATLAS_WIDTH as f64,
                            y: r.get::<_, f64>(1)? * ATLAS_HEIGHT as f64,
                            w: r.get::<_, f64>(2)? * ATLAS_WIDTH as f64,
                            h: r.get::<_, f64>(3)? * ATLAS_HEIGHT as f64,
                        };
                        if header > 0.0 {
                            old.x += DIRECTORY_GUTTER;
                            old.y += header;
                            old.w -= 2.0 * DIRECTORY_GUTTER;
                            old.h -= header + DIRECTORY_GUTTER;
                        }
                        Ok(old)
                    })?
            };
            let transform = Transform {
                sx: rect.w / old.w,
                sy: rect.h / old.h,
                tx: (rect.x - old.x * (rect.w / old.w)) / ATLAS_WIDTH as f64,
                ty: (rect.y - old.y * (rect.h / old.h)) / ATLAS_HEIGHT as f64,
            };
            let reuse: i64 = self.reuse.query_row(
                params![
                    self.v,
                    parent,
                    self.v - 1,
                    transform.sx,
                    transform.sy,
                    transform.tx,
                    transform.ty
                ],
                |r| r.get(0),
            )?;
            let (first, last, start, end): (i64, i64, usize, usize) =
                self.run.query_row([parent], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?;
            self.writer.flush()?;
            let mut rows = self.copy.query(params![first, last])?;
            while let Some(row) = rows.next()? {
                self.control.check()?;
                let source: i64 = row.get(0)?;
                let data = row
                    .get_ref(1)?
                    .as_blob()
                    .map_err(|e| ApiError::new("CACHE_INVALID", e.to_string()))?;
                let lo = if source == first { start } else { 0 };
                let hi = if source == last {
                    end
                } else {
                    data.len() / GEOMETRY_BYTES
                };
                if hi == lo {
                    continue;
                }
                self.aliases.execute(params![
                    self.writer.id,
                    self.v,
                    source,
                    lo,
                    hi - lo,
                    reuse
                ])?;
                self.writer.id += 1;
                for record in
                    data[lo * GEOMETRY_BYTES..hi * GEOMETRY_BYTES].chunks_exact(GEOMETRY_BYTES)
                {
                    let g = Geometry::decode(record);
                    if g.leaf {
                        continue;
                    }
                    let r = transform.rect(g.rect);
                    self.insert.execute(params![
                        self.v,
                        g.key,
                        g.node(),
                        0,
                        r.x,
                        r.y,
                        r.w,
                        r.h,
                        g.weight,
                        g.weight
                    ])?;
                }
            }
            return Ok(());
        }
        let first = self.writer.id;
        let slot = self.writer.data.len() / GEOMETRY_BYTES;
        // The scan and finalized-row requery retain only scalar row endpoints.
        let mut scan = self.scan.query([parent])?;
        let mut start: Option<Key> = None;
        let mut end = Key { weight: 0, key: 0 };
        let mut sum = 0i64;
        let mut min = 0i64;
        let mut max = 0i64;
        while let Some(row) = scan.next()? {
            self.ticks += 1;
            if self.ticks % 512 == 0 {
                self.control.check()?;
            }
            let next = Key {
                key: row.get(0)?,
                weight: row.get(1)?,
            };
            let candidate = store::checked_add(sum, next.weight, "treemap row")?;
            if start.is_some()
                && worst(
                    candidate,
                    min.min(next.weight),
                    max.max(next.weight),
                    rect,
                    remaining,
                ) > worst(sum, min, max, rect, remaining)
            {
                place_row(
                    self.control,
                    self.v,
                    &mut self.range,
                    &mut self.equal_range,
                    &mut self.insert,
                    &mut self.writer,
                    parent,
                    start.unwrap(),
                    end,
                    sum,
                    &mut rect,
                    remaining,
                    &mut self.ticks,
                )?;
                remaining -= sum;
                start = None;
                sum = 0;
            }
            if start.is_none() {
                start = Some(next);
                min = next.weight;
                max = next.weight;
            }
            sum = store::checked_add(sum, next.weight, "treemap row")?;
            min = min.min(next.weight);
            max = max.max(next.weight);
            end = next;
        }
        if let Some(start) = start {
            place_row(
                self.control,
                self.v,
                &mut self.range,
                &mut self.equal_range,
                &mut self.insert,
                &mut self.writer,
                parent,
                start,
                end,
                sum,
                &mut rect,
                remaining,
                &mut self.ticks,
            )?;
        }
        if !self.after {
            self.runs.execute(params![
                parent,
                first,
                self.writer.id,
                slot,
                self.writer.data.len() / GEOMETRY_BYTES
            ])?;
        }
        Ok(())
    }
}
fn worst(sum: i64, min: i64, max: i64, r: BoxRect, remaining: i64) -> f64 {
    let scale = r.w * r.h / remaining as f64;
    let short = r.w.min(r.h);
    let area = sum as f64 * scale;
    (short * short * max as f64 * scale / (area * area))
        .max(area * area / (short * short * min as f64 * scale))
}
#[allow(clippy::too_many_arguments)]
fn place_row(
    control: &JobControl,
    v: i64,
    range: &mut Statement<'_>,
    equal_range: &mut Statement<'_>,
    insert: &mut Statement<'_>,
    writer: &mut BlockWriter<'_>,
    parent: i64,
    start: Key,
    end: Key,
    sum: i64,
    r: &mut BoxRect,
    remaining: i64,
    ticks: &mut u64,
) -> Result<()> {
    let vertical = r.w >= r.h;
    let fraction = sum as f64 / remaining as f64;
    let thickness = if vertical {
        r.w * fraction
    } else {
        r.h * fraction
    };
    let length = if vertical { r.h } else { r.w };
    let mut offset = 0.0;
    // Scalar equality exposes the final key range to SQLite when the endpoints
    // have the same weight; a tuple range alone scans the entire equal tier.
    let mut rows = if start.weight == end.weight {
        equal_range.query(params![parent, start.weight, start.key, end.key])?
    } else {
        range.query(params![
            parent,
            start.weight,
            start.key,
            end.weight,
            end.key
        ])?
    };
    while let Some(row) = rows.next()? {
        *ticks += 1;
        if *ticks % 512 == 0 {
            control.check()?;
        }
        let key: i64 = row.get(0)?;
        let node: i64 = row.get(1)?;
        let leaf: i64 = row.get(2)?;
        let weight: i64 = row.get(3)?;
        let value: i64 = row.get(4)?;
        let segment = length * (weight as f64 / sum as f64);
        let b = if vertical {
            BoxRect {
                x: r.x,
                y: r.y + offset,
                w: thickness,
                h: segment,
            }
        } else {
            BoxRect {
                x: r.x + offset,
                y: r.y,
                w: segment,
                h: thickness,
            }
        };
        let x = b.x / ATLAS_WIDTH as f64;
        let y = b.y / ATLAS_HEIGHT as f64;
        let w = b.w / ATLAS_WIDTH as f64;
        let h = b.h / ATLAS_HEIGHT as f64;
        if leaf == 0 {
            insert.execute(params![v, key, node, leaf, x, y, w, h, weight, value])?;
        }
        let mut marks = 0u8;
        for (col, bit) in [(6, 1), (7, 2), (8, 4), (9, 8)] {
            if row.get::<_, i64>(col)? > 0 {
                marks |= bit;
            }
        }
        let color = if leaf == 0 {
            extension_color("")
        } else {
            let c: u32 = row.get(5)?;
            [(c >> 16) as u8, (c >> 8) as u8, c as u8]
        };
        writer.push(Geometry {
            key,
            rect: BoxRect { x, y, w, h },
            weight,
            leaf: leaf != 0,
            header: 0.0,
            color,
            marks,
        })?;
        offset += segment;
    }
    // Exact integer subtraction preserves the residual of near-i64 weights.
    let residual = (remaining - sum) as f64 / remaining as f64;
    if vertical {
        r.x += thickness;
        r.w *= residual;
    } else {
        r.y += thickness;
        r.h *= residual;
    }
    Ok(())
}

fn paint(
    conn: &Connection,
    control: &JobControl,
    v: i64,
    mode: ChartMode,
    pixels: &mut [u8],
) -> Result<()> {
    let mut ticks = 0u64;
    for p in pixels.chunks_exact_mut(4) {
        p.copy_from_slice(&[52, 55, 59, 255]);
    }
    scan_geometry(conn, control, v, |g| {
        if !g.leaf || !g.visible(mode) {
            return Ok(());
        }
        let mut x = g.rect.x * ATLAS_WIDTH as f64;
        let mut y = g.rect.y * ATLAS_HEIGHT as f64;
        let mut w = g.rect.w * ATLAS_WIDTH as f64;
        let mut h = g.rect.h * ATLAS_HEIGHT as f64;
        if g.header > 0.0 {
            x += DIRECTORY_GUTTER;
            y += g.header * ATLAS_HEIGHT as f64;
            w -= DIRECTORY_GUTTER * 2.0;
            h -= g.header * ATLAS_HEIGHT as f64 + DIRECTORY_GUTTER;
        }
        let x0 = x.floor().max(0.0) as u32;
        let y0 = y.floor().max(0.0) as u32;
        let x1 = (x + w).ceil().max(x0 as f64 + 1.0).min(ATLAS_WIDTH as f64) as u32;
        let y1 = (y + h).ceil().max(y0 as f64 + 1.0).min(ATLAS_HEIGHT as f64) as u32;
        for py in y0..y1 {
            let cy = ((y + h).min(py as f64 + 1.0) - y.max(py as f64))
                .max(0.0)
                .min(h);
            for px in x0..x1 {
                ticks += 1;
                if ticks % 512 == 0 {
                    control.check()?;
                }
                let cx = ((x + w).min(px as f64 + 1.0) - x.max(px as f64))
                    .max(0.0)
                    .min(w);
                let coverage = (cx * cy).clamp(0.0, 1.0);
                let u = ((px as f64 + 0.5 - x) / w).clamp(0.0, 1.0);
                let z = ((py as f64 + 0.5 - y) / h).clamp(0.0, 1.0);
                let cushion = 0.74 + 0.26 * (4.0 * u * (1.0 - u) * 4.0 * z * (1.0 - z)).sqrt();
                let edge =
                    if w > 5.0 && h > 5.0 && (px == x0 || py == y0 || px + 1 == x1 || py + 1 == y1)
                    {
                        0.65
                    } else {
                        1.0
                    };
                let i = ((py * ATLAS_WIDTH + px) * 4) as usize;
                for c in 0..3 {
                    pixels[i + c] = (pixels[i + c] as f64 * (1.0 - coverage)
                        + g.color[c] as f64 * cushion * edge * coverage)
                        .round() as u8;
                }
            }
        }
        Ok(())
    })?;
    // Directory boundaries are decoration, never omitted leaf weight.
    let sql = if matches!(mode, ChartMode::Delta) {
        "SELECT r.x,r.y,r.width,r.height FROM treemap_rects r JOIN comparison_nodes c ON c.node_id=r.node_id JOIN treemap_weights w ON w.key=r.key WHERE r.view=?1 AND r.leaf=0 AND c.expandable=1 AND r.width*4096>=8 AND r.height*1024>=8 AND (w.added>0 OR w.removed>0 OR w.type_changed>0 OR w.modified>0) ORDER BY r.id"
    } else {
        "SELECT r.x,r.y,r.width,r.height FROM treemap_rects r JOIN comparison_nodes c ON c.node_id=r.node_id WHERE r.view=?1 AND r.leaf=0 AND c.expandable=1 AND r.width*4096>=8 AND r.height*1024>=8 ORDER BY r.id"
    };
    let mut query = conn.prepare(sql)?;
    let mut rows = query.query([v])?;
    while let Some(row) = rows.next()? {
        control.check()?;
        let r = BoxRect {
            x: row.get(0)?,
            y: row.get(1)?,
            w: row.get(2)?,
            h: row.get(3)?,
        };
        let (x, y, ex, ey) = pixel_bounds(r);
        if ex > x && ey > y {
            for px in x..ex {
                darken(pixels, px, y);
                darken(pixels, px, ey - 1);
            }
            for py in y..ey {
                darken(pixels, x, py);
                darken(pixels, ex - 1, py);
            }
        }
    }
    if matches!(mode, ChartMode::Delta) {
        let baseline = if v % 6 < 3 { "sb" } else { "ab" };
        let mut query=conn.prepare(&format!("SELECT r.x,r.y,r.width,r.height,EXISTS(SELECT 1 FROM treemap_weights child WHERE child.parent_id=w.key AND child.added>0 AND child.{baseline}=0),0,CASE WHEN c.status='typeChanged' THEN w.type_changed ELSE 0 END,0 FROM treemap_rects r JOIN treemap_weights w ON w.key=r.key JOIN comparison_nodes c ON c.node_id=r.node_id WHERE r.view=?1 AND r.leaf=0 AND (w.added>0 OR c.status='typeChanged') ORDER BY r.id"))?;
        let mut rows = query.query([v])?;
        // Containers first, then every leaf in original deterministic paint order.
        while let Some(row) = rows.next()? {
            control.check()?;
            let mut marks = 0;
            for (col, bit) in [(4, 1), (5, 2), (6, 4), (7, 8)] {
                if row.get::<_, i64>(col)? > 0 {
                    marks |= bit;
                }
            }
            mark(
                pixels,
                BoxRect {
                    x: row.get(0)?,
                    y: row.get(1)?,
                    w: row.get(2)?,
                    h: row.get(3)?,
                },
                marks,
            );
        }
        scan_geometry(conn, control, v, |g| {
            if g.leaf {
                mark(pixels, g.rect, g.marks);
            }
            Ok(())
        })?;
    }
    Ok(())
}
fn pixel_bounds(r: BoxRect) -> (u32, u32, u32, u32) {
    (
        (r.x * ATLAS_WIDTH as f64).floor().max(0.0) as u32,
        (r.y * ATLAS_HEIGHT as f64).floor().max(0.0) as u32,
        ((r.x + r.w) * ATLAS_WIDTH as f64)
            .ceil()
            .min(ATLAS_WIDTH as f64) as u32,
        ((r.y + r.h) * ATLAS_HEIGHT as f64)
            .ceil()
            .min(ATLAS_HEIGHT as f64) as u32,
    )
}
fn mark(pixels: &mut [u8], r: BoxRect, marks: u8) {
    let (x, y, ex, ey) = pixel_bounds(r);
    let mut inset = 0;
    for (bit, color, dashed) in [
        (4, [148, 114, 204], false),
        (8, [225, 183, 80], false),
        (2, [218, 83, 97], true),
        (1, [35, 148, 107], true),
    ] {
        if marks & bit != 0 {
            border(pixels, x, y, ex, ey, inset, color, dashed);
            inset += 8;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn border(
    pixels: &mut [u8],
    x: u32,
    y: u32,
    ex: u32,
    ey: u32,
    inset: u32,
    color: [u8; 3],
    dashed: bool,
) {
    if ex <= x + inset * 2 + 7 || ey <= y + inset * 2 + 7 {
        return;
    }
    let x = x + inset;
    let y = y + inset;
    let ex = ex - inset;
    let ey = ey - inset;
    // Six atlas pixels survive the normal ~4x downscale. Small tiles
    // retain most extension fill; no global status-colour area replaces the baseline.
    let thickness = 6.min(((ex - x).min(ey - y) / 6).max(1));
    for layer in 0..thickness {
        for px in x..ex {
            if !dashed || (px - x) / 16 % 2 == 0 {
                for py in [y + layer, ey - 1 - layer] {
                    let i = ((py * ATLAS_WIDTH + px) * 4) as usize;
                    pixels[i..i + 3].copy_from_slice(&color);
                }
            }
        }
        for py in y + thickness..ey - thickness {
            if !dashed || (py - y) / 16 % 2 == 0 {
                for px in [x + layer, ex - 1 - layer] {
                    let i = ((py * ATLAS_WIDTH + px) * 4) as usize;
                    pixels[i..i + 3].copy_from_slice(&color);
                }
            }
        }
    }
}
fn darken(pixels: &mut [u8], x: u32, y: u32) {
    if x < ATLAS_WIDTH && y < ATLAS_HEIGHT {
        let i = ((y * ATLAS_WIDTH + x) * 4) as usize;
        for c in 0..3 {
            pixels[i + c] = (pixels[i + c] as u16 * 3 / 4) as u8;
        }
    }
}

fn json_error(e: serde_json::Error) -> ApiError {
    ApiError::new("CACHE_ERROR", e.to_string())
}
fn frame_labels(
    conn: &Connection,
    control: &JobControl,
    v: i64,
    mode: ChartMode,
) -> Result<(Vec<TreemapLabel>, i64)> {
    let side = if matches!(mode, ChartMode::After) {
        1
    } else {
        0
    };
    let mut directories = Vec::<Geometry>::with_capacity(257);
    let mut files = Vec::<Geometry>::with_capacity(65);
    let mut rendered = 0;
    let mut kind = conn.prepare("SELECT kind FROM entries WHERE node_id=?1 AND side=?2")?;
    scan_geometry(conn, control, v, |g| {
        if !g.visible(mode) {
            return Ok(());
        }
        if g.leaf {
            rendered += 1;
        }
        let expected = if g.header > 0.0 { "directory" } else { "file" };
        let eligible =
            g.header > 0.0 || (g.leaf && g.rect.w * 4096.0 >= 96.0 && g.rect.h * 1024.0 >= 22.0);
        if eligible
            && kind
                .query_row(params![g.node(), side], |r| {
                    Ok(store::root_text(r, 0)? == expected)
                })
                .optional()?
                .unwrap_or(false)
        {
            let (list, limit) = if expected == "directory" {
                (&mut directories, 256)
            } else {
                (&mut files, 64)
            };
            list.push(g);
            list.sort_unstable_by(|a, b| {
                (b.rect.w * b.rect.h)
                    .total_cmp(&(a.rect.w * a.rect.h))
                    .then_with(|| a.node().cmp(&b.node()))
            });
            list.truncate(limit);
        }
        Ok(())
    })?;
    let mut labels = Vec::with_capacity(directories.len() + files.len());
    let mut meta=conn.prepare("SELECT n.name,e.path FROM nodes n JOIN entries e ON e.node_id=n.id AND e.side=?2 WHERE n.id=?1")?;
    for (kind, list) in [(NodeKind::Directory, directories), (NodeKind::File, files)] {
        for g in list {
            let (name, path) = meta.query_row(params![g.node(), side], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            labels.push(TreemapLabel {
                node_id: format!("n{}", g.node()),
                name,
                path,
                kind: kind.clone(),
                weight: g.weight.to_string(),
                x: g.rect.x,
                y: g.rect.y,
                width: g.rect.w,
                height: if matches!(kind, NodeKind::Directory) {
                    g.header
                } else {
                    g.rect.h
                },
            });
        }
    }
    Ok((labels, rendered))
}
fn frame_warnings(
    conn: &Connection,
    metric: Metric,
    mode: ChartMode,
    added: i64,
) -> Result<Vec<String>> {
    let mut warnings=vec!["全局实际文件汇总；目录标题和边距是结构装饰，不另计占用。零权重文件计入数量但不占面积。有限深度下，截断目录包含全部后代文件，选择隐藏文件会定位到可见祖先。".into()];
    if matches!(mode, ChartMode::Delta) {
        let col = COLUMNS[view(metric, ChartMode::Before)];
        let unanchored: i64 = conn.query_row(
            &format!(
                "SELECT coalesce(sum(added),0) FROM treemap_weights WHERE parent_id=0 AND {col}=0"
            ),
            [],
            |r| r.get(0),
        )?;
        if added > 0 {
            warnings.push(format!(
                "新增文件 {added} 个：使用最近可见之前祖先的虚线框提示，不分配新增面积。"
            ));
        }
        if unanchored > 0 {
            warnings.push(format!("其中 {unanchored} 个新增文件没有之前占用的祖先，无法在基线图定位；请查看之后模式/目录树。以下最多列出 16 个路径："));
            let mut query=conn.prepare(&format!("WITH RECURSIVE missing(key) AS (SELECT key FROM treemap_weights WHERE parent_id=0 AND {col}=0 AND added>0 UNION ALL SELECT w.key FROM treemap_weights w JOIN missing m ON w.parent_id=m.key WHERE w.added>0) SELECT e.path FROM missing m JOIN treemap_weights w ON w.key=m.key JOIN entries e ON e.node_id=w.node_id AND e.side=1 WHERE w.leaf=1 ORDER BY w.node_id LIMIT 16"))?;
            let mut paths = query.query([])?;
            while let Some(row) = paths.next()? {
                warnings.push(row.get(0)?);
            }
        }
    }
    Ok(warnings)
}
pub fn get_frame(
    conn: &Connection,
    metric: Metric,
    mode: ChartMode,
    max_depth: u32,
) -> Result<FullTreemapData> {
    ensure_frame(conn, metric, mode, max_depth)?;
    let v = cache_view(metric, mode, max_depth);
    let mut query=conn.prepare("SELECT png,file_count,visible_file_count,weight_total,positive_total,negative_total,net_delta,exported_total,rendered_block_count,added_file_count,labels,warnings FROM treemap_frames WHERE view=?1")?;
    let mut rows = query.query([v])?;
    let row = rows.next()?.ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let png = row
        .get_ref(0)?
        .as_blob()
        .map_err(|e| ApiError::new("CACHE_INVALID", e.to_string()))?;
    let mut image_data_url = String::with_capacity(22 + png.len().div_ceil(3) * 4);
    image_data_url.push_str("data:image/png;base64,");
    base64::engine::general_purpose::STANDARD.encode_string(png, &mut image_data_url);
    Ok(FullTreemapData {
        image_data_url,
        atlas_width: ATLAS_WIDTH,
        atlas_height: ATLAS_HEIGHT,
        file_count: row.get::<_, i64>(1)? as u64,
        visible_file_count: row.get::<_, i64>(2)? as u64,
        rendered_block_count: row.get::<_, i64>(8)? as u64,
        max_depth,
        added_file_count: row.get::<_, i64>(9)? as u64,
        weight_total: row.get::<_, i64>(3)?.to_string(),
        positive_total: row.get::<_, i64>(4)?.to_string(),
        negative_total: row.get::<_, i64>(5)?.to_string(),
        net_delta: row.get::<_, i64>(6)?.to_string(),
        exported_total: row.get::<_, i64>(7)?.to_string(),
        labels: serde_json::from_str(store::root_text(row, 10)?).map_err(json_error)?,
        warnings: serde_json::from_str(store::root_text(row, 11)?).map_err(json_error)?,
    })
}

pub fn get_bounds(
    conn: &Connection,
    metric: Metric,
    mode: ChartMode,
    node_id: &str,
    max_depth: u32,
) -> Result<Option<TreemapRect>> {
    let n = crate::diff::node_number(node_id)?;
    if !conn.query_row("SELECT EXISTS(SELECT 1 FROM nodes WHERE id=?1)", [n], |r| {
        r.get::<_, bool>(0)
    })? {
        return Err(ApiError::new("INVALID_NODE", "节点不存在或已过期"));
    }
    ensure_frame(conn, metric, mode, max_depth)?;
    if matches!(mode,ChartMode::Delta) && !conn.query_row("SELECT added>0 OR removed>0 OR type_changed>0 OR modified>0 FROM treemap_weights WHERE key=?1",[n],|r|r.get::<_,bool>(0))? {return Ok(None);}
    let v = geometry_view(conn, cache_view(metric, mode, max_depth))?;
    let mut ancestors=conn.prepare("WITH RECURSIVE ancestors(id,parent_id,d) AS (SELECT id,parent_id,0 FROM nodes WHERE id=?1 UNION ALL SELECT n.id,n.parent_id,a.d+1 FROM nodes n JOIN ancestors a ON n.id=a.parent_id) SELECT id FROM ancestors ORDER BY d")?;
    let mut rows = ancestors.query([n])?;
    while let Some(row) = rows.next()? {
        if let Some(g) = cached_geometry(conn, v, row.get(0)?)? {
            if g.visible(mode) {
                return Ok(Some(g.bounds()));
            }
        }
    }
    Ok(None)
}

pub fn hit_test(
    conn: &Connection,
    metric: Metric,
    mode: ChartMode,
    x: f64,
    y: f64,
    max_depth: u32,
) -> Result<Option<TreemapHit>> {
    if !x.is_finite() || !y.is_finite() || !(0.0..=1.0).contains(&x) || !(0.0..=1.0).contains(&y) {
        return Err(ApiError::new(
            "INVALID_POSITION",
            "坐标必须是有限的归一化坐标",
        ));
    }
    ensure_frame(conn, metric, mode, max_depth)?;
    let side = if matches!(mode, ChartMode::After) {
        1
    } else {
        0
    };
    let v = geometry_view(conn, cache_view(metric, mode, max_depth))?;
    let mut blocks=conn.prepare("SELECT b.id,b.data,b.source,b.start,b.count,r.sx,r.sy,r.tx,r.ty FROM treemap_spatial s CROSS JOIN treemap_blocks b ON b.id=s.id LEFT JOIN treemap_reuse r ON r.id=b.reuse_id WHERE s.x0<=?2 AND s.x1>=?2 AND s.y0<=?3 AND s.y1>=?3 AND b.view=?1")?;
    let mut directory =
        conn.prepare("SELECT leaf,header_height FROM treemap_rects WHERE view=?1 AND key=?2")?;
    let mut rows = blocks.query(params![v, x, y])?;
    let mut selected: Option<Geometry> = None;
    read_blocks(conn, &mut rows, |_, data, transform| {
        for record in data.chunks_exact(GEOMETRY_BYTES) {
            let mut g = Geometry::decode(record);
            g.rect = transform.rect(g.rect);
            decorate(&mut g, &mut directory, v)?;
            if !g.visible(mode) {
                continue;
            }
            let r = g.rect;
            let height = if g.leaf { r.h } else { g.header };
            if (g.leaf || g.header > 0.0)
                && r.x <= x
                && r.y <= y
                && (x < r.x + r.w || (x == 1.0 && r.x + r.w >= 1.0))
                && (y < r.y + height || (y == 1.0 && r.y + height >= 1.0))
                && selected.map_or(true, |old| g.node() < old.node())
            {
                selected = Some(g);
            }
        }
        Ok(())
    })?;
    let Some(g) = selected else { return Ok(None) };
    let value = if matches!(mode, ChartMode::Delta) {
        if matches!(metric, Metric::Size) {
            "w.size_value+0*?4"
        } else {
            "w.allocated_value+0*?4"
        }
    } else {
        "?4"
    };
    let mut query=conn.prepare(&format!("SELECT n.name,coalesce(ep.path||e.suffix,n.path),coalesce(e.extension,''),{value},c.status,coalesce(es.kind=1,0),w.leaf,w.added,w.removed,w.type_changed,w.modified FROM nodes n JOIN comparison_nodes c ON c.node_id=n.id JOIN treemap_weights w ON w.key=?1 LEFT JOIN snapshot_entries es ON es.node_id=n.id AND es.side=?3 LEFT JOIN entry_values e ON e.id=es.value_id LEFT JOIN path_prefixes ep ON ep.id=e.prefix_id WHERE n.id=?2"))?;
    Ok(Some(query.query_row(
        params![g.key, g.node(), side, g.weight],
        |r| {
            let is_directory = r.get::<_, bool>(5)?;
            let status = if is_directory {
                if r.get::<_, i64>(9)? > 0 {
                    Status::TypeChanged
                } else if r.get::<_, i64>(10)? > 0 {
                    Status::Modified
                } else if r.get::<_, i64>(8)? > 0 {
                    Status::Removed
                } else if r.get::<_, i64>(7)? > 0 {
                    Status::Added
                } else {
                    Status::Unchanged
                }
            } else {
                match store::root_text(r, 4)? {
                    "added" => Status::Added,
                    "removed" => Status::Removed,
                    "modified" => Status::Modified,
                    "typeChanged" => Status::TypeChanged,
                    _ => Status::Unchanged,
                }
            };
            Ok(TreemapHit {
                node_id: format!("n{}", g.node()),
                name: r.get(0)?,
                path: r.get(1)?,
                extension: r.get(2)?,
                weight: g.weight.to_string(),
                value: r.get::<_, i64>(3)?.to_string(),
                status,
                kind: if is_directory {
                    NodeKind::Directory
                } else {
                    NodeKind::File
                },
                collapsed: r.get::<_, i64>(6)? == 0 && g.leaf,
                rect: g.bounds(),
            })
        },
    )?))
}
