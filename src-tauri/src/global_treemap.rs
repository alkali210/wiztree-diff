//! Global leaf-weighted treemaps with persisted, depth-limited geometry. SQLite
//! owns hierarchy, aggregates, and layout; Rust retains no sibling/path/tree lists.
use crate::{import::JobControl, store, types::*};
use base64::Engine;
use rusqlite::{params, Connection, OptionalExtension, Statement};

pub const ATLAS_WIDTH: u32 = 4096;
pub const ATLAS_HEIGHT: u32 = 1024;
const SPATIAL_BLOCK: i64 = 256;
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
    // Ordinary files are their own leaves. Directory/file type changes and
    // sparse exports with children under an opposite-side file retain both
    // a container and an own-file leaf. Positive sibling keys stay unchanged.
    conn.execute_batch("CREATE INDEX IF NOT EXISTS nodes_parent ON nodes(parent_id);
        CREATE TABLE treemap_weights(
        key INTEGER PRIMARY KEY,node_id INTEGER NOT NULL,parent_id INTEGER NOT NULL,depth INTEGER NOT NULL,leaf INTEGER NOT NULL,
        sb INTEGER NOT NULL CHECK(typeof(sb)='integer'),sa INTEGER NOT NULL CHECK(typeof(sa)='integer'),
        ab INTEGER NOT NULL CHECK(typeof(ab)='integer'),aa INTEGER NOT NULL CHECK(typeof(aa)='integer'),
        size_value INTEGER NOT NULL CHECK(typeof(size_value)='integer'),allocated_value INTEGER NOT NULL CHECK(typeof(allocated_value)='integer'),
        before_file INTEGER NOT NULL,after_file INTEGER NOT NULL,
        added INTEGER NOT NULL,removed INTEGER NOT NULL,type_changed INTEGER NOT NULL,modified INTEGER NOT NULL);
        WITH RECURSIVE parents(id) AS MATERIALIZED (SELECT DISTINCT parent_id FROM nodes WHERE parent_id IS NOT NULL),
        levels(id,level) AS (
            SELECT n.id,1 FROM nodes n JOIN parents p ON p.id=n.id WHERE n.parent_id IS NULL
            UNION ALL SELECT n.id,l.level+1 FROM levels l JOIN nodes n ON n.parent_id=l.id JOIN parents p ON p.id=n.id)
        , own AS (SELECT n.id,coalesce(n.parent_id,0) parent_id,coalesce(l.level+1,1) level,
            CASE WHEN c.before_kind='directory' OR c.after_kind='directory' OR child.id IS NOT NULL THEN 0 ELSE 1 END leaf,
            CASE WHEN c.before_kind='file' THEN c.before_size ELSE 0 END sb,CASE WHEN c.after_kind='file' THEN c.after_size ELSE 0 END sa,
            CASE WHEN c.before_kind='file' THEN c.before_allocated ELSE 0 END ab,CASE WHEN c.after_kind='file' THEN c.after_allocated ELSE 0 END aa,
            coalesce(c.before_kind='file',0) bf,coalesce(c.after_kind='file',0) af,
            coalesce(c.after_kind='file' AND (c.before_kind IS NULL OR c.before_kind!='file'),0) added,
            coalesce(c.status='removed',0) removed,coalesce(c.status='typeChanged',0) changed,coalesce(c.status='modified',0) modified
            FROM nodes n NOT INDEXED CROSS JOIN comparison_nodes c ON c.node_id=n.id
            LEFT JOIN levels l ON l.id=n.parent_id LEFT JOIN parents child ON child.id=n.id)
        INSERT INTO treemap_weights SELECT id,id,parent_id,level,leaf,
            sb*leaf,sa*leaf,ab*leaf,aa*leaf,(sa-sb)*leaf,(aa-ab)*leaf,
            bf*leaf,af*leaf,added*leaf,removed*leaf,changed*leaf,modified*leaf FROM own;
        CREATE INDEX treemap_depth ON treemap_weights(depth,key) WHERE leaf=0;
        INSERT INTO treemap_weights
        SELECT -c.node_id,c.node_id,c.node_id,w.depth+1,1,
        CASE WHEN c.before_kind='file' THEN c.before_size ELSE 0 END,CASE WHEN c.after_kind='file' THEN c.after_size ELSE 0 END,
        CASE WHEN c.before_kind='file' THEN c.before_allocated ELSE 0 END,CASE WHEN c.after_kind='file' THEN c.after_allocated ELSE 0 END,
        (CASE WHEN c.after_kind='file' THEN c.after_size ELSE 0 END)-(CASE WHEN c.before_kind='file' THEN c.before_size ELSE 0 END),
        (CASE WHEN c.after_kind='file' THEN c.after_allocated ELSE 0 END)-(CASE WHEN c.before_kind='file' THEN c.before_allocated ELSE 0 END),
        coalesce(c.before_kind='file',0),coalesce(c.after_kind='file',0),
        coalesce(c.after_kind='file' AND (c.before_kind IS NULL OR c.before_kind!='file'),0),coalesce(c.status='removed',0),
        coalesce(c.status='typeChanged',0),coalesce(c.status='modified',0)
        FROM treemap_weights w INDEXED BY treemap_depth CROSS JOIN comparison_nodes c ON c.node_id=w.node_id
        WHERE w.leaf=0 AND (c.before_kind='file' OR c.after_kind='file');
        CREATE INDEX treemap_parent ON treemap_weights(parent_id);
        CREATE TABLE treemap_rects(id INTEGER PRIMARY KEY,view INTEGER NOT NULL,key INTEGER NOT NULL,node_id INTEGER NOT NULL,leaf INTEGER NOT NULL,
            x REAL NOT NULL,y REAL NOT NULL,width REAL NOT NULL,height REAL NOT NULL,weight INTEGER NOT NULL,value INTEGER NOT NULL,
            header_height REAL NOT NULL DEFAULT 0,
            UNIQUE(view,key));
        CREATE INDEX treemap_leaf_scan ON treemap_rects(view,id) WHERE leaf=1;
        CREATE INDEX treemap_label_page ON treemap_rects(view,(width*height) DESC,node_id) WHERE leaf=1 AND width*4096>=96 AND height*1024>=22;
        CREATE INDEX treemap_directory_labels ON treemap_rects(view,(width*height) DESC,node_id) WHERE header_height>0;
        CREATE VIRTUAL TABLE treemap_spatial USING rtree(id,x0,x1,y0,y1);
        CREATE TABLE treemap_frames(view INTEGER PRIMARY KEY,png BLOB NOT NULL,file_count INTEGER NOT NULL,visible_file_count INTEGER NOT NULL,
            weight_total INTEGER NOT NULL,positive_total INTEGER NOT NULL,negative_total INTEGER NOT NULL,net_delta INTEGER NOT NULL,exported_total INTEGER NOT NULL,rendered_block_count INTEGER NOT NULL,added_file_count INTEGER NOT NULL);").map_err(store::aggregate_error)?;
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
    // The initial unlimited After map is complete before import is published.
    // Other metrics/modes/depths are cached only when actually requested.
    let mut pixels = vec![0u8; ATLAS_WIDTH as usize * ATLAS_HEIGHT as usize * 4];
    generate(
        conn,
        control,
        Metric::Size,
        ChartMode::After,
        0,
        &mut pixels,
    )?;
    transaction.commit()?;
    Ok(())
}

fn cache_view(metric: Metric, mode: ChartMode, max_depth: u32) -> i64 {
    if max_depth == 3 {
        view(metric, mode) as i64
    } else {
        (max_depth as i64 + 1) * 6 + view(metric, mode) as i64
    }
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
    conn.execute_batch("PRAGMA cache_size=-32768; PRAGMA temp_store=FILE; PRAGMA temp.cache_size=-32768; PRAGMA mmap_size=0;")?;
    let transaction = conn.unchecked_transaction()?;
    let control = JobControl::default();
    let mut pixels = vec![0u8; ATLAS_WIDTH as usize * ATLAS_HEIGHT as usize * 4];
    generate(conn, &control, metric, mode, max_depth, &mut pixels)?;
    transaction.commit()?;
    Ok(())
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
    if matches!(mode, ChartMode::Delta) {
        generate(conn, control, metric, ChartMode::Before, max_depth, pixels)?;
        let value = match metric {
            Metric::Size => "size_value",
            Metric::Allocated => "allocated_value",
        };
        // Preserve paint order too: subpixel coverage blending is order-sensitive.
        conn.execute(&format!("INSERT INTO treemap_rects(view,key,node_id,leaf,x,y,width,height,weight,value,header_height) SELECT ?1,r.key,r.node_id,r.leaf,r.x,r.y,r.width,r.height,r.weight,w.{value},r.header_height FROM treemap_rects r JOIN treemap_weights w ON w.key=r.key WHERE r.view=?2 ORDER BY r.id"), params![v,cache_view(metric, ChartMode::Before,max_depth)])?;
    } else if totals.weight > 0 {
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
                // Only readable directory blocks reserve decoration. Tiny/deep
                // containers still traverse fully and retain every positive file.
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
    }
    // Index bounded geometry blocks rather than every subpixel file separately.
    // Rebuild the shared last block when another view appends to it; hit tests
    // still check each candidate rectangle and its half-open title/file bounds.
    conn.execute(&format!("INSERT OR REPLACE INTO treemap_spatial
        SELECT id/{SPATIAL_BLOCK},min(x),max(x+width),min(y),max(y+CASE WHEN leaf=1 THEN height ELSE header_height END)
        FROM treemap_rects WHERE id>=(SELECT min(id)/{SPATIAL_BLOCK}*{SPATIAL_BLOCK} FROM treemap_rects WHERE view=?1)
        AND id<=(SELECT max(id) FROM treemap_rects WHERE view=?1) AND (leaf=1 OR header_height>0) GROUP BY id/{SPATIAL_BLOCK}"), [v])?;
    paint(conn, control, v, mode, pixels)?;
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
    control.check()?;
    let rendered: i64 = conn.query_row(
        "SELECT count(*) FROM treemap_rects WHERE view=?1 AND leaf=1",
        [v],
        |r| r.get(0),
    )?;
    conn.execute(
        "INSERT INTO treemap_frames VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
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
            totals.added
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
    let sql = format!("SELECT coalesce({files},0),coalesce(sum({w}>0),0),coalesce(sum({w}),0),coalesce(sum(CASE WHEN {value}>0 THEN {value} ELSE 0 END),0),coalesce(sum(CASE WHEN {value}<0 THEN -{value} ELSE 0 END),0),coalesce(sum({value}),0),coalesce(sum(added),0) FROM treemap_weights WHERE leaf=1", w=COLUMNS[v]);
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
struct Layout<'a> {
    control: &'a JobControl,
    v: i64,
    scan: Statement<'a>,
    range: Statement<'a>,
    insert: Statement<'a>,
    ticks: u64,
}
impl<'a> Layout<'a> {
    fn new(
        conn: &'a Connection,
        control: &'a JobControl,
        v: i64,
        order: usize,
        col: &str,
    ) -> Result<Self> {
        let base = format!(
            "FROM treemap_weights INDEXED BY treemap_order_{order} WHERE parent_id=?1 AND {col}>0"
        );
        let value = col;
        conn.execute_batch(&format!(
            "CREATE INDEX IF NOT EXISTS treemap_order_{order} ON treemap_weights(parent_id,{col} DESC,key DESC) WHERE {col}>0"
        ))?;
        Ok(Self {control,v,
            scan:conn.prepare(&format!("SELECT key,{col} {base} ORDER BY {col} DESC,key DESC"))?,
            range:conn.prepare(&format!("SELECT key,node_id,leaf,{col},{value} {base} AND ({col},key)<=(?2,?3) AND ({col},key)>=(?4,?5) ORDER BY {col} DESC,key DESC"))?,
            insert:conn.prepare("INSERT INTO treemap_rects(view,key,node_id,leaf,x,y,width,height,weight,value) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)")?,
            ticks:0 })
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
                    &mut self.insert,
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
                &mut self.insert,
                parent,
                start,
                end,
                sum,
                &mut rect,
                remaining,
                &mut self.ticks,
            )?;
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
    insert: &mut Statement<'_>,
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
    let mut rows = range.query(params![
        parent,
        start.weight,
        start.key,
        end.weight,
        end.key
    ])?;
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
        insert.execute(params![v, key, node, leaf, x, y, w, h, weight, value])?;
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
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.copy_from_slice(&[52, 55, 59, 255]);
    }
    let side = if matches!(mode, ChartMode::After) {
        1
    } else {
        0
    };
    let mut query=conn.prepare("SELECT r.x,r.y,r.width,r.height,CASE WHEN r.key<0 OR (CASE WHEN ?2=0 THEN c.before_kind ELSE c.after_kind END)='file' THEN c.extension ELSE '' END,r.header_height FROM treemap_rects r INDEXED BY treemap_leaf_scan JOIN comparison_nodes c ON c.node_id=r.node_id WHERE r.view=?1 AND r.leaf=1 ORDER BY r.id")?;
    let mut rows = query.query(params![v, side])?;
    let mut ticks = 0u64;
    while let Some(row) = rows.next()? {
        ticks += 1;
        if ticks % 512 == 0 {
            control.check()?;
        }
        let mut x = row.get::<_, f64>(0)? * ATLAS_WIDTH as f64;
        let mut y = row.get::<_, f64>(1)? * ATLAS_HEIGHT as f64;
        let mut w = row.get::<_, f64>(2)? * ATLAS_WIDTH as f64;
        let mut h = row.get::<_, f64>(3)? * ATLAS_HEIGHT as f64;
        let header = row.get::<_, f64>(5)? * ATLAS_HEIGHT as f64;
        if header > 0.0 {
            x += DIRECTORY_GUTTER;
            y += header;
            w -= DIRECTORY_GUTTER * 2.0;
            h -= header + DIRECTORY_GUTTER;
        }
        let color = extension_color(store::root_text(row, 4)?);
        let x0 = x.floor().max(0.0) as u32;
        let y0 = y.floor().max(0.0) as u32;
        let x1 = (x + w).ceil().max(x0 as f64 + 1.0).min(ATLAS_WIDTH as f64) as u32;
        let y1 = (y + h).ceil().max(y0 as f64 + 1.0).min(ATLAS_HEIGHT as f64) as u32;
        for py in y0..y1 {
            ticks += 1;
            if ticks % 512 == 0 {
                control.check()?;
            }
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
                        + color[c] as f64 * cushion * edge * coverage)
                        .round() as u8;
                }
            }
        }
    }
    // Subtle directory boundaries are an overlay, never padding that discards files.
    let mut query=conn.prepare("SELECT r.x,r.y,r.width,r.height FROM treemap_rects r JOIN comparison_nodes c ON c.node_id=r.node_id WHERE r.view=?1 AND r.leaf=0 AND c.expandable=1 AND r.width*4096>=8 AND r.height*1024>=8 ORDER BY r.id")?;
    let mut rows = query.query([v as i64])?;
    while let Some(row) = rows.next()? {
        ticks += 1;
        if ticks % 512 == 0 {
            control.check()?;
        }
        let x = (row.get::<_, f64>(0)? * ATLAS_WIDTH as f64)
            .floor()
            .max(0.0) as u32;
        let y = (row.get::<_, f64>(1)? * ATLAS_HEIGHT as f64)
            .floor()
            .max(0.0) as u32;
        let ex = ((row.get::<_, f64>(0)? + row.get::<_, f64>(2)?) * ATLAS_WIDTH as f64)
            .ceil()
            .min(ATLAS_WIDTH as f64) as u32;
        let ey = ((row.get::<_, f64>(1)? + row.get::<_, f64>(3)?) * ATLAS_HEIGHT as f64)
            .ceil()
            .min(ATLAS_HEIGHT as f64) as u32;
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
        let mut query = conn.prepare(&format!("SELECT r.x,r.y,r.width,r.height,CASE WHEN r.leaf=1 THEN w.added ELSE EXISTS(SELECT 1 FROM treemap_weights child WHERE child.parent_id=w.key AND child.added>0 AND child.{baseline}=0) END,CASE WHEN r.leaf=1 THEN w.removed ELSE 0 END,CASE WHEN r.leaf=1 OR c.status='typeChanged' THEN w.type_changed ELSE 0 END,CASE WHEN r.leaf=1 THEN w.modified ELSE 0 END FROM treemap_rects r JOIN treemap_weights w ON w.key=r.key JOIN comparison_nodes c ON c.node_id=r.node_id WHERE r.view=?1 AND (w.added>0 OR c.status='typeChanged' OR (r.leaf=1 AND (w.removed>0 OR w.type_changed>0 OR w.modified>0))) ORDER BY r.leaf,r.id"))?;
        let mut rows = query.query([v])?;
        while let Some(row) = rows.next()? {
            control.check()?;
            let x = (row.get::<_, f64>(0)? * ATLAS_WIDTH as f64)
                .floor()
                .max(0.0) as u32;
            let y = (row.get::<_, f64>(1)? * ATLAS_HEIGHT as f64)
                .floor()
                .max(0.0) as u32;
            let ex = ((row.get::<_, f64>(0)? + row.get::<_, f64>(2)?) * ATLAS_WIDTH as f64)
                .ceil()
                .min(ATLAS_WIDTH as f64) as u32;
            let ey = ((row.get::<_, f64>(1)? + row.get::<_, f64>(3)?) * ATLAS_HEIGHT as f64)
                .ceil()
                .min(ATLAS_HEIGHT as f64) as u32;
            let mut inset = 0;
            // Multiple descendant change kinds remain visible even when net growth cancels loss.
            for (column, color, dashed) in [
                (6, [148, 114, 204], false),
                (7, [225, 183, 80], false),
                (5, [218, 83, 97], true),
                (4, [35, 148, 107], true),
            ] {
                if row.get::<_, i64>(column)? > 0 {
                    border(pixels, x, y, ex, ey, inset, color, dashed);
                    inset += 8;
                }
            }
        }
    }
    Ok(())
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

pub fn get_frame(
    conn: &Connection,
    metric: Metric,
    mode: ChartMode,
    max_depth: u32,
) -> Result<FullTreemapData> {
    ensure_frame(conn, metric, mode, max_depth)?;
    let v = cache_view(metric, mode, max_depth);
    let (png,files,visible,weight,positive,negative,net,exported,rendered,added):(Vec<u8>,i64,i64,i64,i64,i64,i64,i64,i64,i64)=conn.query_row("SELECT png,file_count,visible_file_count,weight_total,positive_total,negative_total,net_delta,exported_total,rendered_block_count,added_file_count FROM treemap_frames WHERE view=?1",[v],|r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?)))?;
    let mut labels = Vec::with_capacity(320);
    let side = if matches!(mode, ChartMode::After) {
        1
    } else {
        0
    };
    // Directory title bars and file captions have independent bounded budgets.
    for (kind, sql) in [
        (NodeKind::Directory, "SELECT r.node_id,n.name,e.path,r.x,r.y,r.width,r.header_height,r.weight FROM treemap_rects r JOIN nodes n ON n.id=r.node_id JOIN entries e ON e.node_id=n.id AND e.side=?2 AND e.kind='directory' WHERE r.view=?1 AND r.header_height>0 ORDER BY r.width*r.height DESC,r.node_id LIMIT 256"),
        (NodeKind::File, "SELECT r.node_id,n.name,e.path,r.x,r.y,r.width,r.height,r.weight FROM treemap_rects r JOIN nodes n ON n.id=r.node_id JOIN entries e ON e.node_id=n.id AND e.side=?2 AND e.kind='file' WHERE r.view=?1 AND r.leaf=1 AND r.width*4096>=96 AND r.height*1024>=22 ORDER BY r.width*r.height DESC,r.node_id LIMIT 64"),
    ] {
        let mut query = conn.prepare(sql)?;
        let mut rows = query.query(params![v,side])?;
        while let Some(r) = rows.next()? {
            labels.push(TreemapLabel {
                node_id: format!("n{}", r.get::<_,i64>(0)?),
                name: r.get(1)?, path: r.get(2)?, kind: kind.clone(),
                weight: r.get::<_,i64>(7)?.to_string(),
                x: r.get(3)?, y: r.get(4)?, width: r.get(5)?, height: r.get(6)?,
            });
        }
    }
    let mut warnings = vec!["全局实际文件汇总；目录标题和边距是结构装饰，不另计占用。零权重文件计入数量但不占面积。有限深度下，截断目录包含全部后代文件，选择隐藏文件会定位到可见祖先。".into()];
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
            let mut query = conn.prepare(&format!("WITH RECURSIVE missing(key) AS (SELECT key FROM treemap_weights WHERE parent_id=0 AND {col}=0 AND added>0 UNION ALL SELECT w.key FROM treemap_weights w JOIN missing m ON w.parent_id=m.key WHERE w.added>0) SELECT e.path FROM missing m JOIN treemap_weights w ON w.key=m.key JOIN entries e ON e.node_id=w.node_id AND e.side=1 WHERE w.leaf=1 ORDER BY w.node_id LIMIT 16"))?;
            let mut paths = query.query([])?;
            while let Some(row) = paths.next()? {
                warnings.push(row.get(0)?);
            }
        }
    }
    Ok(FullTreemapData {
        image_data_url: format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        ),
        atlas_width: ATLAS_WIDTH,
        atlas_height: ATLAS_HEIGHT,
        file_count: files as u64,
        visible_file_count: visible as u64,
        rendered_block_count: rendered as u64,
        max_depth,
        added_file_count: added as u64,
        weight_total: weight.to_string(),
        positive_total: positive.to_string(),
        negative_total: negative.to_string(),
        net_delta: net.to_string(),
        exported_total: exported.to_string(),
        labels,
        warnings,
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
    Ok(conn.query_row("WITH RECURSIVE ancestors(id,parent_id,d) AS (SELECT id,parent_id,0 FROM nodes WHERE id=?2 UNION ALL SELECT n.id,n.parent_id,a.d+1 FROM nodes n JOIN ancestors a ON n.id=a.parent_id) SELECT r.x,r.y,r.width,r.height FROM ancestors a JOIN treemap_rects r ON r.view=?1 AND r.key=a.id ORDER BY a.d LIMIT 1",params![cache_view(metric,mode,max_depth),n], |r| Ok(TreemapRect{x:r.get(0)?,y:r.get(1)?,width:r.get(2)?,height:r.get(3)?})).optional()?)
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
    let mut query=conn.prepare(&format!("SELECT r.node_id,n.name,coalesce(e.path,n.path),coalesce(e.extension,''),r.weight,r.value,c.status,r.x,r.y,r.width,r.height,e.kind,w.leaf,w.added,w.removed,w.type_changed,w.modified,r.leaf FROM treemap_spatial s CROSS JOIN treemap_rects r NOT INDEXED JOIN nodes n ON n.id=r.node_id JOIN comparison_nodes c ON c.node_id=r.node_id JOIN treemap_weights w ON w.key=r.key LEFT JOIN entries e ON e.node_id=n.id AND e.side=?4 WHERE s.x0<=?2 AND s.x1>=?2 AND s.y0<=?3 AND s.y1>=?3 AND r.id>=s.id*{SPATIAL_BLOCK} AND r.id<(s.id+1)*{SPATIAL_BLOCK} AND r.view=?1 AND (r.leaf=1 OR r.header_height>0) AND r.x<=?2 AND r.y<=?3 AND (?2<r.x+r.width OR (?2=1.0 AND r.x+r.width>=1.0)) AND (?3<r.y+CASE WHEN r.leaf=1 THEN r.height ELSE r.header_height END OR (?3=1.0 AND r.y+CASE WHEN r.leaf=1 THEN r.height ELSE r.header_height END>=1.0)) ORDER BY r.node_id LIMIT 1"))?;
    Ok(query
        .query_row(
            params![cache_view(metric, mode, max_depth), x, y, side],
            |r| {
                let collapsed = r.get::<_, i64>(12)? == 0 && r.get::<_, i64>(17)? == 1;
                let is_directory = store::root_text(r, 11)? == "directory";
                let status = store::root_text(r, 6)?;
                let status = if is_directory {
                    if r.get::<_, i64>(15)? > 0 {
                        Status::TypeChanged
                    } else if r.get::<_, i64>(16)? > 0 {
                        Status::Modified
                    } else if r.get::<_, i64>(14)? > 0 {
                        Status::Removed
                    } else if r.get::<_, i64>(13)? > 0 {
                        Status::Added
                    } else {
                        Status::Unchanged
                    }
                } else {
                    match status {
                        "added" => Status::Added,
                        "removed" => Status::Removed,
                        "modified" => Status::Modified,
                        "typeChanged" => Status::TypeChanged,
                        _ => Status::Unchanged,
                    }
                };
                Ok(TreemapHit {
                    node_id: format!("n{}", r.get::<_, i64>(0)?),
                    name: r.get(1)?,
                    path: r.get(2)?,
                    extension: r.get(3)?,
                    weight: r.get::<_, i64>(4)?.to_string(),
                    value: r.get::<_, i64>(5)?.to_string(),
                    status,
                    kind: if is_directory {
                        NodeKind::Directory
                    } else {
                        NodeKind::File
                    },
                    collapsed,
                    rect: TreemapRect {
                        x: r.get(7)?,
                        y: r.get(8)?,
                        width: r.get(9)?,
                        height: r.get(10)?,
                    },
                })
            },
        )
        .optional()?)
}
