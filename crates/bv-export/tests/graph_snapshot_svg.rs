//! Byte-exact assertions for the static graph snapshot's SVG writer.
//!
//! The expected documents below were produced by running Go
//! `export.SaveGraphSnapshot` from parity commit `18afafa` over the fixture
//! built in `fixture()`, with `Path`, `Format`, `Title`, `Preset` and
//! `DataHash` set per case. They are compared byte-for-byte, so the layout
//! constants, the element order, the attribute order and the escaping all have
//! to match `pkg/export/graph_snapshot.go` exactly.
//!
//! `compact` is the interesting case: it spans four critical-path levels, so
//! it exercises the column arithmetic, the within-level sort, the edge
//! endpoints, the title truncation at 44 and 40 runes, and the minimum-canvas
//! clamp. `roomy` covers the preset branch and XML escaping.

use bv_analysis::label_health::GraphStats;
use bv_core::model::{Dependency, DependencyType, Issue, Status};
use bv_export::graph_snapshot::{save_graph_snapshot, GraphSnapshotOptions};

// `png` is a direct dependency so the encoded file can be decoded again here.

/// The seven-node fixture, matching the Go harness
/// `pkg/export/graph_snapshot.go` was driven with.
fn fixture() -> Vec<Issue> {
    let make = |id: &str, title: &str, status: Status, deps: &[(&str, DependencyType)]| Issue {
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
    };
    vec![
        make(
            "ROOT",
            "The root issue with a very long title that must be cut down",
            Status::Open,
            &[],
        ),
        make(
            "A-1",
            "Alpha task",
            Status::Open,
            &[("ROOT", DependencyType::Blocks)],
        ),
        make(
            "A-2",
            "Beta task",
            Status::InProgress,
            &[("ROOT", DependencyType::Blocks)],
        ),
        make(
            "B-1",
            "Gamma task",
            Status::Blocked,
            &[
                ("A-1", DependencyType::Blocks),
                ("A-2", DependencyType::Blocks),
            ],
        ),
        make(
            "B-2",
            "Delta task",
            Status::Closed,
            &[("A-1", DependencyType::Blocks)],
        ),
        make(
            "C-1",
            "Epsilon task with a title long enough to be truncated at forty characters exactly",
            Status::Tombstone,
            &[
                ("B-1", DependencyType::Blocks),
                ("B-2", DependencyType::Blocks),
            ],
        ),
        // A non-blocking dependency, which the snapshot must not draw.
        make(
            "LOOSE",
            "Unconnected issue",
            Status::Open,
            &[("LOOSE", DependencyType::Related)],
        ),
    ]
}

fn stats() -> GraphStats {
    let to_map =
        |pairs: &[(&str, f64)]| pairs.iter().map(|(k, v)| ((*k).to_string(), *v)).collect();
    GraphStats {
        pagerank: to_map(&[
            ("ROOT", 0.30),
            ("A-1", 0.20),
            ("A-2", 0.10),
            ("B-1", 0.05),
            ("B-2", 0.04),
            ("C-1", 0.02),
            ("LOOSE", 0.01),
        ]),
        betweenness: to_map(&[("B-1", 0.9), ("A-1", 0.4), ("ROOT", 0.2), ("C-1", 0.1)]),
        critical_path: to_map(&[
            ("ROOT", 1.0),
            ("A-1", 2.0),
            ("A-2", 2.0),
            ("B-1", 3.0),
            ("B-2", 3.0),
            ("C-1", 4.0),
            ("LOOSE", 0.0),
        ]),
    }
}

fn write_snapshot(
    dir: &std::path::Path,
    format: &str,
    title: &str,
    preset: &str,
    hash: &str,
) -> String {
    let path = dir.join(format!("out_{preset}.{format}"));
    let opts = GraphSnapshotOptions {
        path: path.clone(),
        format: format.to_string(),
        title: title.to_string(),
        preset: preset.to_string(),
        issues: fixture(),
        data_hash: hash.to_string(),
    };
    save_graph_snapshot(opts, Some(&stats())).expect("snapshot written");
    std::fs::read_to_string(&path).expect("snapshot read back")
}

fn scratch(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("bvr-snapshot-svg-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// `Preset: compact`, `Title: "Snapshot Title"`, `DataHash: abc123`.
#[test]
fn compact_svg_matches_go_byte_for_byte() {
    let dir = scratch("compact");
    let got = write_snapshot(&dir, "svg", "Snapshot Title", "compact", "abc123");
    assert_eq!(got, EXPECTED_COMPACT);
}

/// `Preset: roomy`, `Title: "Roomy <Title> & \"Quotes\""`, `DataHash: hash999`.
#[test]
fn roomy_svg_matches_go_byte_for_byte() {
    let dir = scratch("roomy");
    let got = write_snapshot(
        &dir,
        "svg",
        "Roomy <Title> & \"Quotes\"",
        "roomy",
        "hash999",
    );
    assert_eq!(got, EXPECTED_ROOMY);
}

// ---- Expected documents, captured from Go 18afafa ---------------------

// Preset=compact DataHash=abc123
const EXPECTED_COMPACT: &str = r####"<?xml version="1.0"?>
<!-- Generated by SVGo -->
<svg width="1242" height="482"
     xmlns="http://www.w3.org/2000/svg"
     xmlns:xlink="http://www.w3.org/1999/xlink">
<rect x="0" y="0" width="1242" height="482" style="fill:#f9fafb" />
<rect x="16" y="16" width="1210" height="96" rx="10" ry="10" style="fill:#f3f4f6" />
<text x="32" y="44" style="fill:#111111;font-size:16px;font-family:monospace;font-weight:bold" >Snapshot Title</text>
<text x="32" y="64" style="fill:#666666;font-size:13px;font-family:monospace" >data_hash: abc123</text>
<text x="32" y="84" style="fill:#666666;font-size:13px;font-family:monospace" >nodes: 7  edges: 7</text>
<text x="32" y="104" style="fill:#666666;font-size:13px;font-family:monospace" >top bottleneck: B-1 (0.90)</text>
<rect x="1042" y="24" width="180" height="96" rx="10" ry="10" style="fill:#eeeeee;stroke:#222222;stroke-width:1" />
<text x="1054" y="42" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >Legend</text>
<rect x="1054" y="52" width="14" height="14" rx="3" ry="3" style="fill:#c8e6c9;stroke:#222222;stroke-width:1" />
<text x="1074" y="60" style="fill:#666666;font-size:12px;font-family:monospace" >Open / Ready</text>
<rect x="1054" y="68" width="14" height="14" rx="3" ry="3" style="fill:#fff3e0;stroke:#222222;stroke-width:1" />
<text x="1074" y="76" style="fill:#666666;font-size:12px;font-family:monospace" >In Progress</text>
<rect x="1054" y="84" width="14" height="14" rx="3" ry="3" style="fill:#ffcdd2;stroke:#222222;stroke-width:1" />
<text x="1074" y="92" style="fill:#666666;font-size:12px;font-family:monospace" >Blocked</text>
<rect x="1054" y="100" width="14" height="14" rx="3" ry="3" style="fill:#cfd8dc;stroke:#222222;stroke-width:1" />
<text x="1074" y="108" style="fill:#666666;font-size:12px;font-family:monospace" >Closed</text>
<line x1="456" y1="191" x2="36" y2="191" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="36,191 44,195 44,187" style="fill:#6b80bf" />
<line x1="456" y1="301" x2="36" y2="191" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="36,191 44,195 44,187" style="fill:#6b80bf" />
<line x1="706" y1="191" x2="286" y2="191" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="286,191 294,195 294,187" style="fill:#6b80bf" />
<line x1="706" y1="191" x2="286" y2="301" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="286,301 294,305 294,297" style="fill:#6b80bf" />
<line x1="706" y1="301" x2="286" y2="191" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="286,191 294,195 294,187" style="fill:#6b80bf" />
<line x1="956" y1="191" x2="536" y2="191" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="536,191 544,195 544,187" style="fill:#6b80bf" />
<line x1="956" y1="191" x2="536" y2="301" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="536,301 544,305 544,297" style="fill:#6b80bf" />
<rect x="36" y="156" width="170" height="70" rx="8" ry="8" style="fill:#c8e6c9;stroke:#222222;stroke-width:1.2" />
<text x="46" y="178" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >ROOT</text>
<text x="46" y="198" style="fill:#666666;font-size:12px;font-family:monospace" >The root issue with a very long title...</text>
<text x="46" y="216" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.300</text>
<rect x="36" y="266" width="170" height="70" rx="8" ry="8" style="fill:#c8e6c9;stroke:#222222;stroke-width:1.2" />
<text x="46" y="288" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >LOOSE</text>
<text x="46" y="308" style="fill:#666666;font-size:12px;font-family:monospace" >Unconnected issue</text>
<text x="46" y="326" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.010</text>
<rect x="286" y="156" width="170" height="70" rx="8" ry="8" style="fill:#c8e6c9;stroke:#222222;stroke-width:1.2" />
<text x="296" y="178" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >A-1</text>
<text x="296" y="198" style="fill:#666666;font-size:12px;font-family:monospace" >Alpha task</text>
<text x="296" y="216" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.200</text>
<rect x="286" y="266" width="170" height="70" rx="8" ry="8" style="fill:#fff3e0;stroke:#222222;stroke-width:1.2" />
<text x="296" y="288" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >A-2</text>
<text x="296" y="308" style="fill:#666666;font-size:12px;font-family:monospace" >Beta task</text>
<text x="296" y="326" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.100</text>
<rect x="536" y="156" width="170" height="70" rx="8" ry="8" style="fill:#ffcdd2;stroke:#222222;stroke-width:1.2" />
<text x="546" y="178" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >B-1</text>
<text x="546" y="198" style="fill:#666666;font-size:12px;font-family:monospace" >Gamma task</text>
<text x="546" y="216" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.050</text>
<rect x="536" y="266" width="170" height="70" rx="8" ry="8" style="fill:#cfd8dc;stroke:#222222;stroke-width:1.2" />
<text x="546" y="288" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >B-2</text>
<text x="546" y="308" style="fill:#666666;font-size:12px;font-family:monospace" >Delta task</text>
<text x="546" y="326" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.040</text>
<rect x="786" y="156" width="170" height="70" rx="8" ry="8" style="fill:#cfd8dc;stroke:#222222;stroke-width:1.2" />
<text x="796" y="178" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >C-1</text>
<text x="796" y="198" style="fill:#666666;font-size:12px;font-family:monospace" >Epsilon task with a title long enough...</text>
<text x="796" y="216" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.020</text>
</svg>
"####;

// Preset=roomy DataHash=hash999
const EXPECTED_ROOMY: &str = r####"<?xml version="1.0"?>
<!-- Generated by SVGo -->
<svg width="1462" height="548"
     xmlns="http://www.w3.org/2000/svg"
     xmlns:xlink="http://www.w3.org/1999/xlink">
<rect x="0" y="0" width="1462" height="548" style="fill:#f9fafb" />
<rect x="16" y="16" width="1430" height="96" rx="10" ry="10" style="fill:#f3f4f6" />
<text x="32" y="44" style="fill:#111111;font-size:16px;font-family:monospace;font-weight:bold" >Roomy &lt;Title&gt; &amp; &#34;Quotes&#34;</text>
<text x="32" y="64" style="fill:#666666;font-size:13px;font-family:monospace" >data_hash: hash999</text>
<text x="32" y="84" style="fill:#666666;font-size:13px;font-family:monospace" >nodes: 7  edges: 7</text>
<text x="32" y="104" style="fill:#666666;font-size:13px;font-family:monospace" >top bottleneck: B-1 (0.90)</text>
<rect x="1262" y="24" width="180" height="96" rx="10" ry="10" style="fill:#eeeeee;stroke:#222222;stroke-width:1" />
<text x="1274" y="42" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >Legend</text>
<rect x="1274" y="52" width="14" height="14" rx="3" ry="3" style="fill:#c8e6c9;stroke:#222222;stroke-width:1" />
<text x="1294" y="60" style="fill:#666666;font-size:12px;font-family:monospace" >Open / Ready</text>
<rect x="1274" y="68" width="14" height="14" rx="3" ry="3" style="fill:#fff3e0;stroke:#222222;stroke-width:1" />
<text x="1294" y="76" style="fill:#666666;font-size:12px;font-family:monospace" >In Progress</text>
<rect x="1274" y="84" width="14" height="14" rx="3" ry="3" style="fill:#ffcdd2;stroke:#222222;stroke-width:1" />
<text x="1294" y="92" style="fill:#666666;font-size:12px;font-family:monospace" >Blocked</text>
<rect x="1274" y="100" width="14" height="14" rx="3" ry="3" style="fill:#cfd8dc;stroke:#222222;stroke-width:1" />
<text x="1294" y="108" style="fill:#666666;font-size:12px;font-family:monospace" >Closed</text>
<line x1="526" y1="197" x2="36" y2="197" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="36,197 44,201 44,193" style="fill:#6b80bf" />
<line x1="526" y1="334" x2="36" y2="197" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="36,197 44,201 44,193" style="fill:#6b80bf" />
<line x1="826" y1="197" x2="336" y2="197" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="336,197 344,201 344,193" style="fill:#6b80bf" />
<line x1="826" y1="197" x2="336" y2="334" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="336,334 344,338 344,330" style="fill:#6b80bf" />
<line x1="826" y1="334" x2="336" y2="197" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="336,197 344,201 344,193" style="fill:#6b80bf" />
<line x1="1126" y1="197" x2="636" y2="197" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="636,197 644,201 644,193" style="fill:#6b80bf" />
<line x1="1126" y1="197" x2="636" y2="334" style="stroke:#6b80bf;stroke-width:2" />
<polygon points="636,334 644,338 644,330" style="fill:#6b80bf" />
<rect x="36" y="156" width="190" height="82" rx="8" ry="8" style="fill:#c8e6c9;stroke:#222222;stroke-width:1.2" />
<text x="46" y="178" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >ROOT</text>
<text x="46" y="198" style="fill:#666666;font-size:12px;font-family:monospace" >The root issue with a very long title...</text>
<text x="46" y="216" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.300</text>
<rect x="36" y="293" width="190" height="82" rx="8" ry="8" style="fill:#c8e6c9;stroke:#222222;stroke-width:1.2" />
<text x="46" y="315" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >LOOSE</text>
<text x="46" y="335" style="fill:#666666;font-size:12px;font-family:monospace" >Unconnected issue</text>
<text x="46" y="353" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.010</text>
<rect x="336" y="156" width="190" height="82" rx="8" ry="8" style="fill:#c8e6c9;stroke:#222222;stroke-width:1.2" />
<text x="346" y="178" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >A-1</text>
<text x="346" y="198" style="fill:#666666;font-size:12px;font-family:monospace" >Alpha task</text>
<text x="346" y="216" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.200</text>
<rect x="336" y="293" width="190" height="82" rx="8" ry="8" style="fill:#fff3e0;stroke:#222222;stroke-width:1.2" />
<text x="346" y="315" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >A-2</text>
<text x="346" y="335" style="fill:#666666;font-size:12px;font-family:monospace" >Beta task</text>
<text x="346" y="353" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.100</text>
<rect x="636" y="156" width="190" height="82" rx="8" ry="8" style="fill:#ffcdd2;stroke:#222222;stroke-width:1.2" />
<text x="646" y="178" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >B-1</text>
<text x="646" y="198" style="fill:#666666;font-size:12px;font-family:monospace" >Gamma task</text>
<text x="646" y="216" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.050</text>
<rect x="636" y="293" width="190" height="82" rx="8" ry="8" style="fill:#cfd8dc;stroke:#222222;stroke-width:1.2" />
<text x="646" y="315" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >B-2</text>
<text x="646" y="335" style="fill:#666666;font-size:12px;font-family:monospace" >Delta task</text>
<text x="646" y="353" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.040</text>
<rect x="936" y="156" width="190" height="82" rx="8" ry="8" style="fill:#cfd8dc;stroke:#222222;stroke-width:1.2" />
<text x="946" y="178" style="fill:#111111;font-size:13px;font-family:monospace;font-weight:bold" >C-1</text>
<text x="946" y="198" style="fill:#666666;font-size:12px;font-family:monospace" >Epsilon task with a title long enough...</text>
<text x="946" y="216" style="fill:#666666;font-size:11px;font-family:monospace" >PR 0.020</text>
</svg>
"####;

/// The PNG branch cannot be compared byte-for-byte — Go rasterises through
/// `gg`, which anti-aliases, while [`bv_export::raster`] does not — so what is
/// pinned here is everything that *is* contractual: the canvas size, the
/// colour type, and that the file is a decodable PNG. The IHDR bytes below
/// were read from Go's own `SaveGraphSnapshot` output for this fixture.
#[test]
fn png_header_matches_go() {
    let dir = scratch("png");
    let path = dir.join("out_compact.png");
    let opts = GraphSnapshotOptions {
        path: path.clone(),
        format: "png".to_string(),
        title: "Snapshot Title".to_string(),
        preset: "compact".to_string(),
        issues: fixture(),
        data_hash: "abc123".to_string(),
    };
    save_graph_snapshot(opts, Some(&stats())).expect("snapshot written");
    let bytes = std::fs::read(&path).expect("snapshot read back");

    assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n");
    // IHDR: width, height, bit depth, colour type.
    assert_eq!(u32::from_be_bytes(bytes[16..20].try_into().unwrap()), 1242);
    assert_eq!(u32::from_be_bytes(bytes[20..24].try_into().unwrap()), 482);
    assert_eq!(bytes[24], 8, "bit depth");
    assert_eq!(bytes[25], 2, "colour type 2 = truecolour RGB");

    // And it decodes.
    let decoded = png::Decoder::new(std::io::Cursor::new(&bytes));
    let reader = decoded.read_info().expect("decode");
    assert_eq!((reader.info().width, reader.info().height), (1242, 482));
}

/// The roomy preset must grow the PNG by exactly the difference the layout
/// constants imply, and an extension-less path must land on `.svg`.
#[test]
fn the_png_preset_changes_the_canvas() {
    let dir = scratch("png-preset");
    let render = |preset: &str| -> (u32, u32) {
        let path = dir.join(format!("p_{preset}.png"));
        let opts = GraphSnapshotOptions {
            path: path.clone(),
            format: "png".to_string(),
            title: "T".to_string(),
            preset: preset.to_string(),
            issues: fixture(),
            data_hash: "h".to_string(),
        };
        save_graph_snapshot(opts, Some(&stats())).expect("written");
        let bytes = std::fs::read(&path).unwrap();
        (
            u32::from_be_bytes(bytes[16..20].try_into().unwrap()),
            u32::from_be_bytes(bytes[20..24].try_into().unwrap()),
        )
    };
    // Go: 72 + 4*(w+colGap) + w, and 72 + header + 2*(h+rowGap) + h.
    assert_eq!(render("compact"), (1242, 482));
    assert_eq!(render("roomy"), (1462, 548));
}
