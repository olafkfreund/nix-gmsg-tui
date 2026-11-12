//! Pairing through native Google sign-in or the local pairing page (see [`web`]).

use std::collections::BTreeMap;
use std::io;

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};

use crate::web;

/// The cookies libgm needs (mautrix-gmessages asks the same), the last one optional.
const COOKIES: [&str; 7] = [
    "SID",
    "HSID",
    "SSID",
    "APISID",
    "SAPISID",
    "OSID",
    "__Secure-1PSIDTS",
];
const OPTIONAL: usize = 1;

pub type Cookies = BTreeMap<String, String>;

pub const SIGN_IN: &str =
    "https://accounts.google.com/AccountChooser?continue=https://messages.google.com/web/config";

/// What the user gives, in the terminal or the browser.
pub enum Pasted {
    Cookies(Cookies),
    Quit,
    Failed(String),
    /// The sign-in window could not open, or stopped: why. Pairing goes on from the browser.
    #[cfg_attr(not(feature = "wry-signin"), allow(dead_code))]
    Unavailable(String),
}

/// Selects native sign-in by default, with a manual fallback: asked for (`--manual`), or when
/// the window cannot open.
pub enum Pairing {
    Manual(ManualPairing),
    #[cfg(feature = "wry-signin")]
    Native(crate::signin::Native),
}

impl Pairing {
    pub async fn start(manual: bool) -> io::Result<Self> {
        #[cfg(feature = "wry-signin")]
        if !manual
            && has_display()
            && let Ok(native) = crate::signin::Native::start("")
        {
            return Ok(Self::Native(native));
        }
        #[cfg(not(feature = "wry-signin"))]
        let _ = manual;
        ManualPairing::start().await.map(Self::Manual)
    }

    /// Whether pairing is from the browser (cookies pasted), not in the sign-in window.
    pub fn is_manual(&self) -> bool {
        matches!(self, Self::Manual(_))
    }

    pub async fn next(&mut self) -> Option<Pasted> {
        match self {
            Self::Manual(pairing) => pairing.next().await,
            #[cfg(feature = "wry-signin")]
            Self::Native(pairing) => pairing.next().await,
        }
    }

    pub fn page(&self) -> Option<&web::Page> {
        match self {
            Self::Manual(pairing) => Some(pairing.page()),
            #[cfg(feature = "wry-signin")]
            Self::Native(_) => None,
        }
    }

    pub fn finish(self) {
        match self {
            Self::Manual(pairing) => pairing.finish(),
            #[cfg(feature = "wry-signin")]
            Self::Native(_) => {}
        }
    }
}

/// A pairing page served on localhost, which also shows the emoji to tap.
pub struct ManualPairing {
    pasted: UnboundedReceiver<Pasted>,
    page: web::Page,
}

impl ManualPairing {
    pub async fn start() -> io::Result<Self> {
        let (paste, pasted) = unbounded();
        let page = web::Page::serve(paste).await?;
        Ok(Self { pasted, page })
    }

    pub async fn next(&mut self) -> Option<Pasted> {
        self.pasted.next().await
    }

    pub fn page(&self) -> &web::Page {
        &self.page
    }

    pub fn url(&self) -> &str {
        self.page.url()
    }

    pub fn finish(self) {
        self.page.done();
    }
}

/// The cookies in `text`: a cURL command (Copy as cURL, from Chrome or Firefox), a `Cookie:`
/// header, or a JSON object.
pub fn parse(text: &str) -> Result<Cookies, String> {
    let text = text.trim();
    let all: Cookies = if text.starts_with('{') {
        serde_json::from_str(text).map_err(|e| format!("Not a JSON object of cookies: {e}"))?
    } else {
        pairs(curl_cookies(text).unwrap_or(text))
    };
    let missing: Vec<_> = COOKIES[..COOKIES.len() - OPTIONAL]
        .iter()
        .filter(|name| all.get(**name).is_none_or(|value| value.trim().is_empty()))
        .copied()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "Missing cookies: {} (copy the request to messages.google.com, signed in)",
            missing.join(", ")
        ));
    }
    // Only these: the session keeps them, no more than it needs.
    Ok(all
        .into_iter()
        .filter(|(name, _)| COOKIES.contains(&name.as_str()))
        .collect())
}

/// The cookies of a cURL command: `-b '…'`, `--cookie '…'` or `-H 'Cookie: …'`.
fn curl_cookies(curl: &str) -> Option<&str> {
    for (i, _) in curl.match_indices(['\'', '"']) {
        let quote = &curl[i..i + 1];
        let before = curl[..i].trim_end();
        let rest = &curl[i + 1..];
        let value = &rest[..rest.find(quote)?];
        if before.ends_with(" -b") || before.ends_with(" --cookie") {
            return Some(value);
        }
        if (before.ends_with(" -H") || before.ends_with(" --header"))
            && let Some((name, cookies)) = value.split_once(':')
            && name.trim().eq_ignore_ascii_case("cookie")
        {
            return Some(cookies);
        }
    }
    None
}

/// `name=value; name=value`, with or without a `Cookie:` in front.
fn pairs(header: &str) -> Cookies {
    let header = header.trim();
    let header = match header.split_once(':') {
        Some((name, rest)) if name.trim().eq_ignore_ascii_case("cookie") => rest,
        _ => header,
    };
    header
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .filter(|(name, _)| !name.is_empty())
        .collect()
}

/// Whether a window can open: on Linux and the BSDs, a Wayland or X11 display is set.
#[cfg(feature = "wry-signin")]
fn has_display() -> bool {
    if cfg!(any(target_os = "macos", target_os = "windows")) {
        return true;
    }
    ["WAYLAND_DISPLAY", "DISPLAY"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COOKIE: &str = "SID=s; HSID=h; SSID=ss; APISID=a; SAPISID=sa; OSID=o; NID=n";

    fn check(cookies: Cookies) {
        assert_eq!(cookies["SID"], "s");
        assert_eq!(cookies["OSID"], "o");
        assert!(!cookies.contains_key("NID"), "only the cookies libgm needs");
        assert_eq!(cookies.len(), 6);
    }

    #[test]
    fn chrome_curl() {
        let curl = format!(
            "curl 'https://messages.google.com/web/config' \\\n  -H 'accept: */*' \\\n  -b '{COOKIE}' \\\n  -H 'user-agent: x'"
        );
        check(parse(&curl).unwrap());
    }

    #[test]
    fn firefox_curl() {
        let curl = format!(
            "curl 'https://messages.google.com/web/config' -H 'Accept: */*' -H 'Cookie: {COOKIE}' -H 'TE: trailers'"
        );
        check(parse(&curl).unwrap());
    }

    #[test]
    fn header_and_json() {
        check(parse(&format!("Cookie: {COOKIE}")).unwrap());
        check(parse(COOKIE).unwrap());
        let json = r#"{"SID":"s","HSID":"h","SSID":"ss","APISID":"a","SAPISID":"sa","OSID":"o"}"#;
        check(parse(json).unwrap());
    }

    #[test]
    fn empty_required_cookie_is_missing() {
        let error = parse(&COOKIE.replace("OSID=o", "OSID= ")).unwrap_err();
        assert!(error.contains("OSID"));
    }

    #[test]
    fn missing() {
        let e = parse("curl 'https://x' -b 'SID=s; HSID=h'").unwrap_err();
        assert!(e.contains("SSID, APISID, SAPISID, OSID"), "{e}");
    }
}
