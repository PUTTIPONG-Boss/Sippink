//! Facebook and Instagram videos and reels, read by yt-dlp. Only posts a
//! logged-out visitor can watch work; anything else yt-dlp reports as
//! needing a login.

const HOSTS: [&str; 6] = ["facebook.com", "fb.watch", "fb.com", "instagram.com", "instagr.am", "threads.net"];

pub fn is_meta(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or(url).split(['/', '?', '#']).next().unwrap_or("");
    let host = host.to_ascii_lowercase();
    HOSTS.iter().any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// Facebook titles start with counters ("9.9K views · 341 reactions | ...");
/// keep the caption after them.
pub fn clean_title(title: &str) -> &str {
    let mut parts = title.splitn(2, " | ");
    match (parts.next(), parts.next()) {
        (Some(head), Some(rest)) if head.contains(" · ") && head.chars().next().is_some_and(|c| c.is_ascii_digit()) => {
            rest.trim()
        }
        _ => title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_meta_links() {
        assert!(is_meta("https://www.facebook.com/reel/1195289147628387"));
        assert!(is_meta("https://m.facebook.com/watch/?v=1"));
        assert!(is_meta("https://fb.watch/abc/"));
        assert!(is_meta("https://www.instagram.com/reel/C2RsuzcvKVt/"));
        assert!(!is_meta("https://notfacebook.com/x"));
        assert!(!is_meta("https://example.com/?u=facebook.com"));
    }

    #[test]
    fn drops_counters_from_titles() {
        assert_eq!(clean_title("9.9K views · 341 reactions | Slapathon | Beast Camp"), "Slapathon | Beast Camp");
        assert_eq!(clean_title("How to Share | With Friends"), "How to Share | With Friends");
    }
}
