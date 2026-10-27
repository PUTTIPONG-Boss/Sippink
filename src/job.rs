//! Probe a link, then download + convert each picked item.
//! Shared by the GUI and the `--get` command line.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::files::{ext_for_mime, resolve_filename, sanitize, unique_path};
use crate::media::{self, Fetch, Fmt, Kind, Media, Probe, Quality, Variant};
use crate::tools::Tools;
use crate::{pinterest, x, youtube};

pub enum Evt {
    Probed { token: u64, res: Result<Probe, String> },
    /// Running commentary from a long probe ("Board: 200 of 356 pins").
    ProbeStatus { token: u64, text: String },
    Named { id: u64, name: String },
    Progress { id: u64, done: u64, total: u64 },
    Phase { id: u64, text: String },
    Done { id: u64, path: PathBuf },
    Error { id: u64, msg: String },
    ToolReady { tool: crate::tools::Tool, res: Result<PathBuf, String> },
    Notice(String),
    /// Latest release tag, or why it couldn't be checked.
    UpdateChecked(Result<String, String>),
    /// The new exe is in place (start it), or the update failed.
    Updated(Result<PathBuf, String>),
}

/// Lets a long probe (a whole board) report progress and be stopped.
pub struct Ctx<'a> {
    pub progress: &'a dyn Fn(String),
    pub cancel: &'a AtomicBool,
    /// Search without the usual time budget (slow on big accounts).
    pub deep: bool,
}

#[cfg(test)]
impl Ctx<'static> {
    /// No progress, never cancelled.
    pub fn quiet() -> Self {
        static NEVER: AtomicBool = AtomicBool::new(false);
        Ctx { progress: &|_| {}, cancel: &NEVER, deep: false }
    }
}

impl Ctx<'_> {
    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

pub struct Job {
    pub id: u64,
    pub media: Media,
    pub fmt: Fmt,
    pub quality: Quality,
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) Sippink/0.1")
        .timeout_connect(Duration::from_secs(15))
        .build()
}

/// Works out what `url` holds without downloading the body.
pub fn probe(agent: &ureq::Agent, tools: &Tools, url: &str, ctx: &Ctx) -> Result<Probe, String> {
    if let Some(id) = x::tweet_id(url) {
        return x::resolve(agent, &id);
    }
    if let Some(id) = youtube::video_id(url) {
        return youtube::probe(tools, &id);
    }
    if pinterest::is_pinterest(url) {
        return pinterest::resolve(agent, tools, url, ctx);
    }
    if crate::meta::is_meta(url) {
        return youtube::probe_url(tools, url);
    }
    if url.split(['?', '#']).next().is_some_and(|p| p.ends_with(".m3u8")) {
        let host = url.split('/').nth(2).unwrap_or("link").to_string();
        let stem = sanitize(url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("stream"));
        return Ok(Probe {
            title: format!("HLS stream from {host}"),
            items: vec![Media {
                kind: Kind::Video,
                stem: stem.trim_end_matches(".m3u8").to_string(),
                src_ext: "mp4".into(),
                variants: vec![Variant { url: url.to_string(), label: "STREAM".into(), height: None }],
                fetch: Fetch::Hls,
                default_on: true,
                subdir: String::new(),
            }],
            note: String::new(),
        });
    }
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let name = resolve_filename(&resp, url);
    let ct = resp.header("content-type").unwrap_or("").to_string();
    let path = Path::new(&name);
    let stem = path.file_stem().map_or(name.clone(), |s| s.to_string_lossy().into_owned());
    let mut ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .or_else(|| ext_for_mime(&ct).map(String::from))
        .unwrap_or_else(|| "bin".into());
    if ext == "jpeg" {
        ext = "jpg".into();
    }
    let kind = media::kind_for(&ct, &ext);
    let host = url.split('/').nth(2).unwrap_or("link").to_string();
    Ok(Probe {
        title: format!("{} from {host}", kind.label()),
        items: vec![Media {
            kind,
            stem: sanitize(&stem),
            src_ext: ext,
            variants: vec![Variant { url: url.to_string(), label: "ORIGINAL".into(), height: None }],
            fetch: Fetch::Http,
            default_on: true,
            subdir: String::new(),
        }],
        note: String::new(),
    })
}

/// Runs `jobs` on `workers` threads; each result goes to `done`.
/// Returns once every job has finished.
pub fn run_all(
    tools: &Tools,
    folder: &Path,
    jobs: Vec<Job>,
    workers: usize,
    report: &(dyn Fn(Evt) + Sync),
    done: &(dyn Fn(&Job, Result<PathBuf, String>) + Sync),
) {
    let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(jobs)));
    std::thread::scope(|s| {
        for _ in 0..workers.max(1) {
            let queue = queue.clone();
            s.spawn(move || {
                let agent = agent();
                loop {
                    let Some(job) = queue.lock().unwrap().pop_front() else { break };
                    let res = run(&agent, tools, folder, &job, report);
                    done(&job, res);
                }
            });
        }
    });
}

/// A size that isn't on the server (Pinterest answers 403 for missing
/// originals, others 404).
fn missing(err: &str) -> bool {
    err.contains("status code 404") || err.contains("status code 403")
}

/// Temp files of one job start with this, so a failure can sweep them all
/// (yt-dlp leaves per-stream pieces like `.f137.mp4.part`).
pub fn temp_prefix(id: u64) -> String {
    format!(".sippink-{}-{id}.", std::process::id())
}

/// Downloads one item into `folder` and converts it if needed.
/// Returns the final file path.
pub fn run(
    agent: &ureq::Agent,
    tools: &Tools,
    folder: &Path,
    job: &Job,
    report: &dyn Fn(Evt),
) -> Result<PathBuf, String> {
    let m = &job.media;
    let ffmpeg = tools.ffmpeg.as_deref();
    let dir = folder.join(&m.subdir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let folder = dir.as_path();
    let fmt = if m.kind == Kind::File { Fmt::Original } else { job.fmt };
    if ffmpeg.is_none() && media::needs_ffmpeg(m, fmt) {
        return Err(format!("{} needs ffmpeg: press GET FFMPEG", fmt.label()));
    }
    let out_ext = if fmt == Fmt::Original { m.src_ext.as_str() } else { fmt.ext() };
    let planned = folder.join(format!("{}.{out_ext}", m.stem));
    // Boards and profiles download into their own folder; running one again
    // only fetches what isn't there yet.
    if !m.subdir.is_empty() && planned.is_file() {
        report(Evt::Named { id: job.id, name: format!("{} (already there)", m.stem) });
        return Ok(planned);
    }
    let dest = unique_path(planned);
    let shown = dest.file_name().map_or(String::new(), |s| s.to_string_lossy().into_owned());
    report(Evt::Named { id: job.id, name: shown });

    let first = job.quality.index(m.variants.len());
    let res = (|| {
        let src = match m.fetch {
            Fetch::Http => {
                let src = folder.join(format!("{}{}", temp_prefix(job.id), m.src_ext));
                let part = src.with_extension(format!("{}.part", m.src_ext));
                // Size lists can name sizes that don't exist (old YouTube
                // videos have no maxres thumbnail, some pins no original):
                // step down to the next size.
                let mut tries = m.variants[first..].iter().peekable();
                while let Some(v) = tries.next() {
                    match fetch(agent, &v.url, &part, job.id, report) {
                        Err(e) if missing(&e) && tries.peek().is_some() => continue,
                        r => r?,
                    }
                    break;
                }
                std::fs::rename(&part, &src).map_err(|e| e.to_string())?;
                src
            }
            Fetch::YtDlp => youtube::download(tools, &m.variants[first], fmt, folder, job.id, report)?,
            Fetch::Hls => {
                let src = folder.join(format!("{}mp4", temp_prefix(job.id)));
                report(Evt::Phase { id: job.id, text: "fetching stream".into() });
                let ffmpeg = ffmpeg.ok_or("streams need ffmpeg: press GET FFMPEG")?;
                let inputs = hls_inputs(agent, &m.variants[first].url)?;
                fetch_hls(ffmpeg, &inputs, &src, job.id, report)?;
                report(Evt::Phase { id: job.id, text: String::new() });
                src
            }
        };
        let src_ext = src.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
        if fmt == Fmt::Original || src_ext.as_deref() == Some(out_ext) {
            return std::fs::rename(&src, &dest).map_err(|e| e.to_string());
        }
        report(Evt::Phase { id: job.id, text: format!("converting to {}", fmt.label()) });
        let r = media::convert(ffmpeg, &src, fmt, &dest);
        let _ = std::fs::remove_file(&src);
        r
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&dest);
        let prefix = temp_prefix(job.id);
        for e in std::fs::read_dir(folder).into_iter().flatten().flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    res.map(|()| dest)
}

pub fn fetch(
    agent: &ureq::Agent,
    url: &str,
    to: &Path,
    id: u64,
    report: &dyn Fn(Evt),
) -> Result<(), String> {
    let resp = agent.get(url).call().map_err(|e| e.to_string())?;
    let total: u64 = resp
        .header("content-length")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(to).map_err(|e| e.to_string())?;
    let mut buf = [0u8; 65536];
    let mut done: u64 = 0;
    let mut last = Instant::now();
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if last.elapsed() >= Duration::from_millis(120) {
            report(Evt::Progress { id, done, total });
            last = Instant::now();
        }
    }
    report(Evt::Progress { id, done, total });
    Ok(())
}

/// What to hand ffmpeg for an HLS link: the best video rendition and its
/// separate audio rendition, if any.
///
/// Renditions stored as one CMAF file addressed by byte ranges (Pinterest
/// does this) are passed as that plain file: ffmpeg's HLS reader garbles
/// them and stops after a couple of seconds.
fn hls_inputs(agent: &ureq::Agent, master: &str) -> Result<Vec<String>, String> {
    let get = |url: &str| -> Result<String, String> {
        agent.get(url).call().map_err(|e| e.to_string())?.into_string().map_err(|e| e.to_string())
    };
    let text = get(master)?;
    if !text.contains("#EXT-X-STREAM-INF") {
        return Ok(vec![single_file(master, &text).unwrap_or_else(|| master.to_string())]);
    }
    // Highest-bandwidth variant: its URI is the line after the tag.
    let mut best: Option<(u64, &str)> = None;
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let bw = attr(attrs, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0);
            let uri = lines.by_ref().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#'));
            if let Some(uri) = uri.filter(|_| best.map_or(true, |(b, _)| bw > b)) {
                best = Some((bw, uri));
            }
        }
    }
    let video = join_url(master, best.ok_or("empty HLS playlist")?.1);
    let mut inputs = vec![video];
    let audio = text
        .lines()
        .filter_map(|l| l.strip_prefix("#EXT-X-MEDIA:"))
        .filter(|a| attr(a, "TYPE") == Some("AUDIO"))
        .find_map(|a| attr(a, "URI"));
    if let Some(a) = audio {
        inputs.push(join_url(master, a));
    }
    for input in inputs.iter_mut() {
        if let Ok(t) = get(input) {
            if let Some(file) = single_file(input, &t) {
                *input = file;
            }
        }
    }
    Ok(inputs)
}

/// The file behind a media playlist whose init map and every segment are
/// byte ranges of one URI.
fn single_file(playlist_url: &str, text: &str) -> Option<String> {
    let map_uri = text.lines().filter_map(|l| l.strip_prefix("#EXT-X-MAP:")).find_map(|a| attr(a, "URI"))?;
    let mut segs = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    let all_same = segs.all(|s| s == map_uri);
    (all_same && text.contains("#EXT-X-BYTERANGE")).then(|| join_url(playlist_url, map_uri))
}

/// `KEY=value` or `KEY="value"` from an HLS attribute list.
fn attr<'a>(attrs: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = attrs;
    while !rest.is_empty() {
        let (k, after) = rest.split_once('=')?;
        let (v, next) = match after.strip_prefix('"') {
            Some(q) => {
                let (v, n) = q.split_once('"')?;
                (v, n.strip_prefix(',').unwrap_or(n))
            }
            None => after.split_once(',').unwrap_or((after, "")),
        };
        if k.trim() == key {
            return Some(v);
        }
        rest = next;
    }
    None
}

fn join_url(base: &str, rel: &str) -> String {
    if rel.starts_with("http://") || rel.starts_with("https://") {
        return rel.to_string();
    }
    let base = base.split(['?', '#']).next().unwrap_or(base);
    match base.rfind('/') {
        Some(i) => format!("{}/{rel}", &base[..i]),
        None => rel.to_string(),
    }
}

/// Copies the inputs (video, then optional audio) into one MP4 without
/// re-encoding. ffmpeg's `-progress` lines give the bytes written so far;
/// the total is unknown.
fn fetch_hls(ffmpeg: &Path, inputs: &[String], to: &Path, id: u64, report: &dyn Fn(Evt)) -> Result<(), String> {
    use std::io::BufRead;
    let mut cmd = crate::tools::hidden(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-nostats", "-progress", "pipe:1", "-y"]);
    for i in inputs {
        cmd.arg("-i").arg(i);
    }
    if inputs.len() > 1 {
        cmd.args(["-map", "0:v:0", "-map", "1:a:0"]);
    }
    let mut child = cmd
        .args(["-c", "copy", "-movflags", "+faststart"])
        .arg(to)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    let stderr = child.stderr.take().unwrap();
    let errs = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::BufReader::new(stderr), &mut buf);
        buf
    });
    for line in std::io::BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let Some(n) = line.strip_prefix("total_size=").and_then(|n| n.trim().parse().ok()) {
            report(Evt::Progress { id, done: n, total: 0 });
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        let err = errs.join().unwrap_or_default();
        Err(format!("ffmpeg: {}", err.lines().last().unwrap_or("stream failed").trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hls_attributes() {
        let a = r#"TYPE=AUDIO,GROUP-ID="audio1",NAME="1",URI="x_audio.m3u8",CHANNELS="2""#;
        assert_eq!(attr(a, "TYPE"), Some("AUDIO"));
        assert_eq!(attr(a, "URI"), Some("x_audio.m3u8"));
        assert_eq!(attr("BANDWIDTH=405528,CODECS=\"a,b\",RESOLUTION=1x2", "RESOLUTION"), Some("1x2"));
        assert_eq!(join_url("https://h/a/b/m.m3u8?t=1", "v.m3u8"), "https://h/a/b/v.m3u8");
        assert_eq!(join_url("https://h/a/m.m3u8", "https://o/v.m3u8"), "https://o/v.m3u8");
    }

    #[test]
    fn spots_single_file_cmaf() {
        let cmaf = "#EXTM3U
#EXT-X-MAP:URI=\"v.cmfv\",BYTERANGE=\"971@0\"
#EXTINF:2.0
#EXT-X-BYTERANGE:10@971
v.cmfv
#EXTINF:2.0
#EXT-X-BYTERANGE:10@981
v.cmfv
";
        assert_eq!(single_file("https://h/p/v.m3u8", cmaf).as_deref(), Some("https://h/p/v.cmfv"));
        let segmented = "#EXTM3U
#EXTINF:2.0
seg1.ts
#EXTINF:2.0
seg2.ts
";
        assert_eq!(single_file("https://h/p/v.m3u8", segmented), None);
    }
}
