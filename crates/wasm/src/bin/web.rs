// Place at: <your-crate>/src/bin/serve.rs
//
// Rust translation of serve.sh. No external crates — just std, so this adds
// no dependency to the crate's graph even if the crate is published as a
// library elsewhere.
//
// Usage:
//   cargo run --bin serve [port]         # default port 8080
//   NO_REBUILD=1 cargo run --bin serve   # skip the rebuild (serve whatever is in pkg/)
//   SKIP_VAMPIRE=1 cargo run --bin serve     # forwarded to build-vampire.sh
//   VAMPIRE_RECLONE=1 cargo run --bin serve  # forwarded to build-vampire.sh
//
// With this alias in .cargo/config.toml:
//   [alias]
//   serve = "run --bin serve --"
// you can just run `cargo serve` / `cargo serve 3000`.
//
// Simplifications vs. the Python server: connections are closed after one
// response (no keep-alive), and a directory with no index.html 404s instead
// of listing its contents. Neither matters for this demo site.

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::thread;

fn main() -> ExitCode {
    // CARGO_MANIFEST_DIR is set by cargo at compile time to this crate's
    // root — the equivalent of serve.sh's $CRATE_DIR.
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let port: u16 = env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(8080);

    if let Err(e) = rebuild(&crate_dir) {
        eprintln!("serve: {e}");
        return ExitCode::FAILURE;
    }

    if let Err(e) = mirror_pkg(&crate_dir) {
        eprintln!("serve: {e}");
        return ExitCode::FAILURE;
    }

    // Non-fatal, same as serve.sh's `|| { echo ... }` — SKIP_VAMPIRE and
    // VAMPIRE_RECLONE are read by build-vampire.sh itself; Command inherits
    // the parent's environment, so they pass through untouched.
    let vampire_ok = Command::new("sh")
        .arg(crate_dir.join("build-vampire.sh"))
        .status()
        .is_ok_and(|s| s.success());
    if !vampire_ok {
        eprintln!(
            "==> Vampire WASM build failed or skipped — the demo still runs, just without that backend."
        );
    }

    println!();
    println!("  Open:  http://localhost:{port}/");
    println!("  (Ctrl-C to stop)");
    println!();

    serve(&crate_dir.join("web"), port);
    ExitCode::SUCCESS
}

fn rebuild(crate_dir: &Path) -> std::io::Result<()> {
    let no_rebuild = env::var("NO_REBUILD").as_deref() == Ok("1");
    if !no_rebuild {
        println!("==> Rebuilding pkg/ so the served wasm + JS are current…");
        run_build_npm(crate_dir)?;
    } else if !crate_dir.join("pkg/sdk.mjs").is_file() {
        run_build_npm(crate_dir)?;
    }
    Ok(())
}

fn run_build_npm(crate_dir: &Path) -> std::io::Result<()> {
    let status = Command::new("sh").arg(crate_dir.join("build-npm.sh")).status()?;
    if !status.success() {
        return Err(std::io::Error::other("build-npm.sh failed"));
    }
    Ok(())
}

// The demo imports `./pkg/…`, so pkg/ must be a sibling of web/index.html —
// mirror it in, same as the deployed layout.
fn mirror_pkg(crate_dir: &Path) -> std::io::Result<()> {
    let web_pkg = crate_dir.join("web/pkg");
    if web_pkg.exists() {
        fs::remove_dir_all(&web_pkg)?;
    }
    copy_dir_recursive(&crate_dir.join("pkg"), &web_pkg)?;

    // Threaded bundle is optional; mirror it only if a prior THREADED=1
    // build produced it. sigma.worker.js falls back to pkg/ if absent.
    let pkg_threaded = crate_dir.join("pkg-threaded");
    if pkg_threaded.is_dir() {
        let web_pkg_threaded = crate_dir.join("web/pkg-threaded");
        if web_pkg_threaded.exists() {
            fs::remove_dir_all(&web_pkg_threaded)?;
        }
        copy_dir_recursive(&pkg_threaded, &web_pkg_threaded)?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else {
            fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}

// ---------- static file server ----------

fn serve(web_root: &Path, port: u16) {
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|e| {
        eprintln!("serve: couldn't bind 127.0.0.1:{port}: {e}");
        std::process::exit(1);
    });

    // One thread per connection, mirroring ThreadingHTTPServer.
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let web_root = web_root.to_path_buf();
        thread::spawn(move || {
            let _ = handle_connection(stream, &web_root);
        });
    }
}

fn handle_connection(mut stream: TcpStream, web_root: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(());
    }
    // Drain headers — we don't need them, but must read past them.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }

    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let url_path = parts.next().unwrap_or("/").split('?').next().unwrap_or("/");

    if method != "GET" && method != "HEAD" {
        return write_response(&mut stream, 405, "Method Not Allowed", &[], 0, b"");
    }

    match resolve(web_root, url_path) {
        Some(file_path) => {
            let mut body = Vec::new();
            fs::File::open(&file_path)?.read_to_end(&mut body)?;
            let headers = [
                ("Content-Type", mime_for(&file_path)),
                ("Cache-Control", "no-store, no-cache, must-revalidate"),
                ("Expires", "0"),
                // vampire.wasm is a pthread-enabled Emscripten build, which
                // only gets SharedArrayBuffer on a cross-origin-isolated
                // page — these two headers turn that on. Harmless when the
                // Vampire backend isn't built.
                ("Cross-Origin-Opener-Policy", "same-origin"),
                ("Cross-Origin-Embedder-Policy", "require-corp"),
            ];
            let len = body.len();
            let to_send: &[u8] = if method == "HEAD" { b"" } else { &body };
            write_response(&mut stream, 200, "OK", &headers, len, to_send)
        }
        None => write_response(&mut stream, 404, "Not Found", &[], 13, b"404 Not Found"),
    }
}

/// Mirrors serve.sh's SPA fallback: reject `..` traversal; serve index.html
/// for a directory (if present) or for a path with no file extension that
/// doesn't exist on disk (client-side routes like /edit, /diagnostics).
fn resolve(web_root: &Path, url_path: &str) -> Option<PathBuf> {
    let decoded = percent_decode(url_path.trim_start_matches('/'));
    if decoded.split('/').any(|seg| seg == "..") {
        return None;
    }

    if decoded.is_empty() {
        let index = web_root.join("index.html");
        return index.is_file().then_some(index);
    }

    let candidate = web_root.join(&decoded);
    if candidate.is_dir() {
        let index = candidate.join("index.html");
        return index.is_file().then_some(index);
    }
    if candidate.is_file() {
        return Some(candidate);
    }

    let has_ext = decoded.rsplit('/').next().is_some_and(|last| last.contains('.'));
    if !has_ext {
        let index = web_root.join("index.html");
        if index.is_file() {
            return Some(index);
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        _ => "application/octet-stream",
    }
}

fn write_response(
    stream: &mut TcpStream,
    code: u16,
    reason: &str,
    extra_headers: &[(&str, &str)],
    content_length: usize,
    body: &[u8],
) -> std::io::Result<()> {
    write!(stream, "HTTP/1.1 {code} {reason}\r\n")?;
    write!(stream, "Content-Length: {content_length}\r\n")?;
    write!(stream, "Connection: close\r\n")?;
    for (k, v) in extra_headers {
        write!(stream, "{k}: {v}\r\n")?;
    }
    write!(stream, "\r\n")?;
    stream.write_all(body)?;
    Ok(())
}