//! External helpers: finding them, installing them into Sippink's own
//! folder, and starting them without a console window.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::job::Evt;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    /// Converts audio/video and merges YouTube's separate streams.
    Ffmpeg,
    /// Extracts YouTube.
    YtDlp,
    /// JavaScript runtime yt-dlp uses for YouTube's player challenges.
    Deno,
}

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::YtDlp, Tool::Ffmpeg, Tool::Deno];

    pub fn name(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "ffmpeg",
            Tool::YtDlp => "yt-dlp",
            Tool::Deno => "deno",
        }
    }

    fn env(self) -> &'static str {
        match self {
            Tool::Ffmpeg => "SIPPINK_FFMPEG",
            Tool::YtDlp => "SIPPINK_YTDLP",
            Tool::Deno => "SIPPINK_DENO",
        }
    }

    fn exe(self) -> String {
        format!("{}{}", self.name(), std::env::consts::EXE_SUFFIX)
    }

    /// Official Windows builds.
    fn download_url(self) -> &'static str {
        match self {
            Tool::YtDlp => "https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe",
            Tool::Ffmpeg => "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
            Tool::Deno => "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip",
        }
    }
}

/// `%APPDATA%\sippink\bin`, or `SIPPINK_BIN`.
pub fn bin_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("SIPPINK_BIN") {
        return PathBuf::from(d);
    }
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("sippink")
        .join("bin")
}

/// The tool's env override, else Sippink's bin folder, the exe's folder,
/// PATH, or where winget links it.
pub fn find(tool: Tool) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(tool.env()).map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let mut dirs = vec![bin_dir()];
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(d);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(l).join("Microsoft").join("WinGet").join("Links"));
    }
    dirs.into_iter().map(|d| d.join(tool.exe())).find(|p| p.is_file())
}

/// What a worker thread needs to know about the helpers.
#[derive(Clone, Default)]
pub struct Tools {
    pub ffmpeg: Option<PathBuf>,
    pub ytdlp: Option<PathBuf>,
    pub deno: Option<PathBuf>,
}

impl Tools {
    pub fn detect() -> Self {
        Tools { ffmpeg: find(Tool::Ffmpeg), ytdlp: find(Tool::YtDlp), deno: find(Tool::Deno) }
    }

    pub fn get(&self, t: Tool) -> Option<&PathBuf> {
        match t {
            Tool::Ffmpeg => self.ffmpeg.as_ref(),
            Tool::YtDlp => self.ytdlp.as_ref(),
            Tool::Deno => self.deno.as_ref(),
        }
    }

    /// yt-dlp with UTF-8 output, no playlists, and Deno wired in when present.
    pub fn ytdlp(&self) -> Result<Command, String> {
        let exe = self.ytdlp.as_ref().ok_or("YouTube needs yt-dlp: press GET YT-DLP")?;
        let mut cmd = hidden(exe);
        cmd.env("PYTHONUTF8", "1")
            .args(["--encoding", "utf-8", "--no-playlist", "--no-warnings"]);
        if let Some(d) = &self.deno {
            cmd.arg("--js-runtimes").arg(format!("deno:{}", d.display()));
        }
        if let Some(f) = &self.ffmpeg {
            cmd.arg("--ffmpeg-location").arg(f);
        }
        Ok(cmd)
    }
}

/// A command that won't flash a console window.
pub fn hidden(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Downloads `tool` into [`bin_dir`]; progress goes to row `id`.
pub fn install(agent: &ureq::Agent, tool: Tool, id: u64, report: &dyn Fn(Evt)) -> Result<PathBuf, String> {
    let dir = bin_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dest = dir.join(tool.exe());
    let url = tool.download_url();
    if !url.ends_with(".zip") {
        let part = dir.join(format!("{}.part", tool.exe()));
        crate::job::fetch(agent, url, &part, id, report)?;
        std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
        return Ok(dest);
    }
    let zip_path = dir.join(format!("{}.zip.part", tool.name()));
    crate::job::fetch(agent, url, &zip_path, id, report)?;
    report(Evt::Phase { id, text: "unpacking".into() });
    let res = unzip_exes(&zip_path, &dir, tool);
    let _ = std::fs::remove_file(&zip_path);
    res?;
    dest.is_file().then_some(dest).ok_or_else(|| format!("{} not found in the archive", tool.exe()))
}

/// Pulls the tool's .exe files (e.g. ffmpeg + ffprobe) out of the archive,
/// wherever they sit inside it.
fn unzip_exes(zip_path: &Path, dir: &Path, tool: Tool) -> Result<(), String> {
    let f = std::fs::File::open(zip_path).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| e.to_string())?;
    let wanted: &[&str] = match tool {
        Tool::Ffmpeg => &["ffmpeg.exe", "ffprobe.exe"],
        _ => &[],
    };
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())) else {
            continue;
        };
        if name == tool.exe() || wanted.contains(&name.as_str()) {
            let mut out = std::fs::File::create(dir.join(&name)).map_err(|e| e.to_string())?;
            std::io::copy(&mut entry, &mut out).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// `yt-dlp -U`; YouTube changes often, so this fixes most breakage.
pub fn update_ytdlp(exe: &Path) -> Result<String, String> {
    let out = hidden(exe).arg("-U").output().map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    let last = text.lines().last().unwrap_or("").trim().to_string();
    if out.status.success() {
        Ok(last)
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().last().unwrap_or("update failed").trim().to_string())
    }
}
