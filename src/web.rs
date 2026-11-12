//! The pairing page, served on localhost while pairing: signing in to Google, the cURL
//! command pasted, then the emoji to tap on the phone.
//!
//! Reachable only from this computer, at a secret path (no other page, nor another user,
//! can post to it), for its own host name only (no DNS rebinding).

use std::cell::{Cell, RefCell};
use std::io;
use std::rc::Rc;
use std::time::Duration;

use futures::channel::mpsc::UnboundedSender;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::pair::{self, Pasted};

/// Largest request read: a cURL command with its cookies fits well.
const MAX_REQUEST: usize = 256 * 1024;
/// How long the page is still served once paired, for it to say so.
const LINGER: Duration = Duration::from_secs(10);

/// Where pairing is, as the page shows it.
#[derive(Clone)]
enum Stage {
    Waiting,
    Asking,
    Emoji(String),
    Failed(String),
    Paired,
    Done,
}

/// The page, served.
pub struct Page {
    url: String,
    stage: Rc<RefCell<Stage>>,
    server: JoinHandle<()>,
    done: Cell<bool>,
}

impl Page {
    /// Serves the page on a free port; what is pasted in it goes to `paste`.
    pub async fn serve(paste: UnboundedSender<Pasted>) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let host = format!("127.0.0.1:{}", listener.local_addr()?.port());
        let mut secret = [0u8; 16];
        getrandom::fill(&mut secret).map_err(|error| io::Error::other(error.to_string()))?;
        let token: String = secret.iter().map(|b| format!("{b:02x}")).collect();
        let url = format!("http://{host}/{token}");
        let stage = Rc::new(RefCell::new(Stage::Waiting));
        let server = {
            let stage = stage.clone();
            tokio::task::spawn_local(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    let (stage, paste) = (stage.clone(), paste.clone());
                    let (host, token) = (host.clone(), token.clone());
                    tokio::task::spawn_local(async move {
                        let _ = serve(stream, &host, &token, &stage, &paste).await;
                    });
                }
            })
        };
        Ok(Page {
            url,
            stage,
            server,
            done: Cell::new(false),
        })
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    pub fn asking(&self) {
        self.set(Stage::Asking);
    }

    pub fn emoji(&self, emoji: &str) {
        self.set(Stage::Emoji(emoji.to_string()));
    }

    pub fn failed(&self, message: &str) {
        self.set(Stage::Failed(message.to_string()));
    }

    pub fn paired(&self) {
        self.set(Stage::Paired);
    }

    /// Ready: says so, and stops serving a little later.
    pub fn done(&self) {
        self.done.set(true);
        self.set(Stage::Done);
        let server = self.server.abort_handle();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(LINGER).await;
            server.abort();
        });
    }

    fn set(&self, stage: Stage) {
        *self.stage.borrow_mut() = stage;
    }
}

impl Drop for Page {
    fn drop(&mut self) {
        if !self.done.get() {
            self.server.abort();
        }
    }
}

/// Answers one request.
async fn serve(
    mut stream: TcpStream,
    host: &str,
    token: &str,
    stage: &RefCell<Stage>,
    paste: &UnboundedSender<Pasted>,
) -> io::Result<()> {
    let Some((method, path, headers, body)) = read_request(&mut stream).await? else {
        return respond(&mut stream, 400, "text/plain", "Bad request").await;
    };
    let header = |name: &str| {
        headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    };
    if header("host") != Some(host) {
        return respond(&mut stream, 403, "text/plain", "Forbidden").await;
    }
    let Some(route) = path
        .strip_prefix('/')
        .and_then(|path| path.strip_prefix(token))
    else {
        return respond(&mut stream, 404, "text/plain", "Not found").await;
    };
    match (method.as_str(), route) {
        ("GET", "" | "/") => {
            let page = PAGE.replace("{SIGN_IN}", pair::SIGN_IN);
            respond(&mut stream, 200, "text/html; charset=utf-8", &page).await
        }
        ("GET", "/stage") => {
            let stage = match &*stage.borrow() {
                Stage::Waiting => json!({"stage": "waiting"}),
                Stage::Asking => json!({"stage": "asking"}),
                Stage::Emoji(emoji) => {
                    json!({"stage": "emoji", "emoji": emoji, "image": emoji_image(emoji)})
                }
                Stage::Failed(message) => json!({"stage": "failed", "message": message}),
                Stage::Paired => json!({"stage": "paired"}),
                Stage::Done => json!({"stage": "done"}),
            };
            respond(&mut stream, 200, "application/json", &stage.to_string()).await
        }
        ("POST", "/cookies") => {
            if matches!(
                *stage.borrow(),
                Stage::Asking | Stage::Emoji(_) | Stage::Paired | Stage::Done
            ) {
                return respond(&mut stream, 409, "text/plain", "Already pairing").await;
            }
            match pair::parse(&String::from_utf8_lossy(&body)) {
                Ok(cookies) => {
                    *stage.borrow_mut() = Stage::Asking;
                    let _ = paste.unbounded_send(Pasted::Cookies(cookies));
                    respond(&mut stream, 200, "text/plain", "OK").await
                }
                Err(e) => respond(&mut stream, 422, "text/plain", &e).await,
            }
        }
        _ => respond(&mut stream, 404, "text/plain", "Not found").await,
    }
}

type Request = (String, String, Vec<(String, String)>, Vec<u8>);

/// Method, path, headers and body; `None` if it is not HTTP or too large.
async fn read_request(stream: &mut TcpStream) -> io::Result<Option<Request>> {
    let mut data = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            break end;
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 || data.len() + n > MAX_REQUEST {
            return Ok(None);
        }
        data.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8_lossy(&data[..head_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(path)) = (request.next(), request.next()) else {
        return Ok(None);
    };
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .collect();
    let length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if head_end + 4 + length > MAX_REQUEST {
        return Ok(None);
    }
    let mut body = data.split_off(head_end + 4);
    while body.len() < length {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Ok(None);
        }
        body.extend_from_slice(&chunk[..n]);
    }
    body.truncate(length);
    Ok(Some((method.to_string(), path.to_string(), headers, body)))
}

async fn respond(stream: &mut TcpStream, status: u16, kind: &str, body: &str) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        _ => "Unprocessable Content",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.shutdown().await
}

/// The emoji as Google Messages draws it (Noto), as libgm links it.
fn emoji_image(emoji: &str) -> String {
    let code: Vec<String> = emoji.chars().map(|c| format!("{:x}", c as u32)).collect();
    format!(
        "https://fonts.gstatic.com/s/e/notoemoji/latest/{}/emoji.svg",
        code.join("_")
    )
}

const PAGE: &str = include_str!("pair.html");

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dropping_page_closes_listener() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let (paste, _) = futures::channel::mpsc::unbounded();
                let page = Page::serve(paste).await.unwrap();
                let port = url::Url::parse(page.url()).unwrap().port().unwrap();
                drop(page);
                tokio::task::yield_now().await;
                assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
            })
            .await;
    }

    #[test]
    fn emoji_image_like_libgm() {
        assert_eq!(
            emoji_image("🦊"),
            "https://fonts.gstatic.com/s/e/notoemoji/latest/1f98a/emoji.svg"
        );
        assert_eq!(
            emoji_image("❤️"),
            "https://fonts.gstatic.com/s/e/notoemoji/latest/2764_fe0f/emoji.svg"
        );
    }
}
