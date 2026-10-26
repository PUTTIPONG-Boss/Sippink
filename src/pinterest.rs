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
use crate::job::Ctx;
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

/// What a Pinterest link points at.
#[derive(Debug, PartialEq)]
enum Target {
    Pin(String),
    /// `/pin/e1_...`: a logged-out search token, not fetchable.
    Session,
    Board { user: String, board: String },
    Section { user: String, board: String, section: String },
    /// `created`: the user's own uploads; otherwise everything they saved.
    Profile { user: String, created: bool },
    Unsupported,
}

/// First path segments that are site pages, not usernames.
const RESERVED: [&str; 12] = [
    "search", "ideas", "today", "explore", "business", "settings", "categories", "topics",
    "videos", "shopping", "login", "_",
];

fn target(url: &str) -> Target {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let parts: Vec<String> = path
        .split('/')
        .skip(3)
        .filter(|p| !p.is_empty())
        .map(crate::files::percent_decode)
        .collect();
    let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
    match parts.as_slice() {
        ["pin", seg, ..] => {
            // Newer links can read /pin/some-title--1234567890/
            let id = seg.rsplit("--").next().unwrap_or(seg);
            if id.starts_with("e1_") {
                Target::Session
            } else if !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()) {
                Target::Pin(id.to_string())
            } else {
                Target::Unsupported
            }
        }
        [user, ..] if RESERVED.contains(user) => Target::Unsupported,
        [user] | [user, "_saved" | "_profile" | "pins" | "boards"] => {
            Target::Profile { user: user.to_string(), created: false }
        }
        [user, "_created"] => Target::Profile { user: user.to_string(), created: true },
        [_, "_shop" | "_tried" | "following" | "followers" | "_community", ..] => Target::Unsupported,
        [user, board] => Target::Board { user: user.to_string(), board: board.to_string() },
        [user, board, section] if !section.starts_with('_') && *section != "more_ideas" => Target::Section {
            user: user.to_string(),
            board: board.to_string(),
            section: section.to_string(),
        },
        [user, board, ..] => Target::Board { user: user.to_string(), board: board.to_string() },
        [] => Target::Unsupported,
    }
}

/// `GET /resource/<name>Resource/get/` with the options the site sends.
fn call(agent: &ureq::Agent, resource: &str, options: &Value) -> Result<Value, String> {
    let data = serde_json::json!({ "options": options, "context": {} }).to_string();
    let resp = agent
        .get(&format!("https://www.pinterest.com/resource/{resource}Resource/get/"))
        .query("source_url", "")
        .query("data", &data)
        .set("X-Pinterest-PWS-Handler", "www/[username].js")
        .set("Accept", "application/json")
        .call();
    match resp {
        Ok(r) => serde_json::from_reader(r.into_reader()).map_err(|e| e.to_string()),
        Err(ureq::Error::Status(404, _)) => Err("not found, deleted, or private".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// Walks a paged resource. `page` gets each page's pins and returns false to
/// stop early. Stops on cancel too.
fn paginate(
    agent: &ureq::Agent,
    resource: &str,
    mut options: Value,
    ctx: &Ctx,
    mut page: impl FnMut(Vec<Value>) -> bool,
) -> Result<(), String> {
    loop {
        if ctx.cancelled() {
            return Err("cancelled".into());
        }
        let body = call(agent, resource, &options)?;
        let data = &body["resource_response"]["data"];
        let list = data.as_array().or(data["results"].as_array()).cloned().unwrap_or_default();
        let pins = list.into_iter().filter(|p| p["type"] == "pin").collect();
        if !page(pins) {
            return Ok(());
        }
        // The next-page token comes back inside the echoed request options.
        let next = &body["resource"]["options"]["bookmarks"];
        let done = next[0]
            .as_str()
            .map_or(true, |b| b == "-end-" || b.starts_with("Y2JOb25lO"));
        if done {
            return Ok(());
        }
        options["bookmarks"] = next.clone();
    }
}

pub fn resolve(agent: &ureq::Agent, tools: &Tools, url: &str, ctx: &Ctx) -> Result<Probe, String> {
    let mut url = url.trim().to_string();
    if host_of(&url).as_deref() == Some("pin.it") {
        // Short links redirect to the full pin / board URL.
        let resp = agent.get(&url).call().map_err(|e| e.to_string())?;
        url = resp.get_url().to_string();
    }
    match target(&url) {
        Target::Pin(id) => resolve_pin(agent, tools, &id),
        Target::Session => Err("that's a temporary search link: open the pin, press Share, copy the link".into()),
        Target::Board { user, board } => resolve_board(agent, &user, &board, ctx),
        Target::Section { user, board, section } => resolve_section(agent, &user, &board, &section, ctx),
        Target::Profile { user, created } => resolve_profile(agent, &user, created, ctx),
        Target::Unsupported => Err("open a pin, a board, or a profile and copy that link".into()),
    }
}

fn resolve_pin(agent: &ureq::Agent, tools: &Tools, id: &str) -> Result<Probe, String> {
    let options = serde_json::json!({ "field_set_key": "unauth_react_main_pin", "id": id });
    let body = call(agent, "Pin", &options).map_err(|e| format!("pin {e}"))?;
    let pin = &body["resource_response"]["data"];
    if !pin.is_object() {
        return Err("pin not found".into());
    }
    let user = owner(pin);
    let items = pin_items(pin, "");
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
    Ok(Probe { title, items, note: String::new() })
}

fn owner(pin: &Value) -> &str {
    pin["native_creator"]["username"]
        .as_str()
        .or(pin["pinner"]["username"].as_str())
        .unwrap_or("pinterest")
}

/// Same pin, whichever feed it came from: logged-out board feeds hand out
/// `e1_...` ids while profile feeds give numeric ones, but the image
/// signature is the same in both.
fn pin_key(pin: &Value) -> Option<String> {
    pin["image_signature"].as_str().or(pin["id"].as_str()).map(String::from)
}

/// Numeric pin id for file names, else a short image signature.
fn name_id(pin: &Value) -> String {
    match pin["id"].as_str() {
        Some(id) if id.bytes().all(|b| b.is_ascii_digit()) => id.to_string(),
        _ => pin["image_signature"].as_str().unwrap_or("pin").chars().take(12).collect(),
    }
}

/// A pin's media, named `<owner>_<pin id>[_n]` and filed under `subdir`.
fn pin_items(pin: &Value, subdir: &str) -> Vec<Media> {
    let id = name_id(pin);
    let user = owner(pin);
    let mut items = collect(pin);
    let many = items.len() > 1;
    for (n, m) in items.iter_mut().enumerate() {
        m.stem = if many { format!("{user}_{id}_{}", n + 1) } else { format!("{user}_{id}") };
        m.subdir = subdir.to_string();
    }
    items
}

/// Media of many pins, each pin once.
fn gather(pins: &[Value], subdir: impl Fn(&Value) -> String) -> Vec<Media> {
    let mut seen = std::collections::HashSet::new();
    pins.iter()
        .filter(|p| pin_key(p).is_some_and(|k| seen.insert(k)))
        .flat_map(|p| pin_items(p, &subdir(p)))
        .collect()
}

fn folder_name(s: &str) -> String {
    let s = sanitize(s);
    s.chars().take(80).collect::<String>().trim().to_string()
}

fn summary(n_pins: usize, items: &[Media]) -> String {
    let count = |k| items.iter().filter(|m| m.kind == k).count();
    format!(
        "{n_pins} pins: {} images, {} GIFs, {} videos",
        count(Kind::Image),
        count(Kind::Gif),
        count(Kind::Video)
    )
}

/// How many of the owner's newest pins a board search looks through before
/// giving up (about two minutes); `Ctx::deep` lifts it.
const SEARCH_BUDGET: usize = 5_000;

fn resolve_board(agent: &ureq::Agent, user: &str, slug: &str, ctx: &Ctx) -> Result<Probe, String> {
    let options = serde_json::json!({ "username": user, "slug": slug, "field_set_key": "board_header" });
    let body = call(agent, "Board", &options).map_err(|e| format!("board {e}"))?;
    let board = &body["resource_response"]["data"];
    let board_id = board["id"].as_str().ok_or("board not found")?.to_string();
    let name = board["name"].as_str().unwrap_or(slug).trim().to_string();
    let owner = user.to_string();
    let expected = board["pin_count"].as_u64().unwrap_or(0) as usize;
    let born = board["created_at"].as_str().and_then(date_key);

    let mut pins: Vec<Value> = Vec::new();
    let feed = serde_json::json!({
        "board_id": board_id, "field_set_key": "react_grid_pin", "prepend": false, "bookmarks": null,
    });
    paginate(agent, "BoardFeed", feed, ctx, |page| {
        pins.extend(page);
        (ctx.progress)(format!("{name}: {} of {expected} pins", pins.len()));
        true
    })?;

    // Logged out, the board feed often stops early (24-200 pins). The
    // owner's full pin list pages all the way, newest first, and says which
    // board each pin is on, so finish from there. Nothing on the board is
    // older than the board, which bounds the search for young boards; old
    // boards of busy accounts hit the budget instead.
    let mut note = String::new();
    if pins.len() * 100 < expected * 95 {
        let total = call(agent, "User", &serde_json::json!({ "username": owner, "field_set_key": "profile" }))
            .ok()
            .and_then(|u| u["resource_response"]["data"]["pin_count"].as_u64());
        let of_total = total.map_or(String::new(), |t| format!(" of {t}"));
        let mut have: std::collections::HashSet<String> = pins.iter().filter_map(pin_key).collect();
        let mut scanned = 0;
        let mut out_of_budget = false;
        let all = serde_json::json!({
            "username": owner, "is_own_profile_pins": false, "field_set_key": "grid_item",
            "pin_filter": null, "page_size": 100,
        });
        paginate(agent, "UserPins", all, ctx, |page| {
            scanned += page.len();
            let last_date = page.last().and_then(|p| p["created_at"].as_str()).and_then(date_key);
            for p in page {
                let on_board = p["board"]["id"].as_str() == Some(board_id.as_str());
                if !on_board {
                    continue;
                }
                match pin_key(&p) {
                    // Seen in the board feed under an e1_ id: keep this
                    // copy instead, its numeric id makes a better name.
                    Some(k) if !have.insert(k.clone()) => {
                        if let Some(old) = pins.iter_mut().find(|o| pin_key(o).as_ref() == Some(&k)) {
                            *old = p;
                        }
                    }
                    _ => pins.push(p),
                }
            }
            (ctx.progress)(format!(
                "{name}: {} of {expected} pins (searched {scanned}{of_total} of @{owner}'s pins)",
                pins.len()
            ));
            out_of_budget = !ctx.deep && scanned >= SEARCH_BUDGET;
            let before_board = matches!((born, last_date), (Some(b), Some(l)) if l < b);
            pins.len() < expected && !out_of_budget && !before_board
        })?;
        if pins.len() < expected {
            note = if out_of_budget {
                format!(
                    "found {} of {expected} pins in @{owner}'s newest {scanned}{of_total}; SEARCH ALL looks further (slow)",
                    pins.len()
                )
            } else {
                format!("Pinterest shows {} of {expected} pins (the rest may be deleted or hidden)", pins.len())
            };
        }
    }
    let dir = folder_name(&name);
    let items = gather(&pins, |_| dir.clone());
    if items.is_empty() {
        return Err("no pins found on this board".into());
    }
    Ok(Probe { title: format!("{name} | board by @{owner} | {}", summary(pins.len(), &items)), items, note })
}

/// "Sat, 11 Feb 2012 01:23:35 +0000" as a sortable number (UTC; Pinterest
/// always sends +0000).
fn date_key(s: &str) -> Option<u64> {
    let mut it = s.split_whitespace().skip(1);
    let day: u64 = it.next()?.parse().ok()?;
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let mon = it.next()?;
    let month = MONTHS.iter().position(|m| *m == mon)? as u64 + 1;
    let year: u64 = it.next()?.parse().ok()?;
    let hms: Vec<u64> = it.next()?.split(':').filter_map(|n| n.parse().ok()).collect();
    let [h, m, sec] = hms[..] else { return None };
    Some((((year * 100 + month) * 100 + day) * 100 + h) * 10_000 + m * 100 + sec)
}

fn resolve_section(agent: &ureq::Agent, user: &str, board: &str, section: &str, ctx: &Ctx) -> Result<Probe, String> {
    let options = serde_json::json!({ "username": user, "board_slug": board, "section_slug": section });
    let body = call(agent, "BoardSection", &options).map_err(|e| format!("section {e}"))?;
    let sec = &body["resource_response"]["data"];
    let section_id = sec["id"].as_str().ok_or("section not found")?.to_string();
    let title = sec["title"].as_str().unwrap_or(section).to_string();
    let expected = sec["pin_count"].as_u64().unwrap_or(0) as usize;
    let mut pins = Vec::new();
    paginate(agent, "BoardSectionPins", serde_json::json!({ "section_id": section_id }), ctx, |page| {
        pins.extend(page);
        (ctx.progress)(format!("{title}: {} of {expected} pins", pins.len()));
        true
    })?;
    let note = if pins.len() < expected {
        format!("Pinterest shows {} of {expected} pins while logged out", pins.len())
    } else {
        String::new()
    };
    let dir = format!("{}/{}", folder_name(board), folder_name(&title));
    let items = gather(&pins, |_| dir.clone());
    if items.is_empty() {
        return Err("no pins found in this section".into());
    }
    Ok(Probe { title: format!("{title} | section of {board} | {}", summary(pins.len(), &items)), items, note })
}

fn resolve_profile(agent: &ureq::Agent, user: &str, created: bool, ctx: &Ctx) -> Result<Probe, String> {
    let (resource, options) = if created {
        ("UserActivityPins", serde_json::json!({
            "username": user, "exclude_add_pin_rep": true, "field_set_key": "grid_item",
            "is_own_profile_pins": false,
        }))
    } else {
        ("UserPins", serde_json::json!({
            "username": user, "is_own_profile_pins": false, "field_set_key": "grid_item",
            "pin_filter": null, "page_size": 100,
        }))
    };
    let mut pins = Vec::new();
    paginate(agent, resource, options, ctx, |page| {
        pins.extend(page);
        (ctx.progress)(format!("@{user}: {} pins so far", pins.len()));
        true
    })?;
    let root = folder_name(user);
    // Saved pins land in one folder per board, created ones together.
    let items = if created {
        let dir = format!("{root}/created");
        gather(&pins, |_| dir.clone())
    } else {
        gather(&pins, |p| {
            let board = p["board"]["name"].as_str().unwrap_or("other");
            format!("{root}/{}", folder_name(board))
        })
    };
    if items.is_empty() {
        return Err(format!("@{user} has no public pins"));
    }
    let what = if created { "created by" } else { "saved by" };
    Ok(Probe { title: format!("pins {what} @{user} | {}", summary(pins.len(), &items)), items, note: String::new() })
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
    Media {
        kind,
        stem: String::new(),
        src_ext: src_ext.into(),
        variants,
        fetch,
        default_on: true,
        subdir: String::new(),
    }
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
    fn orders_dates() {
        let a = date_key("Sat, 11 Feb 2012 01:23:35 +0000").unwrap();
        let b = date_key("Tue, 30 Jun 2026 01:31:05 +0000").unwrap();
        let c = date_key("Sat, 11 Feb 2012 01:23:36 +0000").unwrap();
        assert!(a < c && c < b);
        assert_eq!(date_key("nonsense"), None);
    }

    #[test]
    fn recognises_pin_links() {
        assert!(is_pinterest("https://www.pinterest.com/pin/664281013778109217/"));
        assert!(is_pinterest("https://th.pinterest.com/pin/1/"));
        assert!(is_pinterest("https://www.pinterest.co.uk/pin/1/"));
        assert!(is_pinterest("https://pin.it/72hPRLLfr"));
        assert!(!is_pinterest("https://i.pinimg.com/originals/a/b/c/d.jpg"));
        let id = Target::Pin("664281013778109217".into());
        assert_eq!(target("https://www.pinterest.com/pin/664281013778109217/"), id);
        assert_eq!(target("https://pinterest.jp/pin/origami--664281013778109217/?x=1"), id);
        assert_eq!(target("https://www.pinterest.com/pin/e1_AfqPShxrP6CV/"), Target::Session);
        let board = |u: &str, b: &str| Target::Board { user: u.into(), board: b.into() };
        assert_eq!(target("https://www.pinterest.com/someone/my-board/"), board("someone", "my-board"));
        assert_eq!(target("https://www.pinterest.com/a/b/c/"), Target::Section { user: "a".into(), board: "b".into(), section: "c".into() });
        assert_eq!(target("https://www.pinterest.com/a/b/_tools/more-ideas/"), board("a", "b"));
        let profile = |u: &str, c| Target::Profile { user: u.into(), created: c };
        assert_eq!(target("https://www.pinterest.com/someone/"), profile("someone", false));
        assert_eq!(target("https://www.pinterest.com/someone/_saved/"), profile("someone", false));
        assert_eq!(target("https://www.pinterest.com/someone/_created/"), profile("someone", true));
        assert_eq!(target("https://www.pinterest.com/search/pins/?q=cat"), Target::Unsupported);
        assert_eq!(target("https://www.pinterest.com/ideas/"), Target::Unsupported);
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
            let p = resolve(&agent, &tools, &url, &Ctx::quiet()).unwrap_or_else(|e| panic!("{id}: {e}"));
            let got: Vec<Kind> = p.items.iter().map(|m| m.kind).collect();
            assert_eq!(got, kinds, "{id}");
            assert_eq!(p.items[0].fetch, fetch, "{id}");
            println!("{id} {} {:?}", p.title, p.items.iter().map(|m| &m.variants[0].label).collect::<Vec<_>>());
        }
    }
}
