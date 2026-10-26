//! X / Twitter extractor.
//!
//! Uses the public embed endpoint (`cdn.syndication.twimg.com/tweet-result`)
//! that powers embedded tweets, so no login or API key is needed. The `token`
//! query parameter is required but not validated. Tweets that need a login
//! (sensitive, age-restricted, protected) come back as `TweetTombstone`.

use serde_json::Value;

use crate::media::{Fetch, Kind, Media, Probe, Variant};

const HOSTS: [&str; 8] = [
    "x.com",
    "twitter.com",
    "mobile.x.com",
    "mobile.twitter.com",
    "fxtwitter.com",
    "vxtwitter.com",
    "fixupx.com",
    "fixvx.com",
];

/// Returns the tweet id if `url` points at a single tweet.
pub fn tweet_id(url: &str) -> Option<String> {
    let rest = url
        .trim()
        .strip_prefix("https://")
        .or_else(|| url.trim().strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    if !HOSTS.contains(&host) {
        return None;
    }
    let path = path.split(['?', '#']).next().unwrap_or("");
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    // <user>/status/<id>, i/status/<id>, i/web/status/<id> (+ optional /photo/1 etc.)
    let i = parts.iter().position(|p| *p == "status" || *p == "statuses")?;
    let id = parts.get(i + 1)?;
    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
        Some(id.to_string())
    } else {
        None
    }
}

pub fn resolve(agent: &ureq::Agent, id: &str) -> Result<Probe, String> {
    let url = format!("https://cdn.syndication.twimg.com/tweet-result?id={id}&token=a&lang=en");
    let tweet: Value = match agent.get(&url).call() {
        Ok(r) => serde_json::from_reader(r.into_reader()).map_err(|e| e.to_string())?,
        Err(ureq::Error::Status(404, _)) => return Err("post not found or deleted".into()),
        Err(e) => return Err(e.to_string()),
    };
    if tweet["__typename"] == "TweetTombstone" {
        return Err("this post needs a login to view (sensitive or protected)".into());
    }
    let mut post = &tweet;
    let mut found = media_of(post);
    // A text-only quote of a media post: take the quoted post's media.
    if found.is_empty() && tweet["quoted_tweet"].is_object() {
        post = &tweet["quoted_tweet"];
        found = media_of(post);
    }
    if found.is_empty() {
        return Err("no photo, video or GIF in this post".into());
    }
    let user = post["user"]["screen_name"].as_str().unwrap_or("x");
    let pid = post["id_str"].as_str().unwrap_or(id);
    let many = found.len() > 1;
    let items = found
        .into_iter()
        .enumerate()
        .map(|(n, mut m)| {
            m.stem = if many {
                format!("{user}_{pid}_{}", n + 1)
            } else {
                format!("{user}_{pid}")
            };
            m
        })
        .collect();
    Ok(Probe { title: format!("@{user} on X"), items, note: String::new() })
}

/// Every media entity of one tweet object (stems are filled in by the caller).
fn media_of(tweet: &Value) -> Vec<Media> {
    let mut entities: Vec<Value> = tweet["mediaDetails"].as_array().cloned().unwrap_or_default();
    // Ads / app cards keep their media inside a JSON string. Only take the
    // media the card displays, not e.g. the app-store icon.
    if let Some(card) = tweet["card"]["binding_values"]["unified_card"]["string_value"].as_str() {
        if let Ok(v) = serde_json::from_str::<Value>(card) {
            for key in card_media_keys(&v) {
                if let Some(m) = v["media_entities"].get(&key) {
                    entities.push(m.clone());
                }
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    entities
        .iter()
        .filter(|m| m["id_str"].as_str().map_or(true, |id| seen.insert(id.to_string())))
        .filter_map(to_media)
        .collect()
}

fn card_media_keys(card: &Value) -> Vec<String> {
    let mut keys = Vec::new();
    let Some(objs) = card["component_objects"].as_object() else {
        return keys;
    };
    for o in objs.values() {
        match o["type"].as_str() {
            Some("media") => keys.extend(o["data"]["id"].as_str().map(String::from)),
            Some("swipeable_media") => {
                for m in o["data"]["media_list"].as_array().into_iter().flatten() {
                    keys.extend(m["id"].as_str().map(String::from));
                }
            }
            _ => {}
        }
    }
    keys
}

fn to_media(m: &Value) -> Option<Media> {
    let media = |kind, src_ext: &str, variants| Media {
        kind,
        stem: String::new(),
        src_ext: src_ext.into(),
        variants,
        fetch: Fetch::Http,
        default_on: true,
        subdir: String::new(),
    };
    match m["type"].as_str()? {
        "photo" => {
            let base = m["media_url_https"].as_str()?;
            let (stem, ext) = base.rsplit_once('.')?;
            let ext = if ext == "jpeg" { "jpg" } else { ext };
            let variants = ["orig", "large", "small"]
                .iter()
                .map(|size| Variant {
                    url: format!("{stem}?format={ext}&name={size}"),
                    label: size.to_ascii_uppercase(),
                    height: None,
                })
                .collect();
            Some(media(Kind::Image, ext, variants))
        }
        // X serves GIFs as silent mp4 loops.
        t @ ("video" | "animated_gif") => {
            let mut mp4: Vec<&Value> = m["video_info"]["variants"]
                .as_array()?
                .iter()
                .filter(|v| v["content_type"] == "video/mp4")
                .collect();
            mp4.sort_by_key(|v| std::cmp::Reverse(v["bitrate"].as_u64().unwrap_or(0)));
            let variants: Vec<Variant> = mp4
                .iter()
                .filter_map(|v| {
                    let url = v["url"].as_str()?.to_string();
                    Some(Variant { label: resolution(&url).unwrap_or("MP4").to_string(), url, height: None })
                })
                .collect();
            if variants.is_empty() {
                return None;
            }
            let kind = if t == "video" { Kind::Video } else { Kind::Gif };
            Some(media(kind, "mp4", variants))
        }
        _ => None,
    }
}

/// "1280x720" out of ".../vid/avc1/1280x720/abc.mp4".
fn resolution(url: &str) -> Option<&str> {
    url.split('/').find(|seg| {
        seg.split_once('x').is_some_and(|(w, h)| {
            !w.is_empty() && !h.is_empty() && w.bytes().chain(h.bytes()).all(|c| c.is_ascii_digit())
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{resolution, tweet_id};

    #[test]
    fn reads_resolution() {
        let u = "https://video.twimg.com/amplify_video/1/vid/avc1/1280x720/abc.mp4?tag=14";
        assert_eq!(resolution(u), Some("1280x720"));
        assert_eq!(resolution("https://video.twimg.com/tweet_video/HUlF.mp4"), None);
    }

    #[test]
    fn parses_tweet_links() {
        let id = Some("1932109706354733077".to_string());
        assert_eq!(tweet_id("https://x.com/gdldev/status/1932109706354733077"), id);
        assert_eq!(tweet_id("https://twitter.com/gdldev/status/1932109706354733077?s=20"), id);
        assert_eq!(tweet_id("https://mobile.twitter.com/i/web/status/1932109706354733077"), id);
        assert_eq!(tweet_id("https://www.x.com/gdldev/status/1932109706354733077/photo/1"), id);
        assert_eq!(tweet_id("https://fxtwitter.com/gdldev/status/1932109706354733077"), id);
        assert_eq!(tweet_id("https://x.com/gdldev"), None);
        assert_eq!(tweet_id("https://example.com/a/status/1"), None);
    }

    /// Hits the real endpoint: `cargo test -- --ignored`
    #[test]
    #[ignore]
    fn resolves_live_posts() {
        use crate::media::Kind::*;
        let agent = crate::job::agent();
        let cases = [
            ("2022644523311206885", vec![Image]),              // photo
            ("2099006046543241430", vec![Video]),              // video up to 4K
            ("2110294793750507700", vec![Gif, Gif, Gif, Gif]), // four GIFs
            ("604341487988576256", vec![Image]),
            ("1932109706354733077", vec![Video, Video]),
            ("2001950365332455490", vec![Video]), // video inside an app card
        ];
        for (id, kinds) in cases {
            let p = super::resolve(&agent, id).unwrap_or_else(|e| panic!("{id}: {e}"));
            let got: Vec<_> = p.items.iter().map(|m| m.kind).collect();
            assert_eq!(got, kinds, "{id}");
            for m in &p.items {
                let labels: Vec<&str> = m.variants.iter().map(|v| v.label.as_str()).collect();
                println!("{id} {} {:?} {} {labels:?}", p.title, m.kind, m.stem);
                let r = agent.head(&m.variants[0].url).call().unwrap_or_else(|e| panic!("{id}: {e}"));
                assert_eq!(r.status(), 200);
            }
        }
    }
}
