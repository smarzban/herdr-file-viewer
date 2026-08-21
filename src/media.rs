//! Raster previews (PDF / images) and on-demand HTML for terminal-browser.
//!
//! PDF and images are rasterized to PNG on a worker thread and drawn in-pane via the
//! Kitty graphics protocol (ratatui-image). HTML and Markdown stay text in the pane;
//! `g` converts Markdown to a temp HTML file and opens it in `terminal-browser`.
//! Read-only: never writes into the viewed tree (temp HTML lives under `std::env::temp_dir`).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::proc;

const RASTER_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_SOURCE_BYTES: u64 = 30 * 1024 * 1024;
const PDF_SCALE: &str = "1400";

/// What a path can do in the rich-preview spike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Pdf,
    Image,
    Html,
    Markdown,
    Other,
}

/// Classify by extension (case-insensitive). No I/O.
pub fn kind(path: &Path) -> MediaKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => MediaKind::Pdf,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff" => MediaKind::Image,
        "html" | "htm" => MediaKind::Html,
        "md" | "markdown" => MediaKind::Markdown,
        _ => MediaKind::Other,
    }
}

/// Rasterize a PDF (page 1 via `pdftoppm`) or an image file to PNG bytes.
/// `None` when the path is the wrong kind, too large, unreadable, or the tool is missing.
pub fn rasterize_png(path: &Path) -> Option<Vec<u8>> {
    if !path.is_file() {
        return None;
    }
    let len = fs::metadata(path).ok()?.len();
    if len == 0 || len > MAX_SOURCE_BYTES {
        return None;
    }
    match kind(path) {
        MediaKind::Pdf => rasterize_pdf(path),
        MediaKind::Image => rasterize_image(path),
        _ => None,
    }
}

fn rasterize_image(path: &Path) -> Option<Vec<u8>> {
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let mut out = Vec::new();
    let mut cursor = std::io::Cursor::new(&mut out);
    img.write_to(&mut cursor, image::ImageFormat::Png).ok()?;
    Some(out)
}

fn rasterize_pdf(path: &Path) -> Option<Vec<u8>> {
    let dir = tempfile_dir()?;
    let prefix = dir.join("page");
    let mut child = Command::new("pdftoppm")
        .args([
            "-png",
            "-singlefile",
            "-f",
            "1",
            "-l",
            "1",
            "-scale-to",
            PDF_SCALE,
        ])
        .arg(path)
        .arg(&prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let status = proc::wait_bounded(&mut child, RASTER_TIMEOUT)?;
    if !status.success() {
        return None;
    }
    let png_path = prefix.with_extension("png");
    let bytes = fs::read(&png_path).ok()?;
    let _ = fs::remove_file(png_path);
    Some(bytes)
}

/// Build a `file://` URL for terminal-browser. Markdown is converted to a temp HTML file.
pub fn browser_url(path: &Path) -> Option<String> {
    match kind(path) {
        MediaKind::Html => Some(file_url(path)),
        MediaKind::Markdown => {
            let md = fs::read_to_string(path).ok()?;
            let title = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("preview");
            let html = markdown_to_html(&md, title);
            let out = tempfile_dir()?.join("preview.html");
            fs::write(&out, html).ok()?;
            Some(file_url(&out))
        }
        MediaKind::Pdf | MediaKind::Image => Some(file_url(path)),
        MediaKind::Other => None,
    }
}

/// A conservative Markdown → HTML converter for the on-demand browser preview.
pub fn markdown_to_html(md: &str, title: &str) -> String {
    let mut body = String::new();
    let mut in_code = false;
    let mut para: Vec<String> = Vec::new();
    let flush_para = |para: &mut Vec<String>, body: &mut String| {
        if para.is_empty() {
            return;
        }
        let joined = para.join(" ");
        body.push_str("<p>");
        body.push_str(&inline(&joined));
        body.push_str("</p>\n");
        para.clear();
    };
    for line in md.lines() {
        if let Some(rest) = line.strip_prefix("```") {
            flush_para(&mut para, &mut body);
            if in_code {
                body.push_str("</code></pre>\n");
                in_code = false;
            } else {
                let lang = escape(rest.trim());
                body.push_str("<pre><code class=\"language-");
                body.push_str(&lang);
                body.push_str("\">");
                in_code = true;
            }
            continue;
        }
        if in_code {
            body.push_str(&escape(line));
            body.push('\n');
            continue;
        }
        if line.trim().is_empty() {
            flush_para(&mut para, &mut body);
            continue;
        }
        if let Some(n) = heading_level(line) {
            flush_para(&mut para, &mut body);
            let text = line[n + 1..].trim();
            body.push_str(&format!("<h{n}>{}</h{n}>\n", inline(text)));
            continue;
        }
        if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            flush_para(&mut para, &mut body);
            body.push_str("<ul><li>");
            body.push_str(&inline(item));
            body.push_str("</li></ul>\n");
            continue;
        }
        para.push(line.trim().to_string());
    }
    if in_code {
        body.push_str("</code></pre>\n");
    }
    flush_para(&mut para, &mut body);
    format!(
        "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>{}</title>\
<style>body{{font-family:ui-sans-serif,system-ui,sans-serif;max-width:52rem;margin:2rem auto;\
padding:0 1.25rem;line-height:1.55;color:#e8e8e8;background:#111}} \
pre{{background:#1c1c1c;padding:1rem;overflow:auto}} code{{font-family:ui-monospace,monospace}} \
a{{color:#8cb4ff}}</style></head><body>\n{}\n</body></html>",
        escape(title),
        body
    )
}

fn heading_level(line: &str) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut n = 0;
    while n < bytes.len() && bytes[n] == b'#' {
        n += 1;
    }
    if (1..=6).contains(&n) && bytes.get(n) == Some(&b' ') {
        Some(n)
    } else {
        None
    }
}

fn inline(s: &str) -> String {
    let escaped = escape(s);
    replace_delimited(&escaped, '`', "code")
}

fn replace_delimited(s: &str, delim: char, tag: &str) -> String {
    let parts: Vec<&str> = s.split(delim).collect();
    if parts.len() < 3 {
        return s.to_string();
    }
    let mut out = String::new();
    for (i, part) in parts.iter().enumerate() {
        if i % 2 == 1 && i + 1 < parts.len() {
            out.push_str(&format!("<{tag}>{part}</{tag}>"));
        } else {
            out.push_str(part);
        }
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

fn file_url(path: &Path) -> String {
    let abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut url = String::from("file://");
    for component in abs.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Prefix(p) => {
                url.push('/');
                url.push_str(&p.as_os_str().to_string_lossy());
            }
            other => {
                url.push('/');
                let raw = other.as_os_str().to_string_lossy();
                for b in raw.as_bytes() {
                    match *b {
                        b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                            url.push(*b as char);
                        }
                        _ => url.push_str(&format!("%{:02X}", b)),
                    }
                }
            }
        }
    }
    if abs.as_os_str().to_string_lossy().starts_with('/') && !url.starts_with("file:///") {
        url.insert(7, '/');
    }
    url
}

fn tempfile_dir() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("herdr-file-viewer-preview");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// Launch `terminal-browser open --split right <url>` non-blocking. `None` if the binary is missing.
pub fn open_in_terminal_browser(url: &str) -> Result<(), String> {
    match Command::new("terminal-browser")
        .args(["open", "--split", "right", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(
            "terminal-browser is not installed (https://github.com/zenbu-labs/terminal-browser)"
                .into(),
        ),
        Err(err) => Err(format!("could not launch terminal-browser: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_by_extension() {
        assert_eq!(kind(Path::new("a.PDF")), MediaKind::Pdf);
        assert_eq!(kind(Path::new("x.png")), MediaKind::Image);
        assert_eq!(kind(Path::new("n.markdown")), MediaKind::Markdown);
        assert_eq!(kind(Path::new("i.htm")), MediaKind::Html);
        assert_eq!(kind(Path::new("main.rs")), MediaKind::Other);
    }

    #[test]
    fn markdown_to_html_renders_heading_and_code() {
        let html = markdown_to_html("# Hi\n\nuse `x`\n", "t");
        assert!(html.contains("<h1>Hi</h1>"));
        assert!(html.contains("<code>x</code>"));
        assert!(html.contains("<title>t</title>"));
    }

    #[test]
    fn markdown_escapes_html() {
        let html = markdown_to_html("<script>alert(1)</script>", "t");
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("&lt;script&gt;"));
    }
}
