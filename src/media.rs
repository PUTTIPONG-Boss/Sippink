//! What a link turned out to be, which output formats make sense for it,
//! and turning the downloaded source into the format the user picked.

use std::path::Path;

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

/// Where to sit in an item's size list: 0.0 = biggest, 1.0 = smallest.
/// Relative, so one choice carries over to items with different lists
/// (e.g. 720p of a video and LARGE of its thumbnail).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Quality(pub f32);

impl Quality {
    pub const BEST: Quality = Quality(0.0);

    /// Entry `i` of a list of `n`.
    pub fn at(i: usize, n: usize) -> Quality {
        if n <= 1 {
            Quality::BEST
        } else {
            Quality(i as f32 / (n - 1) as f32)
        }
    }

    pub fn index(self, n: usize) -> usize {
        if n <= 1 {
            0
        } else {
            ((self.0 * (n - 1) as f32).round() as usize).min(n - 1)
        }
    }
}

#[derive(Clone, Debug)]
pub struct Variant {
    /// The file itself, or the page for yt-dlp.
    pub url: String,
    /// e.g. "1280x720" or "ORIG"
    pub label: String,
    /// Height cap handed to yt-dlp.
    pub height: Option<u32>,
}

/// How the bytes get here.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fetch {
    Http,
    YtDlp,
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
    pub fetch: Fetch,
    /// Ticked in the chooser to begin with (extras like thumbnails aren't).
    pub default_on: bool,
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

/// Conversions the built-in image decoder handles without ffmpeg.
fn image_crate_can(src_ext: &str, fmt: Fmt) -> bool {
    let still_src = matches!(src_ext, "jpg" | "png" | "webp" | "gif" | "bmp");
    still_src && matches!(fmt, Fmt::Jpg | Fmt::Png | Fmt::Webp)
}

pub fn needs_ffmpeg(m: &Media, fmt: Fmt) -> bool {
    if m.fetch == Fetch::YtDlp {
        // yt-dlp can hand over MP4 (single-file formats) and M4A audio as-is.
        return !matches!(fmt, Fmt::Mp4 | Fmt::M4a);
    }
    let passthrough = fmt == Fmt::Original || fmt.ext() == m.src_ext;
    !passthrough && !image_crate_can(&m.src_ext, fmt)
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

/// Writes `src` (as downloaded) to `dest` in `fmt`. `src` is left in place.
pub fn convert(ffmpeg: Option<&Path>, src: &Path, fmt: Fmt, dest: &Path) -> Result<(), String> {
    let src_ext = src.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    if image_crate_can(&src_ext, fmt) {
        let img = image::open(src).map_err(|e| format!("decode: {e}"))?;
        let img = match fmt {
            // JPEG has no alpha channel.
            Fmt::Jpg => image::DynamicImage::ImageRgb8(img.to_rgb8()),
            _ => img,
        };
        return img.save(dest).map_err(|e| format!("encode: {e}"));
    }
    let ffmpeg = ffmpeg.ok_or("this format needs ffmpeg: press GET FFMPEG")?;
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
    let mut cmd = crate::tools::hidden(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        .args(args)
        .arg(dest);
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
        Media {
            kind,
            stem: "a".into(),
            src_ext: src_ext.into(),
            variants: vec![],
            fetch: Fetch::Http,
            default_on: true,
        }
    }

    #[test]
    fn quality_carries_over_by_rank() {
        let q = Quality::at(2, 8); // 1080p of 2160..144
        assert_eq!(q.index(8), 2);
        assert_eq!(Quality::BEST.index(5), 0);
        assert_eq!(Quality::at(7, 8).index(3), 2);
        assert_eq!(Quality::at(3, 8).index(1), 0);
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
        let yt = Media { fetch: Fetch::YtDlp, ..media(Kind::Video, "mp4") };
        assert!(!needs_ffmpeg(&yt, Fmt::Mp4));
        assert!(!needs_ffmpeg(&yt, Fmt::M4a));
        assert!(needs_ffmpeg(&yt, Fmt::Mp3));
        assert!(needs_ffmpeg(&yt, Fmt::Webm));
    }

    #[test]
    fn png_to_jpg_without_ffmpeg() {
        let dir = std::env::temp_dir().join("sippink-test");
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("in.png");
        image::RgbaImage::from_pixel(4, 4, image::Rgba([255, 0, 0, 128])).save(&src).unwrap();
        for fmt in [Fmt::Jpg, Fmt::Webp] {
            let dest = dir.join(format!("out.{}", fmt.ext()));
            convert(None, &src, fmt, &dest).unwrap();
            assert!(image::open(&dest).is_ok());
        }
    }
}
