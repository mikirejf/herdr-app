//! What `browser.screenshot` and `browser.clear_data` decide before and after
//! Chromium answers: the file a screenshot goes to, the PNG out of the DevTools
//! answer, the origins whose site storage a clear reaches.

use std::path::PathBuf;

use base64::Engine as _;
use serde_json::Value;

/// `http(s)://host[:port]` of a page's url, the form `Storage.clearDataForOrigin`
/// takes; None for any other scheme (about:blank, file:).
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !matches!(scheme, "http" | "https") {
        return None;
    }
    let authority = rest.split(['/', '?', '#', '\\']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    (!host.is_empty()).then(|| format!("{scheme}://{host}"))
}

/// The page's host for a file name: letters, digits, dots and dashes only
/// (a port or an IPv6 address becomes dashes); "page" when it has none.
fn host_name(url: &str) -> String {
    let host = origin_of(url).map(|o| o.split_once("://").map(|(_, h)| h.to_owned()).unwrap_or_default()).unwrap_or_default();
    let host = host.rsplit_once(':').filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit())).map_or(host.as_str(), |(h, _)| h);
    let clean: String = host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' }).collect();
    let clean = clean.trim_matches(['.', '-']).to_owned();
    if clean.is_empty() { "page".into() } else { clean }
}

/// Where a screenshot goes: the `path` argument (absolute, or `~/…`), else
/// `~/Downloads/<host>-<YYYYMMDD-HHMMSS>.png`.
pub fn screenshot_path(arg: Option<&str>, url: &str, home: &str, stamp: &str) -> Result<PathBuf, String> {
    let Some(arg) = arg.map(str::trim).filter(|a| !a.is_empty()) else {
        return Ok(PathBuf::from(home).join("Downloads").join(format!("{}-{stamp}.png", host_name(url))));
    };
    let path = match arg.strip_prefix("~/") {
        Some(rest) => PathBuf::from(home).join(rest),
        None => PathBuf::from(arg),
    };
    if !path.is_absolute() {
        return Err(format!("path must be absolute or start with ~/, got {arg:?}"));
    }
    if path.is_dir() {
        return Err(format!("{} is a folder: give a file name", path.display()));
    }
    Ok(path)
}

/// `YYYYMMDD-HHMMSS` in local time.
#[cfg(all(target_os = "macos", feature = "browser"))]
pub fn local_stamp() -> String {
    // SAFETY: time and localtime_r with a zeroed out-struct of the right type.
    let tm = unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        tm
    };
    format!("{:04}{:02}{:02}-{:02}{:02}{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min, tm.tm_sec)
}

/// The PNG bytes of a `Page.captureScreenshot` result.
pub fn png_of(result: &Value) -> Result<Vec<u8>, String> {
    let data = result.get("data").and_then(Value::as_str).ok_or("Chromium's screenshot came back without data")?;
    base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| format!("Chromium's screenshot was not valid base64: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn origins_are_what_clear_data_for_origin_takes() {
        assert_eq!(origin_of("https://user:pw@example.com:8443/a/b?q=1#x").as_deref(), Some("https://example.com:8443"));
        assert_eq!(origin_of("http://127.0.0.1:8765/browser-fixture.html").as_deref(), Some("http://127.0.0.1:8765"));
        assert_eq!(origin_of("about:blank"), None);
        assert_eq!(origin_of("file:///etc/hosts"), None);
        assert_eq!(origin_of("https://"), None);
    }

    #[test]
    fn a_default_screenshot_lands_in_downloads_named_after_the_host() {
        let p = screenshot_path(None, "https://www.example.com:8443/x?y", "/Users/me", "20261101-203045").unwrap();
        assert_eq!(p, PathBuf::from("/Users/me/Downloads/www.example.com-20261101-203045.png"));
        let p = screenshot_path(Some("  "), "http://127.0.0.1:8765/", "/Users/me", "20261101-203045").unwrap();
        assert_eq!(p, PathBuf::from("/Users/me/Downloads/127.0.0.1-20261101-203045.png"));
        let p = screenshot_path(None, "about:blank", "/Users/me", "20261101-203045").unwrap();
        assert_eq!(p, PathBuf::from("/Users/me/Downloads/page-20261101-203045.png"));
    }

    #[test]
    fn a_given_path_is_absolute_or_under_home() {
        assert_eq!(screenshot_path(Some("~/shots/a.png"), "https://a.com", "/Users/me", "s").unwrap(), PathBuf::from("/Users/me/shots/a.png"));
        assert_eq!(screenshot_path(Some("/tmp/a.png"), "https://a.com", "/Users/me", "s").unwrap(), PathBuf::from("/tmp/a.png"));
        assert!(screenshot_path(Some("a.png"), "https://a.com", "/Users/me", "s").unwrap_err().contains("absolute"));
        assert!(screenshot_path(Some("/tmp"), "https://a.com", "/Users/me", "s").unwrap_err().contains("folder"));
    }

    #[test]
    fn a_screenshot_result_decodes_to_png_bytes() {
        assert_eq!(png_of(&json!({"data": "iVBORw0K"})).unwrap(), vec![0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a]);
        assert!(png_of(&json!({})).is_err());
        assert!(png_of(&json!({"data": "!!"})).is_err());
    }
}
