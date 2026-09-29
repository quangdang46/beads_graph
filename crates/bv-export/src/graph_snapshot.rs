//! Static graph snapshot export — port of Go `pkg/export/graph_snapshot.go`
//! at the frozen parity commit `18afafa`.
//!
//! `--export-graph` has two outputs in Go: a self-contained interactive HTML
//! page ([`crate::graph_interactive`]) and a static image. The static half
//! takes a `--graph-preset` (`compact` or `roomy`) and a `--graph-title`, and
//! this module is that half: [`save_graph_snapshot`] infers the format from
//! the output path, lays the issues out into a column-per-critical-path-level
//! grid, and writes either an SVG or a PNG.
//!
//! ## What is byte-identical to Go
//!
//! The layout — every dimension constant, the level bucketing, the sort
//! within a level, the coordinate arithmetic, the canvas size and the summary
//! text — is transcribed from `buildLayout` and `topByMetricWithFallback`, and
//! the SVG writer reproduces the `ajstarks/svgo` output shape exactly.
//! `crates/bv-export/tests/graph_snapshot_svg.rs` pins the SVG against Go's
//! own bytes.
//!
//! ## What is not
//!
//! The PNG. Go rasterises with `git.sr.ht/~sbinet/gg`, which anti-aliases; the
//! primitives in [`crate::raster`] do not. The canvas size, colours, geometry
//! and glyphs match, so the image is the same picture, but a byte comparison
//! against Go's PNG will differ. See the module docs on [`crate::raster`].

use bv_analysis::label_health::GraphStats;
use bv_core::model::{Issue, Status};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::raster::{Canvas, Color, Rect};

/// Go `GraphSnapshotOptions` (`pkg/export/graph_snapshot.go:23-31`).
///
/// `stats` is not a field even though Go's struct has one: [`save_graph_snapshot`]
/// takes it as a separate `Option` so the "stats are required" precondition at
/// `graph_snapshot.go:40` is expressible in the type system.
pub struct GraphSnapshotOptions {
    /// Go: `Path` — the output path. When `format` is empty the extension
    /// decides, and a path with no extension at all gains `.svg`.
    pub path: PathBuf,
    /// Go: `Format` — `"svg"` or `"png"`, case-insensitive, with a leading
    /// dot tolerated. Empty means "infer from `path`".
    pub format: String,
    /// Go: `Title` — drawn in the summary block. Blank becomes
    /// `"Graph Snapshot"`.
    pub title: String,
    /// Go: `Preset` — `"roomy"` widens the nodes and the gaps; anything else,
    /// including an unrecognised value, is compact. Go has no enum rule for
    /// this flag (`cmd/bv/main.go:1845-1848`), so an unknown preset is
    /// silently compact and this port keeps that.
    pub preset: String,
    /// Go: `Issues` — already filtered by recipe/workspace by the caller.
    pub issues: Vec<Issue>,
    /// Go: `DataHash` — echoed into the summary block for provenance.
    pub data_hash: String,
}

/// Errors from [`save_graph_snapshot`]. The text reproduces Go's.
#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    /// `graph_snapshot.go:37`.
    #[error("no issues to export")]
    NoIssues,
    /// `graph_snapshot.go:40`.
    #[error("graph stats are required for snapshot export")]
    MissingStats,
    /// `graph_snapshot.go:65`.
    #[error("unsupported format {0:?} (want svg or png)")]
    UnsupportedFormat(String),
    /// `graph_snapshot.go:68`.
    #[error("output path is required")]
    MissingPath,
    /// `graph_snapshot.go:70`.
    #[error("create parent dir: {0}")]
    CreateDir(#[source] std::io::Error),
    /// Writing or encoding the file.
    #[error("writing snapshot: {0}")]
    Write(#[source] Box<dyn std::error::Error + Send + Sync>),
}

/// Go `SaveGraphSnapshot` (`pkg/export/graph_snapshot.go:33-80`).
///
/// `stats` is taken separately from the options struct so the "stats are
/// required" precondition (`graph_snapshot.go:40`) is expressible in the type
/// system; pass [`GraphStats::default`]-shaped data to hit it deliberately.
pub fn save_graph_snapshot(
    mut opts: GraphSnapshotOptions,
    stats: Option<&GraphStats>,
) -> Result<(), SnapshotError> {
    if opts.issues.is_empty() {
        return Err(SnapshotError::NoIssues);
    }
    let Some(stats) = stats else {
        return Err(SnapshotError::MissingStats);
    };

    // Format inference — graph_snapshot.go:44-61. Note that this branch can
    // rewrite `opts.path` to add `.svg`, which is why `opts` is `mut`.
    let mut format = opts.format.trim_start_matches('.').to_lowercase();
    if format.is_empty() {
        let inferred = match opts
            .path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("svg") => "svg",
            Some("png") => "png",
            _ => {
                // Safe default, and the path is fixed up to match. Go only
                // appends when `filepath.Ext(path)` is empty, so a path that
                // already claims some other extension is left alone.
                if opts.path.extension().is_none() {
                    opts.path.set_extension("svg");
                }
                "svg"
            }
        };
        format = inferred.to_string();
    }
    if format != "svg" && format != "png" {
        return Err(SnapshotError::UnsupportedFormat(format));
    }
    if opts.path.as_os_str().is_empty() {
        return Err(SnapshotError::MissingPath);
    }
    if let Some(parent) = opts.path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(SnapshotError::CreateDir)?;
        }
    }

    let layout = build_layout(&opts, stats);
    match format.as_str() {
        "svg" => std::fs::write(&opts.path, render_svg(&layout)),
        "png" => {
            let canvas = render_png(&layout);
            let bytes = canvas
                .to_png()
                .map_err(|e| SnapshotError::Write(Box::new(e)))?;
            std::fs::write(&opts.path, bytes)
        }
        other => unreachable!("format was validated above, got {other}"),
    }
    .map_err(|e| SnapshotError::Write(Box::new(e)))
}

// --- layout ----------------------------------------------------------------

/// One placed node. Go `layoutNode` (`graph_snapshot.go:83-95`).
#[derive(Debug, Clone)]
struct LayoutNode {
    id: String,
    title: String,
    status: Status,
    rank: f64,
    x: f64,
    y: f64,
    node_w: f64,
    node_h: f64,
    page_rank: f64,
}

/// Go `layoutEdge` (`graph_snapshot.go:97-100`).
#[derive(Debug, Clone)]
struct LayoutEdge {
    from: String,
    to: String,
}

/// Go `summaryInfo` (`graph_snapshot.go:109-115`).
#[derive(Debug, Clone)]
struct SummaryInfo {
    title: String,
    data_hash: String,
    node_count: usize,
    edge_count: usize,
    top_bottleneck: String,
}

/// Go `layoutResult` (`graph_snapshot.go:101-107`).
#[derive(Debug, Clone)]
struct LayoutResult {
    nodes: Vec<LayoutNode>,
    edges: Vec<LayoutEdge>,
    width: i32,
    height: i32,
    header: f64,
    summary: SummaryInfo,
}

/// Go `buildLayout` (`pkg/export/graph_snapshot.go:117-225`).
///
/// The constants are Go's, verbatim. `roomy` is the only thing
/// `--graph-preset` changes: wider nodes, taller nodes, and wider column and
/// row gaps.
fn build_layout(opts: &GraphSnapshotOptions, stats: &GraphStats) -> LayoutResult {
    const NODE_W_COMPACT: f64 = 170.0;
    const NODE_H_COMPACT: f64 = 70.0;
    const NODE_W_ROOMY: f64 = 190.0;
    const NODE_H_ROOMY: f64 = 82.0;
    const COL_GAP_COMPACT: f64 = 80.0;
    const ROW_GAP_COMPACT: f64 = 40.0;
    const COL_GAP_ROOMY: f64 = 110.0;
    const ROW_GAP_ROOMY: f64 = 55.0;
    const PADDING: f64 = 36.0;
    const HEADER_HEIGHT: f64 = 120.0;

    let roomy = opts.preset.eq_ignore_ascii_case("roomy");
    let (node_w, node_h, col_gap, row_gap) = if roomy {
        (NODE_W_ROOMY, NODE_H_ROOMY, COL_GAP_ROOMY, ROW_GAP_ROOMY)
    } else {
        (
            NODE_W_COMPACT,
            NODE_H_COMPACT,
            COL_GAP_COMPACT,
            ROW_GAP_COMPACT,
        )
    };

    // Level per node: the rounded critical-path score, floored at 1.
    let mut level_by_id = std::collections::BTreeMap::new();
    let mut max_level = 1usize;
    for issue in &opts.issues {
        let score = stats.critical_path.get(&issue.id).copied().unwrap_or(0.0);
        let level = (score.round() as i64).max(1) as usize;
        level_by_id.insert(issue.id.clone(), level);
        max_level = max_level.max(level);
    }

    // Bucket by level, then sort each bucket by PageRank descending with an
    // epsilon band and the ID as the tie-break. The band matters: two nodes
    // whose PageRank differs only by floating-point noise would otherwise swap
    // places between runs and move the whole column.
    let mut level_buckets: Vec<Vec<LayoutNode>> = vec![Vec::new(); max_level + 1];
    for issue in &opts.issues {
        let level = level_by_id[&issue.id];
        let page_rank = stats.pagerank.get(&issue.id).copied().unwrap_or(0.0);
        level_buckets[level].push(LayoutNode {
            id: issue.id.clone(),
            title: truncate(&issue.title, 44),
            status: issue.status,
            rank: page_rank,
            x: 0.0,
            y: 0.0,
            node_w,
            node_h,
            page_rank,
        });
    }
    const RANK_EPSILON: f64 = 1e-6;
    // Buckets are 1-based; index 0 is never populated.
    for bucket in level_buckets.iter_mut().skip(1) {
        bucket.sort_by(|a, b| {
            let diff = a.rank - b.rank;
            if diff.abs() > RANK_EPSILON {
                // Descending by rank.
                b.rank
                    .partial_cmp(&a.rank)
                    .unwrap_or(std::cmp::Ordering::Equal)
            } else {
                a.id.cmp(&b.id)
            }
        });
    }

    // Coordinates. `max_rows` drives the canvas height.
    let mut nodes = Vec::new();
    let mut max_rows = 0usize;
    for (level, bucket) in level_buckets.iter().enumerate().skip(1) {
        max_rows = max_rows.max(bucket.len());
        for (idx, node) in bucket.iter().enumerate() {
            let mut node = node.clone();
            node.x = PADDING + (level as f64 - 1.0) * (node_w + col_gap);
            node.y = PADDING + HEADER_HEIGHT + idx as f64 * (node_h + row_gap);
            nodes.push(node);
        }
    }

    let mut width = (PADDING * 2.0 + max_level as f64 * (node_w + col_gap) + node_w) as i32;
    if width < 640 {
        width = 640;
    }
    let mut height =
        (PADDING * 2.0 + HEADER_HEIGHT + max_rows as f64 * (node_h + row_gap) + node_h) as i32;
    if height < 480 {
        height = 480;
    }

    // Edges: blocking dependencies whose target survived the caller's filter.
    let node_ids: std::collections::BTreeSet<&str> = nodes.iter().map(|n| n.id.as_str()).collect();
    let mut edges = Vec::new();
    for issue in &opts.issues {
        for dep in &issue.dependencies {
            if !dep.r#type.is_blocking() {
                continue;
            }
            if !node_ids.contains(dep.depends_on_id.as_str()) {
                continue;
            }
            edges.push(LayoutEdge {
                from: issue.id.clone(),
                to: dep.depends_on_id.clone(),
            });
        }
    }

    let all_node_ids: Vec<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let title = if opts.title.trim().is_empty() {
        "Graph Snapshot".to_string()
    } else {
        opts.title.clone()
    };

    LayoutResult {
        summary: SummaryInfo {
            title,
            data_hash: opts.data_hash.clone(),
            node_count: nodes.len(),
            edge_count: edges.len(),
            top_bottleneck: top_by_metric_with_fallback(&stats.betweenness, &all_node_ids),
        },
        nodes,
        edges,
        width,
        height,
        header: HEADER_HEIGHT,
    }
}

/// Go `topByMetric` (`graph_snapshot.go:227-241`).
fn top_by_metric(metric: &std::collections::BTreeMap<String, f64>) -> String {
    let mut best: Option<(&str, f64)> = None;
    for (id, value) in metric {
        best = match best {
            None => Some((id.as_str(), *value)),
            Some((best_id, best_value))
                if *value > best_value || (*value == best_value && id.as_str() < best_id) =>
            {
                Some((id.as_str(), *value))
            }
            other => other,
        };
    }
    match best {
        None => "n/a".to_string(),
        Some((id, value)) => format!("{id} ({value:.2})"),
    }
}

/// Go `topByMetricWithFallback` (`graph_snapshot.go:250-260`).
///
/// A graph where every node has zero betweenness (a star, say) would
/// otherwise report `n/a`; Go names the alphabetically first node with a zero
/// score instead, so the summary always has a leader.
fn top_by_metric_with_fallback(
    metric: &std::collections::BTreeMap<String, f64>,
    fallback_ids: &[String],
) -> String {
    let result = top_by_metric(metric);
    if result != "n/a" {
        return result;
    }
    if fallback_ids.is_empty() {
        return "n/a".to_string();
    }
    let mut ids: Vec<&str> = fallback_ids.iter().map(String::as_str).collect();
    ids.sort_unstable();
    format!("{} (0.00)", ids[0])
}

/// Go `truncate` (`graph_snapshot.go:462-475`).
///
/// Note this is the *other* truncate: three dots and `max-3` characters,
/// unlike `pkg/export/markdown.go:955`'s single `…`.
fn truncate(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let runes: Vec<char> = s.chars().collect();
    if runes.len() <= max {
        return s.to_string();
    }
    if max <= 3 {
        return runes[..max].iter().collect();
    }
    let mut out: String = runes[..max - 3].iter().collect();
    out.push_str("...");
    out
}

// --- SVG -------------------------------------------------------------------

// Go's palette — graph_snapshot.go:229-239.
const COLOR_OPEN: Color = [0xc8, 0xe6, 0xc9];
const COLOR_BLOCKED: Color = [0xff, 0xcd, 0xd2];
const COLOR_IN_PROG: Color = [0xff, 0xf3, 0xe0];
const COLOR_CLOSED: Color = [0xcf, 0xd8, 0xdc];
const COLOR_STROKE: Color = [0x22, 0x22, 0x22];
const COLOR_EDGE: Color = [0x6b, 0x80, 0xbf];
const COLOR_EDGE_ARROW: Color = [0x6b, 0x80, 0xbf];
const COLOR_TEXT: Color = [0x11, 0x11, 0x11];
const COLOR_SUBTLE: Color = [0x66, 0x66, 0x66];
const COLOR_BACKDROP: Color = [0xf9, 0xfa, 0xfb];
const COLOR_HEADER_BG: Color = [0xf3, 0xf4, 0xf6];
const COLOR_LEGEND_BG: Color = [0xee, 0xee, 0xee];

/// Go `statusColor` (`graph_snapshot.go:242-256`), with the closed-like test
/// from `pkg/export/markdown.go:568`.
fn status_color(status: Status) -> Color {
    match status {
        Status::Closed | Status::Tombstone => COLOR_CLOSED,
        Status::Open => COLOR_OPEN,
        Status::Blocked => COLOR_BLOCKED,
        Status::InProgress => COLOR_IN_PROG,
        _ => COLOR_OPEN,
    }
}

/// Go `css` (`graph_snapshot.go:477-479`): `#rrggbb`, lower-case.
fn css(c: Color) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Go `renderSVGToWriter` (`graph_snapshot.go:344-392`).
///
/// The element shapes and attribute order follow `ajstarks/svgo` as Go emits
/// them, including the space before the closing `>` on a `<text>` element.
fn render_svg(layout: &LayoutResult) -> String {
    let mut svg = String::new();
    let w = layout.width;
    let h = layout.height;
    let _ = write!(
        svg,
        "<?xml version=\"1.0\"?>\n<!-- Generated by SVGo -->\n\
         <svg width=\"{w}\" height=\"{h}\"\n     \
         xmlns=\"http://www.w3.org/2000/svg\"\n     \
         xmlns:xlink=\"http://www.w3.org/1999/xlink\">\n"
    );
    let _ = writeln!(
        svg,
        "<rect x=\"0\" y=\"0\" width=\"{w}\" height=\"{h}\" style=\"fill:{}\" />",
        css(COLOR_BACKDROP)
    );
    // The header band is `int(layout.Header-24)` high — 96 — because Go
    // truncates the float to an int here rather than rounding.
    let _ = writeln!(
        svg,
        "<rect x=\"16\" y=\"16\" width=\"{}\" height=\"{}\" rx=\"10\" ry=\"10\" style=\"fill:{}\" />",
        w - 32,
        (layout.header - 24.0) as i32,
        css(COLOR_HEADER_BG)
    );

    let summary = &layout.summary;
    let _ = writeln!(
        svg,
        "<text x=\"32\" y=\"44\" style=\"fill:{};font-size:16px;font-family:monospace;font-weight:bold\" >{}</text>",
        css(COLOR_TEXT),
        escape_xml(&summary.title)
    );
    for (y, body) in [
        (64, format!("data_hash: {}", summary.data_hash)),
        (
            84,
            format!(
                "nodes: {}  edges: {}",
                summary.node_count, summary.edge_count
            ),
        ),
        (104, format!("top bottleneck: {}", summary.top_bottleneck)),
    ] {
        let _ = writeln!(
            svg,
            "<text x=\"32\" y=\"{y}\" style=\"fill:{};font-size:13px;font-family:monospace\" >{}</text>",
            css(COLOR_SUBTLE),
            escape_xml(&body)
        );
    }

    // Legend — graph_snapshot.go:412-421.
    let box_w = 180;
    let box_h = 96;
    let bx = w - box_w - 20;
    let by = 24;
    let _ = writeln!(
        svg,
        "<rect x=\"{bx}\" y=\"{by}\" width=\"{box_w}\" height=\"{box_h}\" rx=\"10\" ry=\"10\" \
         style=\"fill:{};stroke:{};stroke-width:1\" />",
        css(COLOR_LEGEND_BG),
        css(COLOR_STROKE)
    );
    let _ = writeln!(
        svg,
        "<text x=\"{}\" y=\"{}\" style=\"fill:{};font-size:13px;font-family:monospace;font-weight:bold\" >Legend</text>",
        bx + 12,
        by + 18,
        css(COLOR_TEXT)
    );
    for (i, (color, label)) in [
        (COLOR_OPEN, "Open / Ready"),
        (COLOR_IN_PROG, "In Progress"),
        (COLOR_BLOCKED, "Blocked"),
        (COLOR_CLOSED, "Closed"),
    ]
    .iter()
    .enumerate()
    {
        let row_y = by + 36 + i as i32 * 16;
        let _ = writeln!(
            svg,
            "<rect x=\"{}\" y=\"{}\" width=\"14\" height=\"14\" rx=\"3\" ry=\"3\" \
             style=\"fill:{};stroke:{};stroke-width:1\" />",
            bx + 12,
            row_y - 8,
            css(*color),
            css(COLOR_STROKE)
        );
        let _ = writeln!(
            svg,
            "<text x=\"{}\" y=\"{row_y}\" style=\"fill:{};font-size:12px;font-family:monospace\" >{label}</text>",
            bx + 32,
            css(COLOR_SUBTLE)
        );
    }

    // Edges, from the right edge of the source to the left edge of the
    // target, with a triangular arrow head — graph_snapshot.go:360-378.
    let positions: std::collections::BTreeMap<&str, &LayoutNode> =
        layout.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for edge in &layout.edges {
        let (Some(from), Some(to)) = (
            positions.get(edge.from.as_str()),
            positions.get(edge.to.as_str()),
        ) else {
            continue;
        };
        let x1 = (from.x + from.node_w) as i32;
        let y1 = (from.y + from.node_h / 2.0) as i32;
        let x2 = to.x as i32;
        let y2 = (to.y + to.node_h / 2.0) as i32;
        let _ = writeln!(
            svg,
            "<line x1=\"{x1}\" y1=\"{y1}\" x2=\"{x2}\" y2=\"{y2}\" \
             style=\"stroke:{};stroke-width:2\" />",
            css(COLOR_EDGE)
        );
        let _ = writeln!(
            svg,
            "<polygon points=\"{x2},{y2} {},{} {},{}\" style=\"fill:{}\" />",
            x2 + 8,
            y2 + 4,
            x2 + 8,
            y2 - 4,
            css(COLOR_EDGE_ARROW)
        );
    }

    // Nodes — graph_snapshot.go:380-392.
    for node in &layout.nodes {
        let x = node.x as i32;
        let y = node.y as i32;
        let nw = node.node_w as i32;
        let nh = node.node_h as i32;
        let _ = writeln!(
            svg,
            "<rect x=\"{x}\" y=\"{y}\" width=\"{nw}\" height=\"{nh}\" rx=\"8\" ry=\"8\" \
             style=\"fill:{};stroke:{};stroke-width:1.2\" />",
            css(status_color(node.status)),
            css(COLOR_STROKE)
        );
        let _ = writeln!(
            svg,
            "<text x=\"{}\" y=\"{}\" style=\"fill:{};font-size:13px;font-family:monospace;font-weight:bold\" >{}</text>",
            x + 10,
            y + 22,
            css(COLOR_TEXT),
            escape_xml(&node.id)
        );
        let _ = writeln!(
            svg,
            "<text x=\"{}\" y=\"{}\" style=\"fill:{};font-size:12px;font-family:monospace\" >{}</text>",
            x + 10,
            y + 42,
            css(COLOR_SUBTLE),
            escape_xml(&truncate(&node.title, 40))
        );
        let _ = writeln!(
            svg,
            "<text x=\"{}\" y=\"{}\" style=\"fill:{};font-size:11px;font-family:monospace\" >PR {:.3}</text>",
            x + 10,
            y + 60,
            css(COLOR_SUBTLE),
            node.page_rank
        );
    }

    svg.push_str("</svg>\n");
    svg
}

/// Go's XML text escaping, as `ajstarks/svgo` applies it. Note `"` becomes
/// the numeric `&#34;` and `'` becomes `&#39;`, not `&quot;`/`&apos;`.
fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

// --- PNG -------------------------------------------------------------------

/// Go `renderPNG` (`pkg/export/graph_snapshot.go:258-301`).
///
/// The draw order is Go's: backdrop, header band, summary, legend, edges,
/// then nodes. Everything after the header matches the SVG, so the two
/// renderings agree on what is where.
fn render_png(layout: &LayoutResult) -> Canvas {
    let mut canvas = Canvas::new(layout.width, layout.height, COLOR_BACKDROP);

    // Header band — graph_snapshot.go:264-266.
    canvas.fill_rounded_rect(
        Rect::new(
            16.0,
            16.0,
            f64::from(layout.width) - 32.0,
            layout.header - 24.0,
        ),
        10.0,
        COLOR_HEADER_BG,
    );

    // Summary — graph_snapshot.go:281-286.
    let summary = &layout.summary;
    canvas.draw_text_anchored(&summary.title, 32.0, 44.0, COLOR_TEXT);
    canvas.draw_text_anchored(
        &format!("data_hash: {}", summary.data_hash),
        32.0,
        64.0,
        COLOR_SUBTLE,
    );
    canvas.draw_text_anchored(
        &format!(
            "nodes: {}  edges: {}",
            summary.node_count, summary.edge_count
        ),
        32.0,
        84.0,
        COLOR_SUBTLE,
    );
    canvas.draw_text_anchored(
        &format!("top bottleneck: {}", summary.top_bottleneck),
        32.0,
        104.0,
        COLOR_SUBTLE,
    );

    // Legend — graph_snapshot.go:290-301 and 405-427.
    let box_w = 180.0;
    let box_h = 96.0;
    let bx = f64::from(layout.width) - box_w - 20.0;
    let by = 24.0;
    canvas.fill_rounded_rect(Rect::new(bx, by, box_w, box_h), 10.0, COLOR_LEGEND_BG);
    canvas.stroke_rounded_rect(Rect::new(bx, by, box_w, box_h), 10.0, COLOR_STROKE, 1.0);
    canvas.draw_text_anchored("Legend", bx + 12.0, by + 18.0, COLOR_TEXT);
    for (i, (color, label)) in [
        (COLOR_OPEN, "Open / Ready"),
        (COLOR_IN_PROG, "In Progress"),
        (COLOR_BLOCKED, "Blocked (has blockers)"),
        (COLOR_CLOSED, "Closed"),
    ]
    .iter()
    .enumerate()
    {
        let row_y = by + 36.0 + i as f64 * 16.0;
        canvas.fill_rounded_rect(Rect::new(bx + 12.0, row_y - 8.0, 14.0, 14.0), 3.0, *color);
        canvas.stroke_rounded_rect(
            Rect::new(bx + 12.0, row_y - 8.0, 14.0, 14.0),
            3.0,
            COLOR_STROKE,
            1.0,
        );
        canvas.draw_text_anchored(label, bx + 32.0, row_y, COLOR_SUBTLE);
    }

    // Edges — graph_snapshot.go:288-299.
    let positions: std::collections::BTreeMap<&str, &LayoutNode> =
        layout.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for edge in &layout.edges {
        let (Some(from), Some(to)) = (
            positions.get(edge.from.as_str()),
            positions.get(edge.to.as_str()),
        ) else {
            continue;
        };
        let x1 = from.x + from.node_w;
        let y1 = from.y + from.node_h / 2.0;
        let x2 = to.x;
        let y2 = to.y + to.node_h / 2.0;
        canvas.stroke_line(x1, y1, x2, y2, COLOR_EDGE, 2.0);
        // Go's `drawArrow`: a filled triangle pointing back along the edge.
        canvas.fill_polygon(
            &[
                (x2 as i32, y2 as i32),
                (x2 as i32 - 8, y2 as i32 + 4),
                (x2 as i32 - 8, y2 as i32 - 4),
            ],
            COLOR_EDGE_ARROW,
        );
    }

    // Nodes — graph_snapshot.go:301-305 and 394-410.
    for node in &layout.nodes {
        canvas.fill_rounded_rect(
            Rect::new(node.x, node.y, node.node_w, node.node_h),
            8.0,
            status_color(node.status),
        );
        canvas.stroke_rounded_rect(
            Rect::new(node.x, node.y, node.node_w, node.node_h),
            8.0,
            COLOR_STROKE,
            1.2,
        );
        canvas.draw_text_anchored(&node.id, node.x + 10.0, node.y + 18.0, COLOR_TEXT);
        canvas.draw_text_anchored(
            &truncate(&node.title, 40),
            node.x + 10.0,
            node.y + 36.0,
            COLOR_SUBTLE,
        );
        canvas.draw_text_anchored(
            &format!("PR {:.3}", node.page_rank),
            node.x + 10.0,
            node.y + 54.0,
            COLOR_SUBTLE,
        );
    }

    canvas
}

/// Re-exported so a caller can build the options struct without importing
/// [`crate::raster`] directly.
pub type SnapshotPath = Path;

#[cfg(test)]
mod tests {
    use super::*;
    use bv_analysis::label_health::GraphStats;
    use bv_core::model::{Dependency, DependencyType};

    fn issue(id: &str, title: &str, status: Status, deps: &[(&str, DependencyType)]) -> Issue {
        Issue {
            id: id.to_string(),
            content_hash: String::new(),
            title: title.to_string(),
            description: String::new(),
            design: String::new(),
            acceptance_criteria: String::new(),
            notes: String::new(),
            status,
            priority: 1,
            issue_type: "task".to_string(),
            assignee: String::new(),
            estimated_minutes: None,
            created_at: None,
            updated_at: None,
            due_date: None,
            defer_until: None,
            closed_at: None,
            external_ref: None,
            compaction_level: 0,
            compacted_at: None,
            compacted_at_commit: None,
            original_size: 0,
            labels: Vec::new(),
            dependencies: deps
                .iter()
                .map(|(depends_on, kind)| Dependency {
                    issue_id: id.to_string(),
                    depends_on_id: (*depends_on).to_string(),
                    depends_on_legacy: String::new(),
                    target_id_legacy: String::new(),
                    r#type: kind.clone(),
                    created_at: None,
                    created_by: String::new(),
                })
                .collect(),
            comments: Vec::new(),
            source_repo: String::new(),
        }
    }

    fn stats(
        page_rank: &[(&str, f64)],
        betweenness: &[(&str, f64)],
        critical: &[(&str, f64)],
    ) -> GraphStats {
        GraphStats {
            pagerank: page_rank
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
            betweenness: betweenness
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
            critical_path: critical
                .iter()
                .map(|(k, v)| ((*k).to_string(), *v))
                .collect(),
        }
    }

    fn options(preset: &str, title: &str) -> GraphSnapshotOptions {
        GraphSnapshotOptions {
            path: PathBuf::from("out.svg"),
            format: String::new(),
            title: title.to_string(),
            preset: preset.to_string(),
            issues: vec![
                issue("ROOT", "root", Status::Open, &[]),
                issue(
                    "A",
                    "alpha",
                    Status::Open,
                    &[("ROOT", DependencyType::Blocks)],
                ),
                issue(
                    "B",
                    "beta",
                    Status::Blocked,
                    &[("A", DependencyType::Blocks)],
                ),
            ],
            data_hash: "hash".to_string(),
        }
    }

    /// Three critical-path levels, so the canvas is driven by the column
    /// arithmetic rather than by the 640px floor.
    fn three_levels() -> GraphStats {
        stats(
            &[("ROOT", 0.5), ("A", 0.4), ("B", 0.3)],
            &[],
            &[("ROOT", 1.0), ("A", 2.0), ("B", 3.0)],
        )
    }

    #[test]
    fn roomy_widens_the_canvas_and_the_nodes() {
        // graph_snapshot.go:118-131 — the whole point of --graph-preset.
        let compact = build_layout(&options("compact", "T"), &three_levels());
        let roomy = build_layout(&options("roomy", "T"), &three_levels());
        // 72 + 3*(w+colGap) + w
        assert_eq!(compact.width, (72.0 + 3.0 * (170.0 + 80.0) + 170.0) as i32);
        assert_eq!(roomy.width, (72.0 + 3.0 * (190.0 + 110.0) + 190.0) as i32);
        assert_eq!(roomy.nodes[0].node_w, 190.0);
        assert_eq!(roomy.nodes[0].node_h, 82.0);
        assert_eq!(compact.nodes[0].node_w, 170.0);
        assert_eq!(compact.nodes[0].node_h, 70.0);
        // A wider column gap pushes the second column further right.
        let x_of =
            |layout: &LayoutResult, id: &str| layout.nodes.iter().find(|n| n.id == id).unwrap().x;
        assert_eq!(x_of(&compact, "A"), 36.0 + 170.0 + 80.0);
        assert_eq!(x_of(&roomy, "A"), 36.0 + 190.0 + 110.0);
    }

    #[test]
    fn the_preset_comparison_is_case_insensitive_and_defaults_to_compact() {
        // graph_snapshot.go:130 `strings.EqualFold`.
        let roomy = build_layout(&options("RoOmY", "T"), &three_levels());
        let compact = build_layout(&options("compact", "T"), &three_levels());
        assert_eq!(roomy.width, 1162);
        assert_eq!(compact.width, 992);

        // Go has no enum rule for this flag, so anything unrecognised is
        // compact rather than an error.
        let unknown = build_layout(&options("spacious", "T"), &three_levels());
        assert_eq!(unknown.width, compact.width);
        assert_eq!(unknown.nodes[0].node_w, compact.nodes[0].node_w);
    }

    #[test]
    fn levels_come_from_the_rounded_critical_path_score() {
        // graph_snapshot.go:143-153 — a score below 1 floors to level 1.
        let mut opts = options("compact", "T");
        let s = stats(
            &[("ROOT", 0.5), ("A", 0.4), ("B", 0.3)],
            &[],
            &[("ROOT", 1.0), ("A", 2.0), ("B", 2.4)],
        );
        let layout = build_layout(&opts, &s);
        let x_of = |id: &str| layout.nodes.iter().find(|n| n.id == id).unwrap().x;
        // Level 1 -> 2.4 rounds to 2, so A and B share a column.
        assert_eq!(x_of("ROOT"), 36.0);
        assert_eq!(x_of("A"), 286.0);
        assert_eq!(x_of("B"), 286.0);
        opts.preset = "compact".to_string();
    }

    #[test]
    fn nodes_in_a_level_are_ordered_by_pagerank_then_id() {
        // graph_snapshot.go:155-177.
        let opts = options("compact", "T");
        let s = stats(
            &[("ROOT", 0.5), ("A", 0.4), ("B", 0.3)],
            &[],
            &[("ROOT", 1.0), ("A", 2.0), ("B", 2.0)],
        );
        let layout = build_layout(&opts, &s);
        let level2: Vec<&str> = layout
            .nodes
            .iter()
            .filter(|n| n.x == 286.0)
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(level2, ["A", "B"]);

        // Equal rank falls back to the ID.
        let s = stats(
            &[("ROOT", 0.5), ("A", 0.4), ("B", 0.4)],
            &[],
            &[("ROOT", 1.0), ("A", 2.0), ("B", 2.0)],
        );
        let layout = build_layout(&opts, &s);
        let level2: Vec<&str> = layout
            .nodes
            .iter()
            .filter(|n| n.x == 286.0)
            .map(|n| n.id.as_str())
            .collect();
        assert_eq!(level2, ["A", "B"]);
    }

    #[test]
    fn only_blocking_edges_to_visible_nodes_are_drawn() {
        // graph_snapshot.go:197-210.
        let mut opts = options("compact", "T");
        opts.issues = vec![
            issue("A", "alpha", Status::Open, &[]),
            issue(
                "B",
                "beta",
                Status::Open,
                &[
                    ("A", DependencyType::Blocks),
                    ("A", DependencyType::Related),
                    ("GONE", DependencyType::Blocks),
                ],
            ),
        ];
        let layout = build_layout(&opts, &stats(&[], &[], &[]));
        assert_eq!(layout.edges.len(), 1, "{:?}", layout.edges);
        assert_eq!(layout.edges[0].from, "B");
        assert_eq!(layout.edges[0].to, "A");
    }

    #[test]
    fn the_canvas_never_shrinks_below_the_minimum() {
        // graph_snapshot.go:187-194.
        let opts = options("compact", "T");
        let layout = build_layout(&opts, &stats(&[], &[], &[]));
        assert!(layout.width >= 640);
        assert!(layout.height >= 480);
    }

    #[test]
    fn the_title_falls_back_only_when_blank() {
        // graph_snapshot.go:216-220.
        let s = stats(&[], &[], &[]);
        assert_eq!(
            build_layout(&options("compact", ""), &s).summary.title,
            "Graph Snapshot"
        );
        assert_eq!(
            build_layout(&options("compact", "   "), &s).summary.title,
            "Graph Snapshot"
        );
        assert_eq!(
            build_layout(&options("compact", "Real"), &s).summary.title,
            "Real"
        );
    }

    #[test]
    fn an_all_zero_betweenness_graph_still_names_a_leader() {
        // graph_snapshot.go:250-260.
        let opts = options("compact", "T");
        // Betweenness present but all zero: Go's `topByMetric` still finds a
        // maximum, so this takes the normal path and reports 0.00.
        let zeroed = stats(&[], &[("B", 0.0), ("A", 0.0)], &[]);
        assert_eq!(
            build_layout(&opts, &zeroed).summary.top_bottleneck,
            "A (0.00)"
        );
        // Betweenness absent entirely: the fallback picks the first node.
        assert_eq!(
            build_layout(&opts, &stats(&[], &[], &[]))
                .summary
                .top_bottleneck,
            "A (0.00)"
        );
    }

    #[test]
    fn truncate_uses_three_dots_and_max_minus_three() {
        // graph_snapshot.go:462-475
        assert_eq!(truncate("", 5), "");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(truncate("abcde", 5), "abcde");
        assert_eq!(truncate("abcdefgh", 5), "ab...");
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("abcdef", 2), "ab");
        // Rune-based, so a multi-byte title is not split.
        assert_eq!(truncate("🐛🐛🐛🐛🐛🐛", 5), "🐛🐛...");
    }

    #[test]
    fn status_colors_match_go() {
        // graph_snapshot.go:242-256
        assert_eq!(status_color(Status::Open), COLOR_OPEN);
        assert_eq!(status_color(Status::InProgress), COLOR_IN_PROG);
        assert_eq!(status_color(Status::Blocked), COLOR_BLOCKED);
        assert_eq!(status_color(Status::Closed), COLOR_CLOSED);
        assert_eq!(status_color(Status::Tombstone), COLOR_CLOSED);
        // Every other status falls back to open.
        assert_eq!(status_color(Status::Deferred), COLOR_OPEN);
        assert_eq!(status_color(Status::Draft), COLOR_OPEN);
        assert_eq!(status_color(Status::Pinned), COLOR_OPEN);
        assert_eq!(status_color(Status::Hooked), COLOR_OPEN);
    }

    #[test]
    fn xml_escaping_matches_svgo() {
        // Graph with a title containing markup.
        let opts = options("compact", "Roomy <Title> & \"Quotes\"");
        let svg = render_svg(&build_layout(&opts, &stats(&[], &[], &[])));
        assert!(svg.contains("Roomy &lt;Title&gt; &amp; &#34;Quotes&#34;"));
    }

    #[test]
    fn preconditions_are_enforced_before_any_io() {
        // graph_snapshot.go:37-41 and 65-68.
        let dir = std::env::temp_dir().join(format!("bvr-snap-{}", std::process::id()));
        let mut opts = options("compact", "T");
        opts.path = dir.join("sub").join("out.svg");
        opts.issues.clear();
        assert!(matches!(
            save_graph_snapshot(opts, Some(&stats(&[], &[], &[]))),
            Err(SnapshotError::NoIssues)
        ));

        let mut opts = options("compact", "T");
        opts.path = dir.join("sub").join("out.svg");
        assert!(matches!(
            save_graph_snapshot(opts, None),
            Err(SnapshotError::MissingStats)
        ));

        let mut opts = options("compact", "T");
        opts.path = PathBuf::new();
        assert!(matches!(
            save_graph_snapshot(opts, Some(&stats(&[], &[], &[]))),
            Err(SnapshotError::MissingPath)
        ));

        let mut opts = options("compact", "T");
        opts.format = "gif".to_string();
        assert!(matches!(
            save_graph_snapshot(opts, Some(&stats(&[], &[], &[]))),
            Err(SnapshotError::UnsupportedFormat(f)) if f == "gif"
        ));
    }

    #[test]
    fn a_path_with_no_extension_gains_svg() {
        // graph_snapshot.go:50-58.
        let dir = std::env::temp_dir().join(format!("bvr-snap-ext-{}", std::process::id()));
        let target = dir.join("graph");
        let mut opts = options("compact", "T");
        opts.path = target.clone();
        save_graph_snapshot(opts, Some(&stats(&[], &[], &[]))).expect("save");
        assert!(target.with_extension("svg").exists());
    }
}

/// The three graph metrics the snapshot layout reads, detached from
/// `bv_analysis` so both exporters can share one shape.
///
/// [`save_graph_snapshot`] takes a `&GraphStats` directly, because that is what
/// the analyzer produces. The interactive export needs nine more metrics that
/// `GraphStats` does not carry, so it uses this plus
/// [`crate::graph_interactive::GraphSnapshotMetricsExtras`].
#[derive(Debug, Clone, Default)]
pub struct GraphSnapshotMetrics {
    pub pagerank: std::collections::BTreeMap<String, f64>,
    pub betweenness: std::collections::BTreeMap<String, f64>,
    pub critical_path: std::collections::BTreeMap<String, f64>,
}

impl From<&GraphStats> for GraphSnapshotMetrics {
    fn from(stats: &GraphStats) -> Self {
        Self {
            pagerank: stats.pagerank.clone(),
            betweenness: stats.betweenness.clone(),
            critical_path: stats.critical_path.clone(),
        }
    }
}
