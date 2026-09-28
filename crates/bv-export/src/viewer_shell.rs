//! The dashboard's document and stylesheet: the two hand-written files in the
//! exported bundle.
//!
//! Neither is a port. Go ships both as `//go:embed viewer_assets` entries and
//! writes them out through the same `os.WriteFile` as the vendored libraries
//! (`pkg/export/viewer_embed.go:86`). What Rust has to reproduce is the single
//! special case in that walk — the `if relPath == "index.html"` branch at
//! `viewer_embed.go:67` — and nothing else.
//!
//! Two consequences are worth stating plainly, because both are easy to
//! "improve" into a divergence:
//!
//! * `index.html` is a **static file**, not a template. There is no
//!   `text/template` anywhere near it, and no data is interpolated into it: the
//!   303412-byte asset ships with zero placeholders. The dashboard fetches
//!   `beads.sqlite3` and `data/*.json` at runtime (`viewer.js:643`,
//!   `viewer.js:2751`). So a template engine here would be inventing a
//!   mechanism the oracle does not have.
//!
//! * The `h1` half of `replaceTitle` matches nothing. Go's needle is
//!   `<h1 class="text-xl font-semibold">Beads Viewer</h1>` (`viewer_embed.go:187`)
//!   but the asset's heading is
//!   `<h1 class="text-lg sm:text-xl font-semibold">Beads Viewer</h1>`
//!   (line 193). `strings.Replace` with `n=1` on an absent needle is a no-op, so
//!   a non-empty `--pages-title` rewrites the `<title>` and leaves the heading
//!   alone. Verified against the oracle: exporting with
//!   `--pages-title 'A&B<C>D"E'F'` emits
//!   `<title>A&amp;B&lt;C&gt;D&#34;E&#39;F</title>` while line 193 still
//!   reads `Beads Viewer`. The needle is kept verbatim so that a future
//!   "correct" h1 replacement has to be a deliberate edit rather than a
//!   drive-by that silently diverges.

use std::path::Path;

/// The dashboard document, before any transform.
///
/// `include_bytes!` rather than `include_str!` so no text-level normalisation can
/// touch it; Go hands `os.WriteFile` a `[]byte` read straight from the embed FS.
pub const INDEX_HTML: &[u8] = include_bytes!("../assets/viewer_assets/index.html");

/// The stylesheet, written byte-for-byte.
///
/// Carries the two `@font-face` declarations that pull `vendor/inter-variable.woff2`
/// and `vendor/jetbrains-mono-regular.woff2` over `url()`, which is why those two
/// files are part of the bundle without appearing in any script tag.
pub const STYLES_CSS: &[u8] = include_bytes!("../assets/viewer_assets/styles.css");

/// The scripts `AddScriptCacheBusting` stamps, in Go's order
/// (`viewer_embed.go:201`).
///
/// `graph.js` is in the list but has **no** `src=` attribute in the asset —
/// `viewer.js:2913` loads it with a dynamic `import('./graph.js')`, which
/// neither matches the needle nor wants a query string on it. Listing it is
/// faithful to the oracle and costs nothing; assuming it appears would not.
const CACHE_BUST_JS_FILES: &[&str] = &[
    "head_init.js",
    "viewer.js",
    "charts.js",
    "graph.js",
    "hybrid_scorer.js",
    "wasm_loader.js",
];

/// Go's `<title>` needle.
const TITLE_NEEDLE: &str = "<title>Beads Viewer</title>";

/// Go's `<h1>` needle, which no longer occurs in the asset — see the module
/// docs. Kept so the port performs the same search the oracle does.
const H1_NEEDLE: &str = r#"<h1 class="text-xl font-semibold">Beads Viewer</h1>"#;

/// Writes `styles.css` and `index.html` into `out_dir`.
///
/// `title` is the `--pages-title` value; empty means the asset's own strings
/// are left alone. `now` supplies the cache-buster, so the caller keeps control
/// of the clock (the rest of the export path pins one via `SOURCE_DATE_EPOCH`).
///
/// Must run before the offline manifest is bound: the worker hashes
/// `index.html`, so it has to be the transformed document that gets hashed, as
/// in Go's walk-then-bind order.
pub fn copy_viewer_shell(out_dir: &Path, title: &str, now: jiff::Timestamp) -> Result<(), String> {
    write_asset(out_dir, "styles.css", STYLES_CSS)?;
    let index = render_index_html(title, now);
    write_asset(out_dir, "index.html", index.as_bytes())
}

fn write_asset(out_dir: &Path, rel: &str, bytes: &[u8]) -> Result<(), String> {
    let dest = out_dir.join(rel);
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("creating {}: {e}", parent.display()))?;
    }
    std::fs::write(&dest, bytes).map_err(|e| format!("writing {}: {e}", dest.display()))
}

/// Port of `CopyEmbeddedAssets`'s `index.html` branch: `replaceTitle`, then
/// `AddScriptCacheBusting`, in that order (`viewer_embed.go:67-75`).
///
/// The result is never byte-stable across runs — the cache-buster is a unix
/// timestamp — so the way to test this is the invariant that *nothing else*
/// changed, not a frozen blob. `only_the_cache_busters_differ` asserts exactly
/// that.
pub fn render_index_html(title: &str, now: jiff::Timestamp) -> String {
    let asset = String::from_utf8_lossy(INDEX_HTML).into_owned();
    let retitled = if title.is_empty() {
        asset
    } else {
        replace_title(asset, title)
    };
    add_script_cache_busting(retitled, now)
}

/// Port of `replaceTitle` (`viewer_embed.go:175`).
///
/// `strings.Replace(..., 1)` on each needle independently, on the original
/// string rather than on the result of the other replacement.
fn replace_title(content: String, title: &str) -> String {
    if title.is_empty() {
        return content;
    }
    // Go `html.EscapeString`, not the CLI's `html_escape`: the stdlib escapes
    // five characters, encodes `"` as `&#34;` and `'` as `&#39;`, and does the
    // whole thing in one pass. `main.rs::html_escape` covers four of the five
    // and renders `"` as `&quot;`, so reusing it would diverge on any title
    // containing a quote.
    let safe = html_escape_go(title);
    let with_title = content.replacen(TITLE_NEEDLE, &format!("<title>{safe}</title>"), 1);
    with_title.replacen(
        H1_NEEDLE,
        &format!(r#"<h1 class="text-xl font-semibold">{safe}</h1>"#),
        1,
    )
}

/// Go `html.EscapeString` (`html/escape.go`): `&`→`&amp;`, `'`→`&#39;`,
/// `<`→`&lt;`, `>`→`&gt;`, `"`→`&#34;`.
///
/// Built as a single pass over the input. A chain of `str::replace` calls would
/// be wrong twice over: it leaves `'` unescaped, and it re-escapes the `&` that
/// its own earlier replacements introduced. Go's `strings.NewReplacer` matches
/// the input once and does not re-examine what it wrote.
fn html_escape_go(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '\'' => out.push_str("&#39;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            c => out.push(c),
        }
    }
    out
}

/// Port of `AddScriptCacheBusting` (`viewer_embed.go:196`).
///
/// Every occurrence of both `src="X"` and `src='X'`, for all six names.
fn add_script_cache_busting(content: String, now: jiff::Timestamp) -> String {
    let buster = format!("?v={}", now.as_second());
    let mut out = content;
    for js in CACHE_BUST_JS_FILES {
        out = out.replace(&format!("src=\"{js}\""), &format!("src=\"{js}{buster}\""));
        out = out.replace(&format!("src='{js}'"), &format!("src='{js}{buster}'"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digests of Go's source assets, measured with `shasum -a 256`. The
    /// stylesheet's is also the digest of Go's *emitted* copy, because
    /// `styles.css` takes the walk's default branch and is never transformed.
    const INDEX_SHA256: &str = "652ca4c658af57586c8ff7b9774114935450baf1fdf4126eca68f1f59d3a9a6a";
    const STYLES_SHA256: &str = "ab65816834232900eb2a065afced3475bae6a5e5b98aad849bcf0ea622499d2b";

    /// Same sha256, recomputed here with the crate's own dependency, so the
    /// constant above and the bytes actually embedded cannot drift apart.
    fn sha256_hex(bytes: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn at(secs: i64) -> jiff::Timestamp {
        jiff::Timestamp::from_second(secs).unwrap()
    }

    #[test]
    fn assets_are_byte_identical_to_the_go_source() {
        assert_eq!(INDEX_HTML.len(), 303412, "index.html size");
        assert_eq!(STYLES_CSS.len(), 50610, "styles.css size");
        assert_eq!(sha256_hex(INDEX_HTML), INDEX_SHA256, "index.html sha256");
        assert_eq!(sha256_hex(STYLES_CSS), STYLES_SHA256, "styles.css sha256");
    }

    /// The output carries a unix timestamp, so it cannot be pinned. What *can*
    /// be pinned is that the only edits are the cache-busters: un-stamping every
    /// one of them must return the asset byte-for-byte.
    #[test]
    fn only_the_cache_busters_differ() {
        let mut unstamped = render_index_html("", at(1_790_606_544));
        for js in CACHE_BUST_JS_FILES {
            unstamped = unstamped.replace(
                &format!("src=\"{js}?v=1790606544\""),
                &format!("src=\"{js}\""),
            );
        }
        assert_eq!(unstamped, String::from_utf8_lossy(INDEX_HTML));
    }

    /// Five script tags carry a buster; `graph.js` has no `src=` attribute at
    /// all, and neither the vendor bundles nor the stylesheet's `url()` fonts
    /// are touched. Measured against the oracle: 5 changed lines, +65 bytes.
    #[test]
    fn exactly_five_script_tags_are_stamped() {
        let rendered = render_index_html("", at(1_790_606_544));
        assert_eq!(
            rendered.matches("?v=1790606544").count(),
            5,
            "stamped script tags"
        );
        // 5 x len("?v=1790606544") = 65.
        assert_eq!(
            rendered.len() as i64 - INDEX_HTML.len() as i64,
            65,
            "net byte delta"
        );
        assert!(!rendered.contains("src=\"graph.js"), "graph.js has no src=");
    }

    #[test]
    fn title_escaping_matches_go_stdlib() {
        let rendered = render_index_html("A&B<C>D\"E'F", at(1_790_606_544));
        assert!(
            rendered.contains("<title>A&amp;B&lt;C&gt;D&#34;E&#39;F</title>"),
            "escaped title missing"
        );
    }

    /// The oracle's own h1 needle does not occur in the asset, so its
    /// replacement is a no-op and a retitled export keeps the stock heading.
    /// Pinned because "fixing" it is the obvious, wrong move.
    #[test]
    fn the_heading_is_not_retitled() {
        assert!(!String::from_utf8_lossy(INDEX_HTML).contains(H1_NEEDLE));
        let rendered = render_index_html("My Dashboard", at(1_790_606_544));
        assert!(rendered.contains("<title>My Dashboard</title>"));
        assert!(
            rendered.contains(r#"<h1 class="text-lg sm:text-xl font-semibold">Beads Viewer</h1>"#)
        );
    }

    /// Go's replacer runs in one pass, so an ampersand the caller supplied is
    /// escaped once and not re-escaped by the entity it became.
    #[test]
    fn escaping_does_not_double_escape() {
        assert_eq!(html_escape_go("&lt;"), "&amp;lt;");
        assert_eq!(html_escape_go("a<b>&\"'"), "a&lt;b&gt;&amp;&#34;&#39;");
    }

    #[test]
    fn an_empty_title_leaves_the_document_untouched_apart_from_busters() {
        let rendered = render_index_html("", at(0));
        assert!(rendered.contains(TITLE_NEEDLE));
    }
}
