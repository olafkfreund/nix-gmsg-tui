use super::{Page, classify, credentials};
use crate::pair::{self, Pasted};
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use std::io::{self, BufRead, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::WindowBuilder;
use wry::{PageLoadEvent, WebViewBuilder};

/// Owns the private window. Drop always kills AND reaps, including error/retry paths.
pub struct Native {
    child: Child,
    received: UnboundedReceiver<Pasted>,
}

impl Native {
    pub fn start(_intro: &str) -> io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command.arg("--internal-google-signin");
        Self::spawn(&mut command)
    }

    fn spawn(command: &mut Command) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // WebKit diagnostics can include URLs. Only our fixed protocol errors escape.
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child.stdout.take().expect("piped stdout");
        let (send, received) = unbounded();
        std::thread::spawn(move || {
            for line in io::BufReader::new(stdout).lines() {
                let pasted = match line {
                    Ok(line) => match line.as_str() {
                        "refused" => Pasted::Failed("Google refused this embedded browser. Restart with --manual (or --qr, if your phone offers it).".into()),
                        "closed" => Pasted::Quit,
                        "missing" => Pasted::Failed("Google Messages loaded, but required authentication cookies are missing. Restart with --manual (or --qr, if your phone offers it).".into()),
                        "failed" => Pasted::Unavailable("Cannot open the Google sign-in window".into()),
                        _ => match credentials(&line) {
                            Ok(cookies) => Pasted::Cookies(cookies),
                            Err(error) => Pasted::Failed(error),
                        },
                    },
                    Err(_) => Pasted::Failed("Cannot read native sign-in response".into()),
                };
                if send.unbounded_send(pasted).is_err() {
                    break;
                }
            }
        });
        Ok(Self { child, received })
    }

    pub async fn next(&mut self) -> Option<Pasted> {
        Some(
            self.received
                .next()
                .await
                .unwrap_or_else(|| Pasted::Unavailable("The Google sign-in window stopped".into())),
        )
    }
}

impl Drop for Native {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn emit(message: &str) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

enum SigninEvent {
    Loaded(String),
    ParentGone,
}

/// Must run before Tokio: Tao owns this subprocess's main thread.
pub fn run_child() {
    #[cfg(target_os = "linux")]
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: no other threads exist yet.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
    if child_window().is_err() {
        emit("failed");
    }
}

fn child_window() -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoopBuilder::<SigninEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let parent_proxy = proxy.clone();
    std::thread::spawn(move || {
        // This pipe is not the terminal. EOF also closes the window if the parent dies.
        let mut input = String::new();
        let _ = io::stdin().read_line(&mut input);
        let _ = parent_proxy.send_event(SigninEvent::ParentGone);
    });
    let window = WindowBuilder::new()
        .with_title("Google Messages - sign in")
        .build(&event_loop)?;
    let builder = WebViewBuilder::new()
        .with_incognito(true)
        .with_url(pair::SIGN_IN)
        .with_on_page_load_handler(move |event, url| {
            if matches!(event, PageLoadEvent::Finished) {
                let _ = proxy.send_event(SigninEvent::Loaded(url));
            }
        });
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    let webview = builder.build(&window)?;
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().ok_or("no GTK window")?)?
    };
    let mut sent = false;
    let mut retry_at = None;
    let mut attempts = 0;
    event_loop.run(move |event, _, flow| {
        *flow = retry_at.map_or(ControlFlow::Wait, ControlFlow::WaitUntil);
        match event {
            Event::UserEvent(SigninEvent::ParentGone) => *flow = ControlFlow::Exit,
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                emit("closed");
                *flow = ControlFlow::Exit;
            }
            Event::UserEvent(SigninEvent::Loaded(url)) if !sent => match classify(&url) {
                Page::Messages => {
                    retry_at.get_or_insert(Instant::now());
                }
                Page::Refused => {
                    emit("refused");
                    *flow = ControlFlow::Exit;
                }
                Page::Other => {
                    retry_at = None;
                }
            },
            _ => {}
        }
        if !sent && retry_at.is_some_and(|at| Instant::now() >= at) {
            // Recheck current location, not only a stale page-load notification.
            let trusted = webview
                .url()
                .is_ok_and(|url| classify(&url) == Page::Messages);
            if !trusted {
                retry_at = None;
                return;
            }
            attempts += 1;
            #[cfg(target_os = "macos")]
            let cookies = webview.cookies().map(|cookies| {
                cookies
                    .into_iter()
                    .filter(|cookie| super::messages_cookie_scope(cookie.domain(), cookie.path()))
                    .collect::<Vec<_>>()
            });
            #[cfg(not(target_os = "macos"))]
            let cookies = webview.cookies_for_url("https://messages.google.com/web/config");
            let cookies = cookies.ok().map(|cookies| {
                cookies
                    .into_iter()
                    .map(|cookie| (cookie.name().to_owned(), cookie.value().to_owned()))
                    .collect::<pair::Cookies>()
            });
            let valid = cookies.and_then(|cookies| {
                credentials(&serde_json::to_string(&cookies).expect("string map")).ok()
            });
            if let Some(cookies) = valid {
                emit(&serde_json::to_string(&cookies).expect("string map"));
                sent = true;
                retry_at = None;
                window.set_visible(false);
            } else if attempts >= 10 {
                emit("missing");
                retry_at = None;
                *flow = ControlFlow::Exit;
            } else {
                retry_at = Some(Instant::now() + Duration::from_secs(1));
            }
        }
        if !matches!(*flow, ControlFlow::Exit) {
            *flow = retry_at.map_or(ControlFlow::Wait, ControlFlow::WaitUntil);
        }
        // Keep the private webview and its Google session alive until the parent stops us.
        let _ = &webview;
    });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn helper(script: &str) -> Native {
        Native::spawn(Command::new("sh").arg("-c").arg(script)).unwrap()
    }

    #[tokio::test]
    async fn subprocess_delivers_validated_cookies_without_browser() {
        let mut native = helper(
            r#"printf '%s\n' '{"SID":"s","HSID":"h","SSID":"ss","APISID":"a","SAPISID":"sa","OSID":"o","SOCS":"unused"}'; read -r line"#,
        );
        let event = tokio::time::timeout(Duration::from_secs(3), native.next())
            .await
            .unwrap();
        let Some(Pasted::Cookies(cookies)) = event else {
            panic!("expected cookies");
        };
        assert_eq!(cookies.len(), 6);
        assert_eq!(cookies["OSID"], "o");
        assert!(!cookies.contains_key("SOCS"));
        assert!(native.child.try_wait().unwrap().is_none());
    }

    #[tokio::test]
    async fn closed_window_cancels_pairing() {
        let mut native = helper("printf 'closed\\n'");
        let event = tokio::time::timeout(Duration::from_secs(3), native.next())
            .await
            .unwrap();
        assert!(matches!(event, Some(Pasted::Quit)));
    }

    #[tokio::test]
    async fn unexpected_subprocess_exit_is_reported() {
        let mut native = helper("exit 1");
        let event = tokio::time::timeout(Duration::from_secs(3), native.next())
            .await
            .unwrap();
        assert!(matches!(event, Some(Pasted::Unavailable(_))));
    }

    #[tokio::test]
    async fn window_that_cannot_open_is_reported() {
        let mut native = helper("printf 'failed\\n'");
        let event = tokio::time::timeout(Duration::from_secs(3), native.next())
            .await
            .unwrap();
        assert!(matches!(event, Some(Pasted::Unavailable(_))));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dropping_pairing_kills_and_reaps_subprocess() {
        let native = helper("read -r line");
        let proc = std::path::PathBuf::from(format!("/proc/{}", native.child.id()));
        assert!(proc.exists());
        drop(native);
        assert!(
            !proc.exists(),
            "child must not survive as a running process or zombie"
        );
    }
}
