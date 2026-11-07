//! Preview server — port of Go `pkg/export/preview.go` +
//! `livereload.go`: static file serving on 127.0.0.1:9000-9100 with
//! SSE livereload at /__preview__/events and status endpoint.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender};

const PORT_RANGE_START: u16 = 9000;
const PORT_RANGE_END: u16 = 9100;

#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    #[error("no available port in range {start}-{end}")]
    NoPort { start: u16, end: u16 },
    #[error("path traversal detected: {0}")]
    PathTraversal(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Resolve a safe path under `root` (anti-traversal guard).
fn safe_join(root: &Path, requested: &str) -> Result<PathBuf, PreviewError> {
    let cleaned = requested.trim_start_matches('/');
    let resolved = root.join(cleaned);
    let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    // The resolved path must stay inside root (after canonicalizing existing
    // portions; for not-yet-existing files we check component-wise).
    if let Ok(canonical) = resolved.canonicalize() {
        if !canonical.starts_with(&canonical_root) {
            return Err(PreviewError::PathTraversal(requested.to_string()));
        }
    } else {
        // Check components for traversal sequences.
        for comp in Path::new(cleaned).components() {
            if matches!(comp, std::path::Component::ParentDir) {
                return Err(PreviewError::PathTraversal(requested.to_string()));
            }
        }
    }
    Ok(resolved)
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css",
        "js" | "mjs" => "application/javascript",
        "json" => "application/json",
        "sqlite3" | "db" => "application/octet-stream",
        "wasm" => "application/wasm",
        "md" => "text/markdown; charset=utf-8",
        "txt" => "text/plain; charset=utf-8",
        "woff2" => "font/woff2",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

/// Livereload hub: fans file-change events out to every connected SSE
/// client. Each client owns a channel; the watcher thread broadcasts the
/// bundle-relative path that changed, and dead receivers are reaped on send.
struct LiveReloadHub {
    clients: Mutex<Vec<Sender<String>>>,
    shutdown: AtomicBool,
}

impl LiveReloadHub {
    fn new() -> Self {
        LiveReloadHub {
            clients: Mutex::new(Vec::new()),
            shutdown: AtomicBool::new(false),
        }
    }

    /// Register an SSE client. The receiver ends when every sender is gone,
    /// which is also how a disconnecting browser closes its stream.
    fn register(&self) -> Receiver<String> {
        let (tx, rx) = crossbeam_channel::unbounded();
        self.clients.lock().expect("livereload clients").push(tx);
        rx
    }

    fn broadcast(&self, path: &str) {
        let mut clients = self.clients.lock().expect("livereload clients");
        clients.retain(|tx| tx.send(path.to_string()).is_ok());
    }
}

/// `path -> (len, mtime nanos)` for every regular file under `root`.
///
/// `file_type` is used rather than `Path::is_dir` so symlinked directories
/// are never descended into — the same rule `filepath.WalkDir` follows.
fn snapshot_tree(root: &Path) -> BTreeMap<PathBuf, (u64, u128)> {
    let mut map = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(path);
                continue;
            }
            if !file_type.is_file() {
                // Symlinks and special files carry no content of their own.
                continue;
            }
            if let Ok(meta) = std::fs::metadata(&path) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_nanos())
                    .unwrap_or(0);
                map.insert(path, (meta.len(), mtime));
            }
        }
    }
    map
}

/// Poll `root` for changes and broadcast each changed bundle-relative path.
/// There is no `notify` dependency in this crate; a 500ms metadata poll is
/// the same shape Go's livereload watcher uses and is cheap for a static
/// bundle.
fn watch_for_changes(root: PathBuf, hub: Arc<LiveReloadHub>) {
    let mut last = snapshot_tree(&root);
    while !hub.shutdown.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(500));
        let now = snapshot_tree(&root);
        let mut changed: Vec<String> = Vec::new();
        for (path, meta) in &now {
            if last.get(path) != Some(meta) {
                changed.push(relative_name(&root, path));
            }
        }
        for path in last.keys() {
            if !now.contains_key(path) {
                changed.push(relative_name(&root, path));
            }
        }
        last = now;
        for path in changed {
            hub.broadcast(&path);
        }
    }
}

fn relative_name(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Start the preview server. Blocks until Ctrl+C or error.
/// Returns the bound port on success before blocking (via callback).
pub fn start_preview(
    bundle_dir: &Path,
    on_ready: impl FnOnce(u16),
    livereload_enabled: bool,
) -> Result<(), PreviewError> {
    let root = bundle_dir.canonicalize()?;
    let (server, port) = find_available_port()?;
    let hub = Arc::new(LiveReloadHub::new());

    // Watch for file changes to trigger livereload. The SSE stream itself is
    // served on its own thread per connection below — a held-open event
    // stream cannot share the accept loop.
    if livereload_enabled {
        std::thread::spawn({
            let hub = Arc::clone(&hub);
            let watch_dir = root.clone();
            move || watch_for_changes(watch_dir, hub)
        });
    }

    on_ready(port);

    loop {
        match server.recv() {
            Ok(request) => {
                // One thread per request: the SSE endpoint holds its
                // connection open indefinitely, and `respond` blocks until
                // the body is written, so it must not sit on the accept loop.
                let hub = Arc::clone(&hub);
                let root = root.clone();
                std::thread::spawn(move || {
                    let method = request.method().as_str().to_string();
                    let url = request.url().to_string();
                    let path = url.split('?').next().unwrap_or("/");
                    if livereload_enabled && path == "/__preview__/events" {
                        let rx = hub.register();
                        let _ = request.respond(sse_response(rx));
                    } else {
                        let response = handle_request(&root, &method, &url);
                        let _ = request.respond(response);
                    }
                });
            }
            Err(e) => {
                eprintln!("preview: recv error: {e}");
                break;
            }
        }
    }

    hub.shutdown.store(true, Ordering::Relaxed);
    Ok(())
}

fn find_available_port() -> Result<(tiny_http::Server, u16), PreviewError> {
    for port in PORT_RANGE_START..=PORT_RANGE_END {
        if let Ok(server) = tiny_http::Server::http(("127.0.0.1", port)) {
            return Ok((server, port));
        }
    }
    Err(PreviewError::NoPort {
        start: PORT_RANGE_START,
        end: PORT_RANGE_END,
    })
}

/// The SSE body for `/__preview__/events`: a `Read` that emits the
/// `connected` greeting immediately and then one `reload` event per
/// broadcast, ending cleanly when the hub drops every sender.
struct SseReader {
    rx: Receiver<String>,
    pending: Vec<u8>,
    pos: usize,
}

impl SseReader {
    fn new(rx: Receiver<String>) -> Self {
        SseReader {
            rx,
            pending: b"event: connected\ndata: preview\n\n".to_vec(),
            pos: 0,
        }
    }
}

impl Read for SseReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if self.pos < self.pending.len() {
                let n = (self.pending.len() - self.pos).min(buf.len());
                buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
                self.pos += n;
                return Ok(n);
            }
            match self.rx.recv() {
                Ok(path) => {
                    // `data:` is single-line; a pathological filename cannot
                    // smuggle a second SSE field past the watcher.
                    let path = path.replace(['\n', '\r'], " ");
                    self.pending = format!("event: reload\ndata: {path}\n\n").into_bytes();
                    self.pos = 0;
                }
                // Every sender is gone — the stream ends rather than spinning.
                Err(_) => return Ok(0),
            }
        }
    }
}

fn sse_response(rx: Receiver<String>) -> tiny_http::Response<Box<dyn Read + Send>> {
    tiny_http::Response::new(
        tiny_http::StatusCode(200),
        vec![
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/event-stream"[..])
                .expect("static header"),
            tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..])
                .expect("static header"),
        ],
        Box::new(SseReader::new(rx)) as Box<dyn Read + Send>,
        // No Content-Length: tiny_http switches to chunked transfer, which is
        // what an open-ended event stream requires.
        None,
        None,
    )
}

fn handle_request(
    root: &Path,
    method: &str,
    url: &str,
) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let _ = method;

    // Status endpoint
    if url == "/__preview__/status" {
        let body = serde_json::json!({
            "status": "ok",
            "server": "bvr-preview",
        });
        return tiny_http::Response::from_string(body.to_string()).with_header(
            tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap(),
        );
    }

    // Static files: strip query params, default to index.html. `path_part`
    // may be empty or lack a leading slash entirely, so strip defensively
    // rather than slicing `[1..]`.
    let path_part = url.split('?').next().unwrap_or("/");
    let rel = if path_part == "/" || path_part.is_empty() {
        "index.html"
    } else {
        path_part.strip_prefix('/').unwrap_or(path_part)
    };

    match safe_join(root, rel) {
        Ok(file_path) => {
            if file_path.is_file() {
                let mut content = Vec::new();
                if let Ok(mut f) = std::fs::File::open(&file_path) {
                    let _ = f.read_to_end(&mut content);
                }
                let ct = content_type(&file_path);
                let mut response = tiny_http::Response::from_data(content).with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], ct.as_bytes()).unwrap(),
                );
                response.add_header(
                    tiny_http::Header::from_bytes(&b"Cache-Control"[..], &b"no-cache"[..]).unwrap(),
                );
                response
            } else {
                tiny_http::Response::from_string("404 Not Found").with_status_code(404)
            }
        }
        Err(_) => tiny_http::Response::from_string("403 Forbidden").with_status_code(403),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_join_blocks_traversal() {
        let root = std::env::temp_dir().join("bvr-preview-test");
        std::fs::create_dir_all(&root).unwrap();
        let result = safe_join(&root, "../../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn safe_join_allows_normal_paths() {
        let root = std::env::temp_dir().join("bvr-preview-test-ok");
        std::fs::create_dir_all(&root).unwrap();
        let result = safe_join(&root, "index.html");
        assert!(result.is_ok());
    }

    #[test]
    fn content_type_mapping() {
        assert_eq!(
            content_type(Path::new("a.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(content_type(Path::new("a.js")), "application/javascript");
        assert_eq!(content_type(Path::new("a.wasm")), "application/wasm");
        assert_eq!(
            content_type(Path::new("a.unknown")),
            "application/octet-stream"
        );
    }
}
