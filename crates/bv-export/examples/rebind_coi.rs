//! Rebind the offline service worker against an already-exported bundle.
//!
//! `coi-serviceworker.js` is the one file in the site whose bytes depend on
//! every other file in it, so it is also the one file worth diffing against
//! Go: re-run this over a `bv --export-pages` bundle and `cmp` the result
//! against the worker that run produced. Equal output means the manifest
//! builder agrees with Go's on which files exist, how they sort, and how they
//! hash.
//!
//!     cargo run -p bv-export --example rebind_coi -- /path/to/go-export
//!
//! The bundle is edited in place. It does not have to be Go's: a Rust export
//! rebinds against itself, which is how you tell a manifest that was
//! regenerated from one that was merely copied forward.

use std::path::PathBuf;

fn main() -> Result<(), String> {
    let Some(arg) = std::env::args().nth(1) else {
        eprintln!("usage: rebind_coi <exported-bundle-dir>");
        std::process::exit(2);
    };
    let dir = PathBuf::from(arg);
    if !dir.is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }

    let before = std::fs::read(dir.join("coi-serviceworker.js")).ok();
    bv_export::viewer_assets::bind_offline_assets(&dir)?;
    let after = std::fs::read(dir.join("coi-serviceworker.js"))
        .map_err(|e| format!("reading coi-serviceworker.js: {e}"))?;

    match before {
        Some(prev) if prev == after => println!("coi-serviceworker.js: unchanged"),
        Some(prev) => println!(
            "coi-serviceworker.js: rewritten ({} → {} bytes)",
            prev.len(),
            after.len()
        ),
        None => println!("coi-serviceworker.js: created ({} bytes)", after.len()),
    }
    Ok(())
}
