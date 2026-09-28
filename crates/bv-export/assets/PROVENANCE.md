# Vendored assets

Three files in this directory are copied rather than written. Go holds the same
three files, in `pkg/export/` at parity commit `18afafa`; this crate is a port,
so the bytes are ported too. `crates/bv-export/tests/graph_interactive_parity.rs`
pins the rendered document against Go's own output byte-for-byte, which is what
keeps these copies honest.

| File | Source | Licence | Notes |
|---|---|---|---|
| `force-graph.min.js` | `pkg/export/force-graph.min.js` — [vasturiano/force-graph](https://github.com/vasturiano/force-graph) v1.43.5 | MIT | Embedded so the exported page works with no network. |
| `marked.min.js` | `pkg/export/marked.min.js` — [markedjs/marked](https://github.com/markedjs/marked) v14.1.0 | MIT (Copyright 2011-2024 Christopher Jeffrey) | Embedded so issue descriptions render as Markdown with no network. |
| `graph_interactive.html` | the `fmt.Sprintf` template in `pkg/export/graph_render_beautiful.go:15` | same as this repository | The page's markup, stylesheet and UI script. |

## The one edit to `graph_interactive.html`

Go's template is a format string. Its thirteen positional verbs —

```
%s title, %s title, %d nodes, %d edges, %d nodes, %d nodes, %d edges,
%s timestamp, %s hash, %s project, %s force-graph, %s marked, %s data
```

— were replaced, positionally and one for one, with the named sentinels
`@@BV_TITLE@@`, `@@BV_NODES@@`, `@@BV_EDGES@@`, `@@BV_TIMESTAMP@@`,
`@@BV_HASH@@`, `@@BV_PROJECT@@`, `@@BV_FORCE_GRAPH_JS@@`, `@@BV_MARKED_JS@@`
and `@@BV_GRAPH_DATA_JSON@@`.

The reason is that Rust's `format!` would otherwise require doubling all 74
literal percent signs *and* every `{` and `}` in the stylesheet — a
transformation nobody could review. Named sentinels keep the template bytes
verbatim and put the substitution in ordinary Rust, where it is testable. The
37 `%%` escapes are deliberately left in place: `render_html` collapses them,
reproducing what Go's `Sprintf` does, and it does so before substitution so
the graph JSON and the two bundles keep their own percent signs.

## Line endings

The two vendored bundles ship with CRLF line endings (8,684 and 1,675 carriage
returns). **Both were converted to LF.** `rustc` normalises CRLF to LF when it
reads any source file, `include_str!` included, so an LF-normalised copy is
what actually reaches the binary — storing the CRs would leave the file on disk
disagreeing with the compiled artefact. The consequence is that the exported
page differs from Go's by those 10,359 carriage returns, all of them inside
third-party JavaScript where line endings are not significant. This is the one
known byte-level divergence, and it is unavoidable in Rust.

# `viewer_assets/` — the static site's application JavaScript

A second group of copies, in the layout Go's `//go:embed viewer_assets` tree
uses. Go emits all of these with a single `os.WriteFile` per file
(`pkg/export/viewer_embed.go:86`), so seven of the eight are not a port at all —
they are the same bytes, carried over. `crates/bv-export/src/viewer_assets.rs`
embeds them with `include_bytes!` and writes them out unchanged.

| File | Bytes | Source | Licence |
|---|---|---|---|
| `viewer.js` | 110701 | `pkg/export/viewer_assets/viewer.js` | same as this repository |
| `graph.js` | 127092 | `pkg/export/viewer_assets/graph.js` | same as this repository |
| `charts.js` | 21159 | `pkg/export/viewer_assets/charts.js` | same as this repository |
| `hybrid_scorer.js` | 2944 | `pkg/export/viewer_assets/hybrid_scorer.js` | same as this repository |
| `head_init.js` | 3540 | `pkg/export/viewer_assets/head_init.js` | same as this repository |
| `wasm_loader.js` | 4205 | `pkg/export/viewer_assets/wasm_loader.js` | same as this repository |
| `coi-serviceworker.js` | 6532 | `pkg/export/viewer_assets/coi-serviceworker.js` | [nicobrinkkemper/coi-serviceworker](https://github.com/nicobrinkkemper/coi-serviceworker), MIT — a *template*; the 9735-byte export is generated |
| `vendor/bv_graph.js` | 31915 | `pkg/export/viewer_assets/vendor/bv_graph.js` | same as this repository (wasm-bindgen output) |
| `vendor/bv_graph_bg.wasm` | 216094 | `pkg/export/viewer_assets/vendor/bv_graph_bg.wasm` | same as this repository |

All eight are pure LF (zero CR bytes, measured), so unlike the two bundles
above there is no normalisation question at all. They are read as bytes rather
than as `&str` regardless, so the point cannot arise later.

## The WASM pair is copied, never rebuilt

`vendor/bv_graph.{js,_bg.wasm}` are checked-in build artefacts, not inputs.
`vendor/MANIFEST.json` records the toolchain that produced them — rustc
1.100.0-nightly, wasm-bindgen 0.2.128, Binaryen 132 `-Os`, `--out-name
bv_graph` — none of which is the toolchain in this repository, so a local
`wasm-pack` run cannot reproduce these bytes and would silently ship a
different artefact. `crates/bv-graph-wasm` has also diverged from Go's copy of
the same crate, which is a second reason not to rebuild from it.

`MANIFEST.json` itself records a stale digest for `bv_graph.js`
(`93de4c67…` against an actual `51d98027…`), because a later whitespace
reformat touched the file without refreshing the manifest. The file is copied
as it stands. Reconciling it would make this port diverge from the oracle,
which is the one thing the oracle is there to prevent.

# `viewer_assets/index.html` and `viewer_assets/styles.css` — the dashboard shell

The dashboard's own markup and stylesheet, hand-written rather than vendored,
and the only two files in Go's embed walk that this crate transforms. Copied
from `pkg/export/viewer_assets/` at parity commit `18afafa` and embedded with
`include_bytes!` in `crates/bv-export/src/viewer_shell.rs`.

| File | Bytes | sha256 | Source | Licence |
|---|---|---|---|---|
| `index.html` | 303412 | `652ca4c658af57586c8ff7b9774114935450baf1fdf4126eca68f1f59d3a9a6a` | `pkg/export/viewer_assets/index.html` | same as this repository |
| `styles.css` | 50610 | `ab65816834232900eb2a065afced3475bae6a5e5b98aad849bcf0ea622499d2b` | `pkg/export/viewer_assets/styles.css` | same as this repository |

Both are pure LF (zero CR bytes, measured), so no normalisation applies.
`styles.css`'s digest is also the digest of Go's *emitted* copy, because it
takes the walk's default branch and is never touched: `if relPath ==
"index.html"` at `viewer_embed.go:67` is the only special case in the whole
walker. That makes it the reference proof that a verbatim copy needs no code at
all.

`index.html`'s digest is of the **source asset**, not of the export. The export
carries a per-run `?v=<unix seconds>` cache-buster on five script tags, so its
digest moves every run and cannot be pinned. Measured against the oracle, the
export differs from the source in exactly 6 lines — the `<title>` when
`--pages-title` is set, plus `head_init.js`, `charts.js`, `wasm_loader.js`,
`hybrid_scorer.js` and `viewer.js` — and in nothing else. `viewer_shell.rs`
pins that as an invariant rather than a blob: strip the five stamps and the
result must equal the asset byte-for-byte.

`README.md` is *not* in this table. It is not an asset at all — Go generates it
per run with `generateREADME` (`cmd/bv/main.go:5527`) — and its footer embeds
`time.Now()`, so it has no fixed digest to record.

# `viewer_assets/vendor/` — the third-party bundle, all 16 files

Go's `CopyEmbeddedAssets` (`pkg/export/viewer_embed.go:30`) walks the embedded
tree and writes every file to `outputDir/<relPath>` with no transformation, so
all sixteen are the same bytes carried over. The directory layout mirrors Go's
so the relPath is a literal passthrough.
`crates/bv-export/src/sqlite_export.rs` (`VENDOR_ASSETS` + `copy_vendor_assets`)
embeds them with `include_bytes!` and writes them out unchanged;
`vendor_asset_hashes_match_the_go_oracle` pins every sha256, and
`vendor_assets_are_sixteen_files_written_verbatim` pins the 6,381,450-byte total.

| File | Bytes | Upstream | Licence |
|---|---|---|---|
| `alpine.min.js` | 70836 | Alpine.js v3.14.3 | MIT |
| `alpine-collapse.min.js` | 2362 | `@alpinejs/collapse` | MIT |
| `chart.umd.min.js` | 331832 | Chart.js 4.4.1 | MIT |
| `d3.v7.min.js` | 472990 | D3 7.9.0 | ISC |
| `dompurify.min.js` | 33427 | DOMPurify 3.0.6 | Apache-2.0 / MPL-2.0 |
| `force-graph.min.js` | 274043 | [vasturiano/force-graph](https://github.com/vasturiano/force-graph) v1.43.5 | MIT |
| `inter-variable.woff2` | 23812 | Inter | OFL-1.1 |
| `jetbrains-mono-regular.woff2` | 92380 | JetBrains Mono | OFL-1.1 |
| `marked.min.js` | 57222 | marked 14.1.4 | MIT |
| `mermaid.min.js` | 3336758 | Mermaid 10.9.x | MIT |
| `sql-wasm.js` | 83791 | sql.js | MIT |
| `sql-wasm.wasm` | 655300 | SQLite 3.45.2 compiled to wasm | public domain |
| `tailwindcss.js` | 690631 | Tailwind CSS Play CDN 3.x | MIT |
| `bv_graph.js`, `bv_graph_bg.wasm` | 31915 / 216094 | see the WASM-pair section above | same as this repository |
| `MANIFEST.json` | 8057 | provenance record for the 15 siblings | same as this repository |

The two woff2 files and the two wasm/binary payloads are read with
`include_bytes!` only; `include_str!` would be a compile error on their CR
bytes.

`vendor/marked.min.js` here is *not* the `assets/marked.min.js` at the top of
this file. The two are different byte streams (`1f2a6819…` versus `3306a1f7…`)
serving different exporters, and the top-level copy is the one
`graph_interactive.rs` renders with.

## `vendor/MANIFEST.json` is copied stale, on purpose

At the frozen parity commit `18afafa`, **10 of the manifest's 15 recorded
sha256 values do not match the bytes on disk** upstream: `alpine.min.js`,
`alpine-collapse.min.js`, `bv_graph.js`, `chart.umd.min.js`, `d3.v7.min.js`,
`dompurify.min.js`, `force-graph.min.js`, `marked.min.js`, `sql-wasm.js` and
`tailwindcss.js`. The five that still match are `bv_graph_bg.wasm`,
`inter-variable.woff2`, `jetbrains-mono-regular.woff2`, `mermaid.min.js` and
`sql-wasm.wasm`. The cause is commit `d433a82`, which reformatted ten minified
`.js` assets without refreshing the manifest; it is not a line-ending artefact
(every mismatching file has zero CR bytes, and CRLF round-tripping does not
reproduce the recorded digests). Go's own
`TestVendorManifest_MatchesShippedAssets` fails upstream at that commit.

Two consequences for this port:

* The manifest is copied exactly as it stands. It is on the export acceptance
  list at 8057 bytes, and regenerating its digests would make this one file
  differ from the oracle — the exact thing the oracle exists to prevent.
* The manifest-verification test and `scripts/verify_vendor.sh` are deliberately
  **not** ported. A Rust equivalent would fail on the same ten entries, and
  "fixing" it would mean diverging. `vendor_asset_hashes_match_the_go_oracle`
  checks the digests that actually ship instead.


