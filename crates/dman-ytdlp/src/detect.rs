use url::Url;

/// Offline heuristic: does this URL look like it belongs to a video/audio site yt-dlp
/// handles? No network access. Direct file links and unknown sites return `false`; the daemon
/// offers a "try as video" probe for those.
pub fn is_probably_video_url(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let on = |d: &str| host == d || host.ends_with(&format!(".{d}"));
    let path = url.path();
    let has_content = path.len() > 1 || url.query().is_some();

    // Domains that are (almost) entirely media.
    const MEDIA: &[&str] = &[
        "youtube.com",
        "youtu.be",
        "youtube-nocookie.com",
        "vimeo.com",
        "dailymotion.com",
        "dai.ly",
        "twitch.tv",
        "soundcloud.com",
        "bilibili.com",
        "b23.tv",
        "tiktok.com",
        "bandcamp.com",
        "v.redd.it",
        "streamable.com",
        "rumble.com",
        "odysee.com",
        "nicovideo.jp",
        "mixcloud.com",
        "kick.com",
        "fb.watch",
        "ted.com",
        "vk.com",
        "ok.ru",
    ];
    if MEDIA.iter().any(|d| on(d)) {
        return has_content;
    }
    if on("x.com") || on("twitter.com") {
        return path.contains("/status/");
    }
    if on("reddit.com") {
        return path.contains("/comments/");
    }
    if on("instagram.com") {
        return ["/p/", "/reel/", "/reels/", "/tv/", "/stories/"]
            .iter()
            .any(|p| path.starts_with(p));
    }
    if on("facebook.com") {
        return path.starts_with("/watch")
            || path.contains("/videos/")
            || path.starts_with("/reel/")
            || path.starts_with("/share/v/")
            || path.starts_with("/share/r/");
    }
    if on("archive.org") {
        return path.starts_with("/details/");
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> bool {
        is_probably_video_url(&Url::parse(s).unwrap())
    }

    #[test]
    fn known_video_sites() {
        for u in [
            "https://www.youtube.com/watch?v=abc",
            "https://m.youtube.com/shorts/abc",
            "https://music.youtube.com/watch?v=abc",
            "https://youtu.be/abc",
            "https://vimeo.com/12345",
            "https://www.dailymotion.com/video/x1",
            "https://www.twitch.tv/videos/1",
            "https://soundcloud.com/a/b",
            "https://www.bilibili.com/video/BV1",
            "https://www.tiktok.com/@a/video/1",
            "https://x.com/user/status/123",
            "https://twitter.com/user/status/123",
            "https://www.reddit.com/r/x/comments/abc/t/",
            "https://www.instagram.com/reel/abc/",
            "https://www.facebook.com/watch?v=1",
            "https://artist.bandcamp.com/track/x",
            "https://archive.org/details/foo",
        ] {
            assert!(v(u), "{u}");
        }
    }

    #[test]
    fn non_video() {
        for u in [
            "https://example.com/file.zip",
            "https://www.youtube.com/",
            "https://x.com/user",
            "https://www.reddit.com/r/rust",
            "https://archive.org/download/foo/foo.zip",
            "https://www.facebook.com/someone",
            "https://notyoutube.com/watch?v=1",
            "magnet:?xt=urn:btih:abc",
            "ftp://youtube.com/watch?v=1",
        ] {
            assert!(!v(u), "{u}");
        }
    }
}
