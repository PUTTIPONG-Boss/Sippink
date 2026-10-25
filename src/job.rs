//! Probe a link, then download + convert each picked item.
//! Shared by the GUI and the `--get` command line.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::files::{ext_for_mime, resolve_filename, sanitize, unique_path};
use crate::media::{self, Fmt, Kind, Media, Probe, Quality, Variant};
use crate::x;

pub enum Evt {
    Probed { token: u64, res: Result<Probe, String> },
    Named { id: u64, name: String },
    Progress { id: u64, done: u64, total: u64 },
    Phase { id: u64, text: String },
    Done { id: u64, path: PathBuf },
    Error { id: u64, msg: String },
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
pub fn probe(agent: &ureq::Agent, url: &str) -> Result<Probe, String> {
    if let Some(id) = x::tweet_id(url) {
        return x::resolve(agent, &id);
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
            variants: vec![Variant { url: url.to_string(), label: "ORIGINAL".into() }],
        }],
    })
}

/// Downloads one item into `folder` and converts it if needed.
/// Returns the final file path.
pub fn run(
    agent: &ureq::Agent,
    ffmpeg: Option<&Path>,
    folder: &Path,
    job: &Job,
    report: &dyn Fn(Evt),
) -> Result<PathBuf, String> {
    let m = &job.media;
    let fmt = if m.kind == Kind::File { Fmt::Original } else { job.fmt };
    if ffmpeg.is_none() && media::needs_ffmpeg(m, fmt) {
        return Err(format!("{} needs ffmpeg (winget install Gyan.FFmpeg)", fmt.label()));
    }
    let out_ext = if fmt == Fmt::Original { m.src_ext.as_str() } else { fmt.ext() };
    let dest = unique_path(folder.join(format!("{}.{out_ext}", m.stem)));
    let shown = dest.file_name().map_or(String::new(), |s| s.to_string_lossy().into_owned());
    report(Evt::Named { id: job.id, name: shown });

    let direct = fmt == Fmt::Original || out_ext == m.src_ext;
    let part = if direct {
        dest.with_extension(format!("{out_ext}.part"))
    } else {
        unique_path(folder.join(format!("{}.src.{}.part", m.stem, m.src_ext)))
    };
    let res = (|| {
        fetch(agent, &m.pick(job.quality).url, &part, job.id, report)?;
        if direct {
            std::fs::rename(&part, &dest).map_err(|e| e.to_string())
        } else {
            report(Evt::Phase { id: job.id, text: format!("converting to {}", fmt.label()) });
            // ffmpeg / the image crate pick the codec from the extension, so
            // give them a source path with its real extension.
            let src = part.with_extension("");
            std::fs::rename(&part, &src).map_err(|e| e.to_string())?;
            let r = media::convert(ffmpeg, m, &src, fmt, &dest);
            let _ = std::fs::remove_file(&src);
            r
        }
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&part);
        let _ = std::fs::remove_file(&dest);
    }
    res.map(|()| dest)
}

fn fetch(
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
