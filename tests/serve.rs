use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Fixture {
    root: std::path::PathBuf,
    child: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "gmessages-serve-{}-{}",
            std::process::id(),
            rand_id()
        ));
        for name in ["data", "cache", "runtime"] {
            let path = root.join(name);
            fs::create_dir_all(&path).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { root, child: None }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gmessages-tui"));
        command
            .arg("serve")
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CACHE_HOME", self.root.join("cache"))
            .env("XDG_RUNTIME_DIR", self.root.join("runtime"))
            .env("GMESSAGES_TUI_PASSPHRASE", "test")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        command
    }

    fn socket(&self) -> std::path::PathBuf {
        self.root.join("runtime/gmessages-tui/daemon.sock")
    }
    fn media(&self) -> std::path::PathBuf {
        self.root.join("runtime/gmessages-tui/media")
    }

    fn start(&mut self) {
        self.child = Some(self.command().spawn().unwrap());
        let until = Instant::now() + Duration::from_secs(5);
        while !self.socket().exists() {
            assert!(Instant::now() < until, "daemon did not create socket");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn connect(&self) -> Client {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(stream) = UnixStream::connect(self.socket()) {
                return Client::new(stream);
            }
            assert!(Instant::now() < until, "daemon did not accept connections");
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn signal(&mut self, signal: i32) {
        unsafe {
            libc::kill(self.child.as_ref().unwrap().id() as i32, signal);
        }
    }

    fn wait(&mut self) {
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            if self.child.as_mut().unwrap().try_wait().unwrap().is_some() {
                return;
            }
            assert!(
                Instant::now() < until,
                "daemon did not exit within five seconds"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn rand_id() -> u64 {
    let mut bytes = [0; 8];
    getrandom::fill(&mut bytes).unwrap();
    u64::from_ne_bytes(bytes)
}

struct Client {
    write: UnixStream,
    read: BufReader<UnixStream>,
}

impl Client {
    fn new(stream: UnixStream) -> Self {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            read: BufReader::new(stream.try_clone().unwrap()),
            write: stream,
        }
    }

    fn recv(&mut self) -> Value {
        let mut line = String::new();
        assert!(self.read.read_line(&mut line).unwrap() > 0, "socket closed");
        serde_json::from_str(&line).unwrap()
    }

    fn raw(&mut self, line: &str) {
        self.write.write_all(line.as_bytes()).unwrap();
        self.write.write_all(b"\n").unwrap();
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.raw(&json!({"id": id, "method": method, "params": params}).to_string());
        loop {
            let line = self.recv();
            if line.get("id") == Some(&json!(id)) {
                return line;
            }
        }
    }

    fn hello(&mut self) {
        assert_eq!(self.recv()["data"]["protocol"], 1);
        assert_eq!(
            self.request(0, "hello", json!({"protocol": 1, "role": "app"}))["ok"],
            true
        );
    }
}

#[test]
fn protocol_and_login_methods() {
    let mut fixture = Fixture::new();
    fixture.start();
    let mut client = fixture.connect();
    client.hello();
    assert_eq!(
        client.request(1, "state", json!({}))["data"]["state"],
        "login"
    );
    client.raw("garbage");
    assert_eq!(client.recv()["error"]["code"], "bad_request");
    assert_eq!(client.request(2, "state", json!({}))["ok"], true);
    assert_eq!(
        client.request(3, "send", json!({"conversation":"c", "text":"hello"}))["error"]["code"],
        "not_paired"
    );
    assert_eq!(
        client.request(4, "pair.cancel", json!({}))["data"],
        json!({})
    );
    assert_eq!(
        client.request(
            5,
            "conversations",
            json!({"folder":"inbox", "offset":0, "limit":50})
        )["data"],
        json!({"items":[], "total":0})
    );

    let mut mismatch = fixture.connect();
    assert_eq!(mismatch.recv()["data"]["protocol"], 1);
    assert_eq!(
        mismatch.request(0, "hello", json!({"protocol":2, "role":"app"}))["error"]["code"],
        "protocol_mismatch"
    );
    assert_eq!(
        mismatch.request(1, "state", json!({}))["error"]["code"],
        "protocol_mismatch"
    );

    let mut long = fixture.connect();
    long.hello();
    long.raw(&"x".repeat(1024 * 1024 + 1));
    let mut line = String::new();
    assert_eq!(long.read.read_line(&mut line).unwrap(), 0);
    let mut fresh = fixture.connect();
    fresh.hello();
    assert_eq!(fresh.request(1, "state", json!({}))["ok"], true);
}

#[test]
fn one_instance_stale_socket_and_sigterm_cleanup() {
    let mut fixture = Fixture::new();
    fixture.start();
    fixture.connect().hello();
    let second = fixture.command().output().unwrap();
    assert_eq!(second.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&second.stderr).contains("already running"));

    fixture.signal(libc::SIGKILL);
    fixture.wait();
    assert!(fixture.socket().exists());
    fixture.start();
    fixture.connect().hello();
    fs::write(fixture.media().join("old"), b"old").unwrap();
    fixture.signal(libc::SIGTERM);
    fixture.wait();
    assert!(!fixture.socket().exists());
    assert_eq!(fs::read_dir(fixture.media()).unwrap().count(), 0);
}
