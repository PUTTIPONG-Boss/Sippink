//! Filename helpers: picking, cleaning and de-duplicating download names.

use std::path::PathBuf;

pub fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    format!("{:.1} {}", v, U[i])
}

pub fn filename_from_url(url: &str) -> String {
    let no_q = url.split(['?', '#']).next().unwrap_or(url);
    let last = no_q.rsplit('/').next().unwrap_or("");
    let name = percent_decode(last);
    if name.trim().is_empty() {
        "download.bin".into()
    } else {
        name
    }
}

pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(h) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn unique_path(mut p: PathBuf) -> PathBuf {
    if !p.exists() {
        return p;
    }
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = p.extension().map(|s| s.to_string_lossy().into_owned());
    let dir = p.parent().map(|d| d.to_path_buf()).unwrap_or_default();
    for n in 1..9999 {
        let name = match &ext {
            Some(e) => format!("{} ({}).{}", stem, n, e),
            None => format!("{} ({})", stem, n),
        };
        p = dir.join(name);
        if !p.exists() {
            return p;
        }
    }
    p
}

pub fn sanitize(name: &str) -> String {
    let replaced = name.replace(['/', '\\'], "_");
    let bad = ['<', '>', ':', '"', '|', '?', '*'];
    let cleaned: String = replaced
        .chars()
        .filter(|c| !bad.contains(c) && !c.is_control())
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim().to_string();
    if cleaned.is_empty() {
        "download.bin".into()
    } else {
        cleaned
    }
}

pub fn ext_for_mime(ct: &str) -> Option<&'static str> {
    let ct = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    Some(match ct.as_str() {
        "video/mp4" => "mp4",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-msvideo" | "video/avi" => "avi",
        "video/mpeg" => "mpg",
        "video/x-flv" => "flv",
        "video/3gpp" => "3gp",
        "video/mp2t" => "ts",
        "audio/mpeg" => "mp3",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/ogg" => "ogg",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mp4" | "audio/aac" | "audio/x-m4a" => "m4a",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "application/pdf" => "pdf",
        "application/zip" => "zip",
        "application/x-rar-compressed" | "application/vnd.rar" => "rar",
        "application/x-7z-compressed" => "7z",
        "application/x-tar" => "tar",
        "application/gzip" => "gz",
        "application/x-bittorrent" => "torrent",
        _ => return None,
    })
}

pub fn filename_from_disposition(h: &str) -> Option<String> {
    let lower = h.to_ascii_lowercase();
    // RFC 5987: filename*=UTF-8''percent%20encoded.ext
    if let Some(idx) = lower.find("filename*=") {
        let v = h[idx + "filename*=".len()..]
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        let parts: Vec<&str> = v.splitn(3, '\'').collect();
        let raw = if parts.len() == 3 { parts[2] } else { v };
        let name = percent_decode(raw.trim_matches('"'));
        if !name.trim().is_empty() {
            return Some(sanitize(&name));
        }
    }
    if let Some(idx) = lower.find("filename=") {
        let v = h[idx + "filename=".len()..]
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .trim_matches('"');
        let name = percent_decode(v);
        if !name.trim().is_empty() {
            return Some(sanitize(&name));
        }
    }
    None
}

pub fn resolve_filename(resp: &ureq::Response, url: &str) -> String {
    if let Some(cd) = resp.header("content-disposition") {
        if let Some(name) = filename_from_disposition(cd) {
            return name;
        }
    }
    let mut name = sanitize(&filename_from_url(url));
    let has_ext = std::path::Path::new(&name).extension().is_some();
    if !has_ext {
        if let Some(ct) = resp.header("content-type") {
            if let Some(ext) = ext_for_mime(ct) {
                name = format!("{name}.{ext}");
            }
        }
    }
    name
}

