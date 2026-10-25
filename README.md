# Sippink

*sip* + *link*: paste a link, get the media. A small tray app for Windows
(Rust + egui) with a terminal look.

## What it does

1. Paste a link and press **SIP** (or Enter).
2. Sippink looks inside first and tells you what's there: `VIDEO`, `GIF`,
   `IMAGE`, or a plain `FILE`, and how many.
3. Pick the output format and quality, untick items you don't want, then
   press **DOWNLOAD**.

| Source is | You can save as |
|---|---|
| VIDEO | MP4, MP3, M4A, GIF, WEBM |
| GIF   | GIF, MP4, WEBM, PNG, JPG |
| IMAGE | JPG, PNG, WEBP |

Quality: `BEST` / `MEDIUM` / `SMALL` (for X videos that's e.g. 3840x2160 /
1280x720 / 480x270; for photos it's ORIG / LARGE / SMALL).

`AUTO` skips the chooser and reuses the last formats you picked.

### Sites

- **X / Twitter**: posts with photos, videos, GIFs, several media, quote posts
  and app cards. Works with `x.com`, `twitter.com`, `mobile.*`, `fxtwitter`,
  `vxtwitter`, `fixupx`. Uses X's public embed endpoint, so no login is
  needed. Posts that need a login (sensitive, age-restricted, protected) are
  not supported yet.
- **Any direct file link**: type detected from `Content-Type` / extension.

Planned: YouTube, Reddit, Pinterest.

## ffmpeg

JPG/PNG/WEBP conversions are built in. Anything that re-encodes video or
audio (MP3, M4A, GIF from video, WEBM, a frame from a video) needs ffmpeg:

```
winget install Gyan.FFmpeg
```

Sippink looks for it in `SIPPINK_FFMPEG`, next to `sippink.exe`, on `PATH`,
and in winget's links folder. Without it, those formats are greyed out and X
GIFs are saved as MP4 loops.

## Command line

Same pipeline as the window, handy for scripts and testing (use a debug
build, release builds have no console):

```
sippink --get <url> [--fmt mp4|mp3|m4a|gif|webm|jpg|png|webp] [--quality best|medium|small] [--out dir] [--probe]
sippink <url> [--out dir]        # open the window with this link
```

## Build

```
cargo build --release
cargo test                 # offline tests
cargo test -- --ignored    # live tests against x.com
```

Font: [Monocraft](https://github.com/IdreesInc/Monocraft) (SIL OFL 1.1,
see `assets/Monocraft-OFL.txt`).
