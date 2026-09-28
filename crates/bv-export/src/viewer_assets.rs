//! The application JavaScript of the static site, and the offline service
//! worker that binds a browser cache to one exact export.
//!
//! Go ships these as a `//go:embed viewer_assets` tree and copies it out with
//! a single `os.WriteFile` per file (`pkg/export/viewer_embed.go:86`). Six of
//! the eight files here are therefore not a port at all — they are the same
//! bytes, carried over. The seventh, `coi-serviceworker.js`, is generated:
//! `bindOfflineAssets` (`viewer_embed.go:133`) injects the bundle's file
//! hashes into a template before writing it. Reproducing the generator is the
//! only real work in this module; everything else is fidelity.

use std::path::Path;

use sha2::{Digest, Sha256};

/// The application scripts, copied verbatim from Go's `viewer_assets`.
///
/// `include_bytes!` rather than `include_str!`: rustc normalises CRLF to LF
/// when it reads a source file, and although all eight of these are already
/// pure LF (measured: zero CR bytes each), reading them as bytes removes the
/// question entirely. Go also hands `os.WriteFile` a `[]byte`.
pub const APPLICATION_ASSETS: &[(&str, &[u8])] = &[
    (
        "viewer.js",
        include_bytes!("../assets/viewer_assets/viewer.js"),
    ),
    (
        "graph.js",
        include_bytes!("../assets/viewer_assets/graph.js"),
    ),
    (
        "charts.js",
        include_bytes!("../assets/viewer_assets/charts.js"),
    ),
    (
        "hybrid_scorer.js",
        include_bytes!("../assets/viewer_assets/hybrid_scorer.js"),
    ),
    (
        "head_init.js",
        include_bytes!("../assets/viewer_assets/head_init.js"),
    ),
    (
        "wasm_loader.js",
        include_bytes!("../assets/viewer_assets/wasm_loader.js"),
    ),
    // Written in its template form here and overwritten with the bound form by
    // `bind_offline_assets`, in that order, exactly as Go's walk-then-bind does.
    // If binding ever fails the bundle keeps the empty manifest, and the worker
    // refuses to install rather than caching a partial site.
    (
        "coi-serviceworker.js",
        include_bytes!("../assets/viewer_assets/coi-serviceworker.js"),
    ),
    (
        "vendor/bv_graph.js",
        include_bytes!("../assets/viewer_assets/vendor/bv_graph.js"),
    ),
    (
        "vendor/bv_graph_bg.wasm",
        include_bytes!("../assets/viewer_assets/vendor/bv_graph_bg.wasm"),
    ),
];

/// The worker template, before the offline manifest is injected.
const COI_TEMPLATE: &[u8] = include_bytes!("../assets/viewer_assets/coi-serviceworker.js");

/// Every file Go's embed walk would leave in the bundle, minus the worker
/// itself — the one path the walk appends to `offlineFiles` conditionally
/// (`viewer_embed.go:80`).
///
/// `graph-demo.html` and `hybrid_scorer.test.js` are absent because
/// `isDevOnlyAsset` (`viewer_embed.go:165`) drops them: the demo pulls
/// third-party scripts from bare CDNs with no SRI, which would run on the
/// exported dashboard's origin next to the exported database.
///
/// The list spans assets this crate does not own — `index.html`, `styles.css`
/// and the other vendored bundles come from sibling modules. It is here rather
/// than assembled from whatever `APPLICATION_ASSETS` happens to contain because
/// the manifest is a property of the *bundle*, not of one contributor: a file
/// another module wrote is offline-cached exactly like a file this one did.
const VIEWER_ASSET_PATHS: &[&str] = &[
    "charts.js",
    "graph.js",
    "head_init.js",
    "hybrid_scorer.js",
    "index.html",
    "styles.css",
    "viewer.js",
    "wasm_loader.js",
    "vendor/MANIFEST.json",
    "vendor/alpine-collapse.min.js",
    "vendor/alpine.min.js",
    "vendor/bv_graph.js",
    "vendor/bv_graph_bg.wasm",
    "vendor/chart.umd.min.js",
    "vendor/d3.v7.min.js",
    "vendor/dompurify.min.js",
    "vendor/force-graph.min.js",
    "vendor/inter-variable.woff2",
    "vendor/jetbrains-mono-regular.woff2",
    "vendor/marked.min.js",
    "vendor/mermaid.min.js",
    "vendor/sql-wasm.js",
    "vendor/sql-wasm.wasm",
    "vendor/tailwindcss.js",
];

/// Bundle paths Go walks to find the database side of the offline cache
/// (`viewer_embed.go:94`).
///
/// All four are walked with `filepath.WalkDir`, which visits a plain file's own
/// root entry — so `beads.sqlite3` is cached as itself, not skipped as "not a
/// directory". A walker that only descends directories loses it.
const OFFLINE_DISK_ROOTS: &[&str] = &[
    "beads.sqlite3",
    "beads.sqlite3.config.json",
    "chunks",
    "data",
];

/// Copies this module's assets into `out_dir` and then binds the offline
/// manifest to what is actually on disk.
pub fn copy_viewer_assets(out_dir: &Path) -> Result<(), String> {
    copy_application_assets(out_dir)?;
    bind_offline_assets(out_dir)
}

/// Writes the application scripts under `out_dir`, creating `vendor/` as needed.
pub fn copy_application_assets(out_dir: &Path) -> Result<(), String> {
    for (rel, bytes) in APPLICATION_ASSETS {
        let dest = out_dir.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        std::fs::write(&dest, bytes).map_err(|e| format!("writing {}: {e}", dest.display()))?;
    }
    Ok(())
}

/// Binds the worker's offline manifest to the bundle at `out_dir`.
///
/// Port of `bindOfflineAssets` (`pkg/export/viewer_embed.go:133`). The manifest
/// exists so that a changed or missing asset stops the worker installing at
/// all: it can precache a complete bundle or none, never a partial one.
///
/// Two details are load-bearing and easy to get wrong:
///
/// * `data/history.json` is written *after* this runs — Go writes it at
///   `cmd/bv/main.go:3181`, well after `copyViewerAssets` at `:3168` — so the
///   real manifest has 30 entries and no `history.json`. Its four sibling
///   `data/` files are written by the database exporter and *are* present.
///   Calling this after the history write silently adds a 31st entry.
///
/// * `coi-serviceworker.js` output is not byte-stable across runs. The
///   manifest hashes `index.html`, whose script tags carry a per-run
///   `?v=<unix timestamp>` cache-buster (`AddScriptCacheBusting`,
///   `viewer_embed.go:196`), so its digest moves every export. Compare the
///   algorithm, never a frozen blob.
pub fn bind_offline_assets(out_dir: &Path) -> Result<(), String> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut absent: Vec<&str> = Vec::new();

    for rel in VIEWER_ASSET_PATHS {
        match read_asset(out_dir, rel) {
            Ok(hash) => entries.push(((*rel).to_string(), hash)),
            // Go cannot reach this branch: it has just written every one of
            // these files. It is reachable here because the bundle is assembled
            // by several modules, and it is reported rather than swallowed —
            // an asset missing from the manifest is one the offline cache will
            // not hold.
            Err(AssetError::Absent) => absent.push(rel),
            Err(AssetError::Io(e)) => return Err(format!("hashing offline asset {rel}: {e}")),
        }
    }

    for root in OFFLINE_DISK_ROOTS {
        let base = out_dir.join(root);
        match std::fs::symlink_metadata(&base) {
            // Go continues past a missing root so that standalone asset
            // callers, which have no database, still get a worker.
            Err(_) => continue,
            Ok(meta) if meta.is_dir() => collect_tree(&base, out_dir, &mut entries)?,
            // `filepath.WalkDir` on a plain file yields that file.
            Ok(_) => match read_asset(out_dir, root) {
                Ok(hash) => entries.push(((*root).to_string(), hash)),
                Err(AssetError::Absent) => {}
                Err(AssetError::Io(e)) => return Err(format!("hashing offline asset {root}: {e}")),
            },
        }
    }

    if !absent.is_empty() {
        println!(
            "  → Warning: offline manifest skipped {} absent asset(s): {}",
            absent.len(),
            absent.join(", ")
        );
    }

    // `sort.Strings` is a bytewise compare, which `String`'s `Ord` also is.
    // It is not a locale collation: `vendor/MANIFEST.json` sorts before
    // `vendor/alpine.min.js` because `M` (0x4d) precedes `a` (0x61).
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let encoded = marshal_assets(&entries);
    let content = String::from_utf8_lossy(COI_TEMPLATE).into_owned();
    let content = replace_first(
        &content,
        "const OFFLINE_ASSETS = [];",
        &format!("const OFFLINE_ASSETS = {encoded};"),
    );
    // Go replaces the single-quoted form here (`viewer_embed.go:159`) while the
    // template carries the double-quoted one (`coi-serviceworker.js:15`), so
    // the substitution never fires and `"development"` ships verbatim. That is
    // an upstream bug, not a reading of the source: `git show HEAD:` shows the
    // same mismatch at the parity commit, and two Go runs both emit
    // `"development"`. The needle is kept single-quoted so the port follows Go
    // rather than quietly correcting it — writing a real digest here would
    // diverge from the oracle.
    let revision = {
        let mut hash = Sha256::new();
        hash.update(COI_TEMPLATE);
        hash.update(encoded.as_bytes());
        hex(&hash.finalize())
    };
    let content = replace_first(
        &content,
        "const CACHE_REVISION = 'development';",
        &format!("const CACHE_REVISION = '{revision}';"),
    );

    std::fs::write(out_dir.join("coi-serviceworker.js"), content)
        .map_err(|e| format!("writing coi-serviceworker.js: {e}"))
}

enum AssetError {
    Absent,
    Io(std::io::Error),
}

fn read_asset(out_dir: &Path, rel: &str) -> Result<String, AssetError> {
    match std::fs::read(out_dir.join(rel)) {
        Ok(data) => Ok(sha256_hex(&data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(AssetError::Absent),
        Err(e) => Err(AssetError::Io(e)),
    }
}

/// Appends every file under `dir` to `entries` as a bundle-relative,
/// forward-slashed path.
fn collect_tree(
    dir: &Path,
    out_dir: &Path,
    entries: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let walk = std::fs::read_dir(dir).map_err(|e| format!("walking {}: {e}", dir.display()))?;
    for entry in walk {
        let entry = entry.map_err(|e| format!("walking {}: {e}", dir.display()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_tree(&path, out_dir, entries)?;
            continue;
        }
        let rel = path
            .strip_prefix(out_dir)
            .map_err(|e| format!("relativising {}: {e}", path.display()))?;
        // `embed.FS` paths are already forward-slashed and Go feeds them to
        // `filepath.ToSlash` before use, so a separator swap cannot survive
        // into the manifest. This keeps the same guarantee on Windows.
        let rel = rel.to_string_lossy().replace('\\', "/");
        let hash = read_asset(out_dir, &rel).map_err(|e| match e {
            AssetError::Absent => format!("hashing offline asset {rel}: missing"),
            AssetError::Io(e) => format!("hashing offline asset {rel}: {e}"),
        })?;
        entries.push((rel, hash));
    }
    Ok(())
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(data);
    hex(&hash.finalize())
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap());
        out.push(char::from_digit(u32::from(byte & 0x0f), 16).unwrap());
    }
    out
}

/// Renders the manifest the way `json.Marshal` renders `[]asset` with
/// `struct { Path, Hash string }` — compact, no spaces, `path` before
/// `sha256`.
///
/// Go's encoder HTML-escapes `<`, `>` and `&` by default; `serde_json` does
/// not, so the escaping is done here rather than delegated. Bundle paths are
/// `[A-Za-z0-9._/-]` and never contain them, but the manifest is hashed into
/// `CACHE_REVISION`, so it is not a place to leave a divergence implicit.
fn marshal_assets(entries: &[(String, String)]) -> String {
    let mut out = String::from("[");
    for (i, (path, hash)) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"path\":\"");
        escape_json_string(&mut out, path);
        out.push_str("\",\"sha256\":\"");
        out.push_str(hash);
        out.push_str("\"}");
    }
    out.push(']');
    out
}

fn escape_json_string(out: &mut String, value: &str) {
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

/// `strings.Replace(s, old, new, 1)`: substitutes the first occurrence and
/// returns `s` untouched when `old` is absent.
fn replace_first(haystack: &str, needle: &str, replacement: &str) -> String {
    match haystack.find(needle) {
        Some(at) => {
            let mut out = String::with_capacity(haystack.len() + replacement.len());
            out.push_str(&haystack[..at]);
            out.push_str(replacement);
            out.push_str(&haystack[at + needle.len()..]);
            out
        }
        None => haystack.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digests the Go oracle at `18afafa` emits for each copied file. They
    /// are measured, not transcribed: `cmp` against a real
    /// `bv --export-pages` run, then `shasum -a 256` on the result.
    const EXPECTED: &[(&str, usize, &str)] = &[
        (
            "viewer.js",
            110701,
            "3d457c9fd388786546bad240bb6bd38faf612d9877a24741c874383204b43df6",
        ),
        (
            "graph.js",
            127092,
            "9ad4661e9df98afa5c5d0b9568d19b5f16023038aac597288d6322756c044293",
        ),
        (
            "charts.js",
            21159,
            "14fc7076a6965b498cd24496c8a8193e5718da0ddb1ccb4abaa875ee46af7d44",
        ),
        (
            "hybrid_scorer.js",
            2944,
            "427ce7fa8e6ac0d8cb1ecdf17a3879f21c81b3bb5d22c22ee4e65f0fc5ac365f",
        ),
        (
            "head_init.js",
            3540,
            "739397d4a273e94c6649fa2ffcaa3bdd21fc0dae093e636a1bb1cb1be87ce87b",
        ),
        (
            "wasm_loader.js",
            4205,
            "f19e82f59b2371c37283dc3762d91d1b18c72bf1894d127f6e1c7ce9928b7d13",
        ),
        (
            "coi-serviceworker.js",
            6532,
            "5339b066f0a97aa00bf55a962abb778a94211cd437d2dff4171aff56286bf4c0",
        ),
        (
            "vendor/bv_graph.js",
            31915,
            "51d98027ed7e10befea35fcbb5a589c744dab9be43fb38fef9758d7deefbca12",
        ),
        (
            "vendor/bv_graph_bg.wasm",
            216094,
            "833799c32a00ba2ae16aa6caced25347c709b4b8be2ba9661074d8bd7468076e",
        ),
    ];

    #[test]
    fn embedded_assets_match_the_go_oracle() {
        for (rel, size, digest) in EXPECTED {
            let embedded = APPLICATION_ASSETS
                .iter()
                .find(|(name, _)| name == rel)
                .unwrap_or_else(|| panic!("{rel} is not embedded"));
            assert_eq!(embedded.1.len(), *size, "{rel} size");
            assert_eq!(&sha256_hex(embedded.1), digest, "{rel} sha256");
        }
    }

    #[test]
    fn every_application_asset_is_in_the_offline_manifest() {
        // The worker is written by hand and deliberately absent from the list
        // it precaches; everything else the module writes must be cached.
        for (rel, _) in APPLICATION_ASSETS {
            if *rel == "coi-serviceworker.js" {
                continue;
            }
            assert!(
                VIEWER_ASSET_PATHS.contains(rel),
                "{rel} would ship without an offline entry"
            );
        }
    }

    #[test]
    fn dev_only_assets_stay_out_of_the_manifest() {
        // `isDevOnlyAsset`, viewer_embed.go:165.
        for rel in ["graph-demo.html", "hybrid_scorer.test.js"] {
            assert!(!VIEWER_ASSET_PATHS.contains(&rel), "{rel} is dev-only");
        }
    }

    #[test]
    fn marshal_assets_is_compact_and_path_first() {
        let entries = vec![
            ("b".to_string(), "bb".to_string()),
            ("a".to_string(), "aa".to_string()),
        ];
        assert_eq!(
            marshal_assets(&entries),
            r#"[{"path":"b","sha256":"bb"},{"path":"a","sha256":"aa"}]"#
        );
    }

    #[test]
    fn replace_first_leaves_the_string_alone_when_the_needle_is_absent() {
        assert_eq!(replace_first("abc", "zz", "Q"), "abc");
        assert_eq!(replace_first("aXcX", "X", "Q"), "aQcX");
    }

    #[test]
    fn binding_keeps_the_literal_development_revision() {
        // Go substitutes the single-quoted form while the template carries the
        // double-quoted one, so nothing is replaced. Asserted here so that
        // "fixing" it has to be a deliberate edit to a test, not a drive-by.
        let dir = tempdir("revision");
        for (rel, bytes) in APPLICATION_ASSETS {
            let dest = dir.join(rel);
            std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
            std::fs::write(&dest, bytes).unwrap();
        }
        bind_offline_assets(&dir).unwrap();
        let worker = std::fs::read_to_string(dir.join("coi-serviceworker.js")).unwrap();
        assert!(worker.contains(r#"const CACHE_REVISION = "development";"#));
    }

    #[test]
    fn a_plain_file_root_is_cached_as_itself() {
        // `filepath.WalkDir` on `beads.sqlite3` visits the file, not a
        // directory of that name. A directory-only walker drops it and the
        // manifest loses an entry.
        let dir = tempdir("plain-file-root");
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("beads.sqlite3"), b"sqlite-bytes").unwrap();
        bind_offline_assets(&dir).unwrap();
        let worker = std::fs::read_to_string(dir.join("coi-serviceworker.js")).unwrap();
        assert!(
            worker.contains(r#"{"path":"beads.sqlite3","sha256":"#),
            "manifest: {}",
            worker
                .split("const OFFLINE_ASSETS = ")
                .nth(1)
                .and_then(|s| s.split(';').next())
                .unwrap_or("<none>")
        );
    }

    #[test]
    fn nested_data_files_are_cached() {
        let dir = tempdir("nested-data");
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("data/meta.json"), b"{}").unwrap();
        std::fs::write(dir.join("data/triage.json"), b"[]").unwrap();
        bind_offline_assets(&dir).unwrap();
        let worker = std::fs::read_to_string(dir.join("coi-serviceworker.js")).unwrap();
        for rel in ["data/meta.json", "data/triage.json"] {
            assert!(
                worker.contains(&format!(r#"{{"path":"{rel}","sha256":"#)),
                "{rel}"
            );
        }
    }

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("bvr-viewer-assets-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
