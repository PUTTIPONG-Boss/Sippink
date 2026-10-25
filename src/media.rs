//! What a link turned out to be, which output formats make sense for it,
//! and turning the downloaded source into the format the user picked.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    Video,
    Gif,
    Image,
    File,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Video => "VIDEO",
            Kind::Gif => "GIF",
            Kind::Image => "IMAGE",
            Kind::File => "FILE",
        }
    }

    /// First entry is the default.
    pub fn formats(self) -> &'static [Fmt] {
        match self {
            Kind::Video => &[Fmt::Mp4, Fmt::Mp3, Fmt::M4a, Fmt::Gif, Fmt::Webm],
            Kind::Gif => &[Fmt::Gif, Fmt::Mp4, Fmt::Webm, Fmt::Png, Fmt::Jpg],
            Kind::Image => &[Fmt::Jpg, Fmt::Png, Fmt::Webp],
            Kind::File => &[Fmt::Original],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    Mp4,
    Webm,
    Mp3,
    M4a,
    Gif,
    Jpg,
    Png,
    Webp,
    Original,
}

impl Fmt {
    pub fn ext(self) -> &'static str {
        match self {
            Fmt::Mp4 => "mp4",
            Fmt::Webm => "webm",
            Fmt::Mp3 => "mp3",
            Fmt::M4a => "m4a",
            Fmt::Gif => "gif",
            Fmt::Jpg => "jpg",
            Fmt::Png => "png",
            Fmt::Webp => "webp",
            Fmt::Original => "",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Fmt::Mp4 => "MP4",
            Fmt::Webm => "WEBM",
            Fmt::Mp3 => "MP3",
            Fmt::M4a => "M4A",
            Fmt::Gif => "GIF",
            Fmt::Jpg => "JPG",
            Fmt::Png => "PNG",
            Fmt::Webp => "WEBP",
            Fmt::Original => "AS-IS",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Quality {
    Best,
    Medium,
    Small,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::Best, Quality::Medium, Quality::Small];

    pub fn label(self) -> &'static str {
        match self {
            Quality::Best => "BEST",
            Quality::Medium => "MEDIUM",
            Quality::Small => "SMALL",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Variant {
    pub url: String,
    /// e.g. "1280x720" or "ORIG"
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct Media {
    pub kind: Kind,
    /// Filename without extension.
    pub stem: String,
    /// Extension of what the server sends (X GIFs are "mp4").
    pub src_ext: String,
    /// Best first.
    pub variants: Vec<Variant>,
}

impl Media {
    pub fn pick(&self, q: Quality) -> &Variant {
        let n = self.variants.len();
        let i = match q {
            Quality::Best => 0,
            Quality::Medium => n / 2,
            Quality::Small => n - 1,
        };
        &self.variants[i]
    }
}

#[derive(Clone, Debug)]
pub struct Probe {
    /// Shown above the chooser, e.g. "@gdldev on X".
    pub title: String,
    pub items: Vec<Media>,
}

impl Probe {
    /// Kinds present, in first-seen order.
    pub fn kinds(&self) -> Vec<Kind> {
        let mut v = Vec::new();
        for m in &self.items {
            if !v.contains(&m.kind) {
                v.push(m.kind);
            }
        }
        v
    }
}

pub fn kind_for(content_type: &str, ext: &str) -> Kind {
    let ct = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if ct == "image/gif" || ext == "gif" {
        Kind::Gif
    } else if ct.starts_with("image/") || matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "bmp") {
        Kind::Image
    } else if ct.starts_with("video/") || matches!(ext, "mp4" | "webm" | "mov" | "mkv" | "m4v") {
        Kind::Video
    } else {
        Kind::File
    }
}

/// True when turning `m` into `fmt` takes more than renaming the file.
fn is_passthrough(m: &Media, fmt: Fmt) -> bool {
    fmt == Fmt::Original || fmt.ext() == m.src_ext
}

/// Conversions the built-in image decoder handles without ffmpeg.
fn image_crate_can(m: &Media, fmt: Fmt) -> bool {
    let still_src = matches!(m.src_ext.as_str(), "jpg" | "png" | "webp" | "gif" | "bmp");
    still_src && matches!(fmt, Fmt::Jpg | Fmt::Png | Fmt::Webp)
}

pub fn needs_ffmpeg(m: &Media, fmt: Fmt) -> bool {
    !is_passthrough(m, fmt) && !image_crate_can(m, fmt)
}

/// `wanted` if it can be made, else the first format for `m` that can.
pub fn usable_fmt(m: &Media, wanted: Option<Fmt>, have_ffmpeg: bool) -> Fmt {
    let ok = |f: &Fmt| have_ffmpeg || !needs_ffmpeg(m, *f);
    wanted
        .filter(|f| m.kind.formats().contains(f))
        .filter(ok)
        .or_else(|| m.kind.formats().iter().copied().find(ok))
        .unwrap_or(Fmt::Original)
}

/// `SIPPINK_FFMPEG`, else ffmpeg next to the exe, on PATH, or where
/// `winget install Gyan.FFmpeg` links it.
pub fn find_ffmpeg() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SIPPINK_FFMPEG").map(PathBuf::from) {
        return p.is_file().then_some(p);
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)) {
        dirs.push(d);
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    if let Some(l) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(l).join("Microsoft").join("WinGet").join("Links"));
    }
    dirs.into_iter()
        .map(|d| d.join(if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" }))
        .find(|p| p.is_file())
}

/// Writes `src` (as downloaded) to `dest` in `fmt`. `src` is left in place.
pub fn convert(
    ffmpeg: Option<&Path>,
    m: &Media,
    src: &Path,
    fmt: Fmt,
    dest: &Path,
) -> Result<(), String> {
    if image_crate_can(m, fmt) {
        let img = image::open(src).map_err(|e| format!("decode: {e}"))?;
        let img = match fmt {
            // JPEG has no alpha channel.
            Fmt::Jpg => image::DynamicImage::ImageRgb8(img.to_rgb8()),
            _ => img,
        };
        return img.save(dest).map_err(|e| format!("encode: {e}"));
    }
    let ffmpeg = ffmpeg.ok_or("this format needs ffmpeg (winget install Gyan.FFmpeg)")?;
    let args: &[&str] = match fmt {
        Fmt::Mp3 => &["-map", "0:a:0", "-c:a", "libmp3lame", "-q:a", "2"],
        Fmt::M4a => &["-map", "0:a:0", "-c:a", "aac", "-b:a", "192k"],
        Fmt::Gif => &[
            "-vf",
            "fps=15,scale='min(480,iw)':-2:flags=lanczos,split[a][b];[a]palettegen[p];[b][p]paletteuse",
            "-loop",
            "0",
        ],
        Fmt::Webm => &["-c:v", "libvpx-vp9", "-crf", "34", "-b:v", "0", "-c:a", "libopus"],
        Fmt::Mp4 => &[
            "-movflags",
            "+faststart",
            "-pix_fmt",
            "yuv420p",
            "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2",
        ],
        Fmt::Jpg | Fmt::Png | Fmt::Webp => &["-frames:v", "1"],
        Fmt::Original => &[],
    };
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(args)
        .arg(dest);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let out = cmd.output().map_err(|e| format!("ffmpeg: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("matches no streams") {
            return Err("this video has no sound to save".into());
        }
        let last = err.lines().last().unwrap_or("failed").trim();
        Err(format!("ffmpeg: {last}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(kind: Kind, src_ext: &str) -> Media {
        Media { kind, stem: "a".into(), src_ext: src_ext.into(), variants: vec![] }
    }

    #[test]
    fn ffmpeg_only_where_needed() {
        let photo = media(Kind::Image, "jpg");
        assert!(!needs_ffmpeg(&photo, Fmt::Jpg));
        assert!(!needs_ffmpeg(&photo, Fmt::Png));
        let real_gif = media(Kind::Gif, "gif");
        assert!(!needs_ffmpeg(&real_gif, Fmt::Gif));
        assert!(!needs_ffmpeg(&real_gif, Fmt::Png));
        assert!(needs_ffmpeg(&real_gif, Fmt::Mp4));
        assert_eq!(usable_fmt(&real_gif, None, false), Fmt::Gif);
        let x_gif = media(Kind::Gif, "mp4");
        assert_eq!(usable_fmt(&x_gif, Some(Fmt::Gif), false), Fmt::Mp4);
        assert_eq!(usable_fmt(&x_gif, Some(Fmt::Gif), true), Fmt::Gif);
        assert_eq!(usable_fmt(&photo, Some(Fmt::Mp3), true), Fmt::Jpg);
        assert!(!needs_ffmpeg(&x_gif, Fmt::Mp4));
        assert!(needs_ffmpeg(&x_gif, Fmt::Gif));
        assert!(needs_ffmpeg(&x_gif, Fmt::Png));
        let video = media(Kind::Video, "mp4");
        assert!(needs_ffmpeg(&video, Fmt::Mp3));
    }

    #[test]
    fn png_to_jpg_without_ffmpeg() {
        let dir = std::env::temp_dir().join("sippink-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 128])).save(&src).unwrap();
        for fmt in [Fmt::Jpg, Fmt::Webp] {
            let dest = dir.join(format!("out.{}", fmt.ext()));
            convert(None, &media(Kind::Image, "png"), &src, fmt, &dest).unwrap();
            assert!(image::open(&dest).is_ok());
        }
    }
}
