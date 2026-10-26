//! Pinterest pins through the same JSON resource the website uses
//! (`/resource/PinResource/get/`), which answers without a login for
//! numeric pin ids.
//!
//! Search results opened while logged out use `e1_...` ids instead; those
//! are session tokens the public API rejects (the site itself redirects to
//! /ideas/), so they get a hint rather than a download.

use serde_json::Value;

use crate::files::sanitize;
use crate::media::{Fetch, Kind, Media, Probe, Variant};
use crate::tools::Tools;

pub fn is_pinterest(url: &str) -> bool {
    host_of(url).is_some_and(|h| h == "pin.it" || h.split('.').any(|p| p == "pinterest"))
}

fn host_of(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    Some(rest.split(['/', '?', '#']).next()?.to_ascii_lowercase())
}

/// What the `/pin/<...>/` part of a Pinterest URL holds.
#[derive(Debug, PartialEq)]
enum PinRef {
    Id(String),
    /// Logged-out search token, not fetchable.
    Session,
    /// A board, profile, search...
    NotAPin,
}

fn pin_ref(url: &str) -> PinRef {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let mut parts = path.split('/').skip(3).filter(|p| !p.is_empty());
    if parts.next() != Some("pin") {
        return PinRef::NotAPin;
    }
    let Some(seg) = parts.next() else {
        return PinRef::NotAPin;
    };
    // Newer links can read /pin/some-title--1234567890/
    let id = seg.rsplit("--").next().unwrap_or(seg);
    if id.starts_with("e1_") {
        PinRef::Session
    } else if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
        PinRef::Id(id.to_string())
    } else {
        PinRef::NotAPin
    }
}

pub fn resolve(agent: &ureq::Agent, tools: &Tools, url: &str) -> Result<Probe, String> {
    let mut url = url.trim().to_string();
    if host_of(&url).as_deref() == Some("pin.it") {
        // Short links redirect to the full pin URL.
        let resp = agent.get(&url).call().map_err(|e| e.to_string())?;
        url = resp.get_url().to_string();
    }
    let id = match pin_ref(&url) {
        PinRef::Id(id) => id,
        PinRef::Session => {
            return Err("that's a temporary search link: open the pin, press Share, copy the link".into())
        }
        PinRef::NotAPin => return Err("boards and profiles aren't supported yet: open one pin and copy its link".into()),
    };
    let data = format!(
        r#"{{"options":{{"field_set_key":"unauth_react_main_pin","id":"{id}"}},"context":{{}}}}"#
    );
    let resp = agent
        .get("https://www.pinterest.com/resource/PinResource/get/")
        .query("source_url", &format!("/pin/{id}/"))
        .query("data", &data)
        .set("X-Pinterest-PWS-Handler", "www/[username].js")
        .call();
    let body: Value = match resp {
        Ok(r) => serde_json::from_reader(r.into_reader()).map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(404, _)) => return Err("pin not found, deleted, or private".into()),
        Err(e) => return Err(e.to_string()),
    };
    let pin = &body["resource_response"]["data"];
    if !pin.is_object() {
        return Err("pin not found".into());
    }

    let mut items = collect(pin);
    let user = pin["native_creator"]["username"]
        .as_str()
        .or(pin["pinner"]["username"].as_str())
        .unwrap_or("pinterest");
    let many = items.len() > 1;
    for (n, m) in items.iter_mut().enumerate() {
        m.stem = if many {
            format!("{user}_{id}_{}", n + 1)
        } else {
            format!("{user}_{id}")
        };
    }
    let title = pin["title"]
        .as_str()
        .or(pin["grid_title"].as_str())
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("pin");
    let mut title = format!("{} | @{user} on Pinterest", sanitize(title));

    // A pin that plays a Vimeo / YouTube video only carries a still of it;
    // the video itself comes through yt-dlp, the still stays as an extra.
    let has_video = items.iter().any(|m| m.kind != Kind::Image);
    if let Some(src) = pin["embed"]["src"].as_str().filter(|_| !has_video) {
        let src = src.replacen("http://", "https://", 1);
        match crate::youtube::probe_url(tools, &src) {
            Ok(mut p) => {
                for mut m in items {
                    m.default_on = false;
                    m.stem = format!("{user}_{id}_cover");
                    p.items.push(m);
                }
                return Ok(p);
            }
            Err(e) if items.is_empty() => return Err(e),
            Err(e) => title = format!("{title} (video: {e})"),
        }
    }
    if items.is_empty() {
        return Err("no image or video in this pin".into());
    }
    Ok(Probe { title, items })
}

/// Media of a pin in display order (stems are filled in by the caller).
fn collect(pin: &Value) -> Vec<Media> {
    // Idea/story pins: pages of image and video blocks.
    let pages = pin["story_pin_data"]["pages"].as_array();
    let blocks: Vec<&Value> = pages.into_iter().flatten().flat_map(|p| p["blocks"].as_array().into_iter().flatten()).collect();
    let from_story: Vec<Media> = blocks
        .iter()
        .filter_map(|b| match b["type"].as_str()? {
            "story_pin_video_block" => video(&b["video"]),
            "story_pin_image_block" => image(&b["image"]["images"]),
            _ => None,
        })
        .collect();
    if !from_story.is_empty() {
        return from_story;
    }
    // Carousels: each slot is a picture or a clip.
    if let Some(slots) = pin["carousel_data"]["carousel_slots"].as_array() {
        let found: Vec<Media> = slots
            .iter()
            .filter_map(|s| video(&s["videos"]).or_else(|| image(&s["images"])))
            .collect();
        if !found.is_empty() {
            return found;
        }
    }
    if let Some(v) = video(&pin["videos"]) {
        return vec![v];
    }
    image(&pin["images"]).into_iter().collect()
}

fn media(kind: Kind, src_ext: &str, variants: Vec<Variant>, fetch: Fetch) -> Media {
    Media { kind, stem: String::new(), src_ext: src_ext.into(), variants, fetch, default_on: true }
}

/// From an object holding `video_list`: MP4 renditions, best first, or the
/// HLS stream when that's all there is.
fn video(v: &Value) -> Option<Media> {
    let list = v["video_list"].as_object()?;
    let mut mp4: Vec<&Value> = list
        .values()
        .filter(|r| r["url"].as_str().is_some_and(|u| u.ends_with(".mp4")))
        .collect();
    mp4.sort_by_key(|r| std::cmp::Reverse(r["width"].as_u64().unwrap_or(0)));
    let label = |r: &Value| match (r["width"].as_u64(), r["height"].as_u64()) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        _ => "MP4".into(),
    };
    let mut variants: Vec<Variant> = Vec::new();
    for r in mp4 {
        let (url, label) = (r["url"].as_str()?.to_string(), label(r));
        if !variants.iter().any(|v| v.label == label) {
            variants.push(Variant { url, label, height: None });
        }
    }
    if !variants.is_empty() {
        return Some(media(Kind::Video, "mp4", variants, Fetch::Http));
    }
    let hls = ["V_HLSV4", "V_HLSV3_WEB", "V_HLSV3_MOBILE"]
        .iter()
        .find_map(|k| list.get(*k))
        .or_else(|| list.values().find(|r| r["url"].as_str().is_some_and(|u| u.contains(".m3u8"))))?;
    let variant = Variant { url: hls["url"].as_str()?.to_string(), label: label(hls), height: None };
    Some(media(Kind::Video, "mp4", vec![variant], Fetch::Hls))
}

/// From an `images` map. The biggest copy lives under /originals/ even when
/// the map only lists resized ones (carousel slots).
fn image(images: &Value) -> Option<Media> {
    let map = images.as_object()?;
    let any = ["orig", "originals", "736x", "564x", "474x", "236x"]
        .iter()
        .find_map(|k| map.get(*k))
        .or_else(|| map.values().next())?;
    let url = any["url"].as_str()?;
    // https://i.pinimg.com/<size>/<a>/<b>/<c>/<hash>.<ext>
    let (base, rest) = url.split_once("pinimg.com/")?;
    let tail = rest.split_once('/')?.1;
    let ext = tail.rsplit('.').next().unwrap_or("jpg").to_ascii_lowercase();
    let ext = if ext == "jpeg" { "jpg".to_string() } else { ext };
    let at = |size: &str| format!("{base}pinimg.com/{size}/{tail}");
    if ext == "gif" {
        // Resized GIFs are still frames; only the original moves.
        let v = vec![Variant { url: at("originals"), label: "ORIG".into(), height: None }];
        return Some(media(Kind::Gif, "gif", v, Fetch::Http));
    }
    let variants = [("originals", "ORIG"), ("736x", "736"), ("474x", "474")]
        .iter()
        .map(|(size, label)| Variant { url: at(size), label: (*label).into(), height: None })
        .collect();
    Some(media(Kind::Image, &ext, variants, Fetch::Http))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_pin_links() {
        assert!(is_pinterest("https://www.pinterest.com/pin/664281013778109217/"));
        assert!(is_pinterest("https://th.pinterest.com/pin/1/"));
        assert!(is_pinterest("https://www.pinterest.co.uk/pin/1/"));
        assert!(is_pinterest("https://pin.it/72hPRLLfr"));
        assert!(!is_pinterest("https://i.pinimg.com/originals/a/b/c/d.jpg"));
        let id = PinRef::Id("664281013778109217".into());
        assert_eq!(pin_ref("https://www.pinterest.com/pin/664281013778109217/"), id);
        assert_eq!(pin_ref("https://pinterest.jp/pin/origami--664281013778109217/?x=1"), id);
        assert_eq!(pin_ref("https://www.pinterest.com/pin/e1_AfqPShxrP6CV/"), PinRef::Session);
        assert_eq!(pin_ref("https://www.pinterest.com/someone/board/"), PinRef::NotAPin);
    }

    /// Hits the real API: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn resolves_live_pins() {
        use crate::media::Kind::*;
        let agent = crate::job::agent();
        let tools = Tools::detect();
        let cases: [(&str, &[Kind], Fetch); 8] = [
            ("725220346239561090", &[Image], Fetch::Http),  // photo
            ("891994270143529394", &[Gif], Fetch::Http),    // animated GIF
            ("664281013778109217", &[Video], Fetch::Http),  // video pin
            ("1084663891475263837", &[Video], Fetch::Http), // idea pin, mp4
            ("606508274845593025", &[Video], Fetch::Hls),   // idea pin, HLS only
            ("1025272671423645004", &[Image], Fetch::Http), // idea pin, image
            ("893260907363406162", &[Video, Video, Image, Image, Image, Image, Video, Video, Video], Fetch::Http), // carousel
            ("441282463481903715", &[Video, Image], Fetch::YtDlp), // Vimeo embed + cover
        ];
        for (id, kinds, fetch) in cases {
            let url = format!("https://www.pinterest.com/pin/{id}/");
            let p = resolve(&agent, &tools, &url).unwrap_or_else(|e| panic!("{id}: {e}"));
            let got: Vec<Kind> = p.items.iter().map(|m| m.kind).collect();
            assert_eq!(got, kinds, "{id}");
            assert_eq!(p.items[0].fetch, fetch, "{id}");
            println!("{id} {} {:?}", p.title, p.items.iter().map(|m| &m.variants[0].label).collect::<Vec<_>>());
        }
    }
}
