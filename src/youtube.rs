//! YouTube, through yt-dlp. YouTube's player changes too often to chase by
//! hand; yt-dlp keeps up and `yt-dlp -U` pulls the fixes.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::Value;

use crate::files::sanitize;
use crate::job::Evt;
use crate::media::{Fetch, Fmt, Kind, Media, Probe, Variant};
use crate::tools::Tools;

/// The 11-character video id, if `url` is a single YouTube video.
pub fn video_id(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    let path = path.split('#').next().unwrap_or("");
    let id = match host {
        "youtu.be" => path.split('/').next().map(String::from),
        "youtube.com" | "m.youtube.com" | "music.youtube.com" | "youtube-nocookie.com" => {
            let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
            match parts.as_slice() {
                ["watch"] => query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("v="))
                    .map(String::from),
                ["shorts" | "live" | "embed" | "v", id, ..] => Some(id.to_string()),
                _ => None,
            }
        }
        _ => None,
    }?;
    let ok = id.len() == 11 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    ok.then_some(id)
}

pub fn watch_url(id: &str) -> String {
    format!("https://www.youtube.com/watch?v={id}")
}

pub fn probe(tools: &Tools, id: &str) -> Result<Probe, String> {
    probe_url(tools, &watch_url(id))
}

/// Any page yt-dlp understands (YouTube, and e.g. Vimeo players embedded
/// in Pinterest pins).
pub fn probe_url(tools: &Tools, url: &str) -> Result<Probe, String> {
    let url = url.to_string();
    let out = tools.ytdlp()?.args(["-J", &url]).output().map_err(|e| format!("yt-dlp: {e}"))?;
    if !out.status.success() {
        return Err(ytdlp_error(&out.stderr));
    }
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("yt-dlp output: {e}"))?;
    if v["is_live"] == true {
        return Err("live streams aren't supported yet".into());
    }
    let id = v["id"].as_str().unwrap_or("video");
    let title = crate::meta::clean_title(v["title"].as_str().unwrap_or(id));
    let site = v["extractor_key"].as_str().unwrap_or("YouTube");
    let channel = v["channel"].as_str().or(v["uploader"].as_str()).unwrap_or(site);
    // Facebook titles already end with the page name.
    let title = title.strip_suffix(&format!(" | {channel}")).unwrap_or(title);
    let secs = v["duration"].as_f64().unwrap_or(0.0).round() as u64;
    let mut stem = sanitize(title);
    if stem.chars().count() > 100 {
        stem = stem.chars().take(100).collect::<String>().trim().to_string();
    }
    let stem = format!("{stem} [{id}]");

    let mut video = Media {
        kind: Kind::Video,
        stem: stem.clone(),
        src_ext: "mp4".into(),
        variants: heights(&v, tools.ffmpeg.is_some())
            .into_iter()
            .map(|h| Variant { url: url.clone(), label: format!("{h}p"), height: Some(h) })
            .collect(),
        fetch: Fetch::YtDlp,
        default_on: true,
        subdir: String::new(),
    };
    if video.variants.is_empty() {
        if v["formats"].as_array().map_or(true, |f| f.is_empty()) {
            return Err("no downloadable formats (try UPDATE YT-DLP)".into());
        }
        // Site lists formats without sizes: let yt-dlp pick its best.
        video.variants.push(Variant { url: url.clone(), label: "BEST".into(), height: None });
    }
    let mut items = vec![video];
    if let Some(thumb) = thumbnail(&v, &stem) {
        items.push(thumb);
    }
    Ok(Probe {
        title: format!("{title} | {channel} | {}:{:02}", secs / 60, secs % 60),
        items,
        note: String::new(),
    })
}

/// Heights on offer, best first. Without ffmpeg yt-dlp can't merge the
/// separate video/audio streams, so only formats that carry both count.
fn heights(v: &Value, can_merge: bool) -> Vec<u32> {
    let mut hs: Vec<u32> = v["formats"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["vcodec"].as_str().is_some_and(|c| c != "none"))
        .filter(|f| can_merge || f["acodec"].as_str().is_some_and(|c| c != "none"))
        // storyboards / images have no real codec
        .filter(|f| f["protocol"].as_str() != Some("mhtml"))
        .filter_map(|f| f["height"].as_u64().map(|h| h as u32))
        .collect();
    hs.sort_unstable_by(|a, b| b.cmp(a));
    hs.dedup();
    hs
}

/// The cover image as an optional extra item (unticked by default).
fn thumbnail(v: &Value, stem: &str) -> Option<Media> {
    const RANK: [&str; 5] = ["maxresdefault", "sddefault", "hqdefault", "mqdefault", "default"];
    const LABEL: [&str; 5] = ["MAXRES", "SD", "HQ", "MQ", "SMALL"];
    let mut found: Vec<(usize, String)> = v["thumbnails"]
        .as_array()?
        .iter()
        .filter_map(|t| t["url"].as_str())
        .map(|u| u.split('?').next().unwrap_or(u))
        .filter(|u| u.ends_with(".jpg"))
        .filter_map(|u| {
            let name = u.rsplit('/').next()?.trim_end_matches(".jpg");
            RANK.iter().position(|r| *r == name).map(|i| (i, u.to_string()))
        })
        .collect();
    found.sort();
    found.dedup_by_key(|(i, _)| *i);
    if found.is_empty() {
        return None;
    }
    let variants = found
        .into_iter()
        .map(|(i, url)| Variant { url, label: LABEL[i].into(), height: None })
        .collect();
    Some(Media {
        kind: Kind::Image,
        stem: format!("{stem} thumb"),
        src_ext: "jpg".into(),
        variants,
        fetch: Fetch::Http,
        default_on: false,
        subdir: String::new(),
    })
}

/// yt-dlp's `-f` / `-S` for the wanted output, so it fetches as little as
/// possible: audio-only for MP3/M4A, video-only for GIF, VP9/Opus for WEBM.
fn selection(fmt: Fmt, height: Option<u32>, can_merge: bool) -> (String, String, Option<&'static str>) {
    let res = match (fmt, height) {
        (Fmt::Gif, Some(h)) => format!("res:{}", h.min(480)),
        (Fmt::Gif, None) => "res:480".into(),
        (_, Some(h)) => format!("res:{h}"),
        (_, None) => "res".into(),
    };
    match fmt {
        Fmt::Mp3 | Fmt::M4a => ("ba[ext=m4a]/ba/b".into(), "".into(), None),
        Fmt::Gif => ("bv/b".into(), res, None),
        Fmt::Webm if can_merge => ("bv*+ba/b".into(), format!("{res},vcodec:vp9,acodec:opus"), Some("webm/mp4")),
        _ if can_merge => ("bv*+ba/b".into(), format!("{res},vcodec:h264,acodec:aac"), Some("mp4")),
        _ => ("b".into(), format!("{res},vcodec:h264"), None),
    }
}

/// Runs yt-dlp into `folder` under a temporary name and returns the file it
/// wrote. Its extension may differ from `fmt`; the caller converts.
pub fn download(
    tools: &Tools,
    variant: &Variant,
    fmt: Fmt,
    folder: &Path,
    id: u64,
    report: &dyn Fn(Evt),
) -> Result<PathBuf, String> {
    let (format, sort, merge) = selection(fmt, variant.height, tools.ffmpeg.is_some());
    let template = folder.join(format!("{}%(ext)s", crate::job::temp_prefix(id)));
    let mut cmd = tools.ytdlp()?;
    cmd.args(["-f", &format]);
    if !sort.is_empty() {
        cmd.args(["-S", &sort]);
    }
    if let Some(m) = merge {
        cmd.args(["--merge-output-format", m]);
    }
    cmd.args(["--no-mtime", "--newline", "--progress", "--print", "after_move:filepath"])
        .args(["--progress-template", "download:SIPP %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s"])
        .arg("-o")
        .arg(&template)
        .arg(&variant.url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("yt-dlp: {e}"))?;
    // Drain stderr on its own thread so a chatty yt-dlp can't block on a full pipe.
    let stderr = child.stderr.take().unwrap();
    let errs = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut BufReader::new(stderr), &mut buf);
        buf
    });
    let mut path = None;
    let mut stream = 0;
    let mut last_done = 0;
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if let Some(rest) = line.strip_prefix("SIPP ") {
            let n: Vec<u64> = rest.split(' ').map(|s| s.parse().unwrap_or(0)).collect();
            let done = n.first().copied().unwrap_or(0);
            let total = n.get(1).copied().filter(|t| *t > 0).or(n.get(2).copied()).unwrap_or(0);
            // A drop in bytes means the next stream (audio after video) began.
            if done < last_done {
                stream += 1;
                report(Evt::Phase { id, text: String::new() });
            }
            last_done = done;
            report(Evt::Progress { id, done, total });
            if stream > 0 && total > 0 && done >= total && merge.is_some() {
                report(Evt::Phase { id, text: "merging".into() });
            }
        } else if !line.trim().is_empty() {
            path = Some(PathBuf::from(line.trim()));
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let stderr = errs.join().unwrap_or_default();
    if !status.success() {
        return Err(ytdlp_error(&stderr));
    }
    path.filter(|p| p.is_file()).ok_or_else(|| "yt-dlp finished but wrote no file".into())
}

fn ytdlp_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .rev()
        .find(|l| l.starts_with("ERROR"))
        .or(text.lines().last())
        .unwrap_or("yt-dlp failed")
        .trim()
        .trim_start_matches("ERROR: ")
        .to_string();
    if line.contains("Sign in to confirm") {
        format!("YouTube wants a login for this one ({line})")
    } else if line.contains("--cookies") || line.contains("login") {
        // yt-dlp's text ends with a long FAQ pointer; the first sentence is enough.
        let site = line.split(']').next().unwrap_or("").trim_start_matches('[');
        format!("{site} only shows this post to logged-in users")
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_video_links() {
        let id = Some("jNQXAC9IVRw".to_string());
        assert_eq!(video_id("https://www.youtube.com/watch?v=jNQXAC9IVRw"), id);
        assert_eq!(video_id("https://youtube.com/watch?feature=share&v=jNQXAC9IVRw&t=3"), id);
        assert_eq!(video_id("https://youtu.be/jNQXAC9IVRw?si=abc"), id);
        assert_eq!(video_id("https://m.youtube.com/shorts/jNQXAC9IVRw"), id);
        assert_eq!(video_id("https://music.youtube.com/watch?v=jNQXAC9IVRw&list=RD"), id);
        assert_eq!(video_id("https://www.youtube.com/embed/jNQXAC9IVRw"), id);
        assert_eq!(video_id("https://www.youtube.com/live/jNQXAC9IVRw"), id);
        assert_eq!(video_id("https://www.youtube.com/@jawed"), None);
        assert_eq!(video_id("https://www.youtube.com/playlist?list=PL123"), None);
        assert_eq!(video_id("https://example.com/watch?v=jNQXAC9IVRw"), None);
    }

    #[test]
    fn picks_lean_selections() {
        let (f, s, m) = selection(Fmt::Mp3, Some(1080), true);
        assert_eq!((f.as_str(), s.as_str(), m), ("ba[ext=m4a]/ba/b", "", None));
        let (_, s, m) = selection(Fmt::Mp4, Some(2160), true);
        assert_eq!((s.as_str(), m), ("res:2160,vcodec:h264,acodec:aac", Some("mp4")));
        let (f, _, m) = selection(Fmt::Mp4, Some(360), false);
        assert_eq!((f.as_str(), m), ("b", None));
        let (_, s, _) = selection(Fmt::Gif, Some(1080), true);
        assert_eq!(s, "res:480");
    }
}
