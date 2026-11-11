//! One local daemon owns the encrypted store and fans phone events out to socket clients.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json, to_value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};

use crate::{VERSION, key, pair, paths, phone, proto, store, sync};

const MAX_LINE: usize = 1024 * 1024;
const QUEUE: usize = 1000;

enum Incoming {
    Line(u64, String),
    Closed(u64),
    Phone(phone::Event),
    Pasted(pair::Pasted),
    PairDone,
}

enum PairStatus {
    Asking,
    Emoji(String),
    Failed(String),
    Paired,
}

struct Client {
    tx: mpsc::Sender<String>,
    reader: JoinHandle<()>,
    writer: JoinHandle<()>,
    handshaken: bool,
    mismatch: bool,
}

struct Pending {
    conn: u64,
    id: u64,
    until: Instant,
}

struct Daemon {
    clients: HashMap<u64, Client>,
    pending: HashMap<u64, Pending>,
    next_conn: u64,
    next_rid: u64,
    state: proto::State,
    detail: Option<String>,
    sync: Option<sync::Sync>,
    commands: Option<phone::Commands>,
    control: Option<phone::Control>,
    pairing: Option<JoinHandle<()>>,
    pair_status: Option<mpsc::Sender<PairStatus>>,
    data: PathBuf,
    tx: mpsc::Sender<Incoming>,
}

pub fn run() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("cannot start runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let result = tokio::task::LocalSet::new().block_on(&runtime, serve());
    if let Err(error) = result {
        eprintln!("{error}");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

async fn serve() -> io::Result<()> {
    let data = paths::data()?;
    let cache = paths::cache()?;
    let runtime = paths::runtime()?;
    for dir in [&data, &cache, &runtime] {
        paths::create_private_dir(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let _lock = lock(&data.join("lock"))?;
    let media = runtime.join("media");
    clear_media(&media)?;
    let socket = runtime.join("daemon.sock");
    if socket.exists() {
        std::fs::remove_file(&socket)?;
    }
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let result = serve_bound(listener, data, &media).await;
    let _ = std::fs::remove_file(&socket);
    let _ = clear_media(&media);
    result
}

async fn serve_bound(listener: UnixListener, data: PathBuf, _media: &Path) -> io::Result<()> {
    trim_log(&data.join("phone.log"))?;
    phone::log::init(&data.join("phone.log"));
    let (tx, mut rx) = mpsc::channel(QUEUE);
    let mut daemon = Daemon {
        clients: HashMap::new(),
        pending: HashMap::new(),
        next_conn: 1,
        next_rid: 1,
        state: proto::State::Starting,
        detail: None,
        sync: None,
        commands: None,
        control: None,
        pairing: None,
        pair_status: None,
        data,
        tx,
    };
    daemon.try_key().await;
    let mut retry = tokio::time::interval_at(
        Instant::now() + Duration::from_secs(30),
        Duration::from_secs(30),
    );
    let mut deadlines = tokio::time::interval(Duration::from_millis(100));
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                if stream.peer_cred()?.uid() == unsafe { libc::geteuid() } {
                    daemon.accept(stream);
                }
            }
            incoming = rx.recv() => {
                if let Some(incoming) = incoming { daemon.receive(incoming).await; }
            }
            _ = retry.tick() => {
                if matches!(daemon.state, proto::State::Locked | proto::State::KeyringMissing) { daemon.try_key().await; }
            }
            _ = deadlines.tick() => daemon.expire(),
            _ = term.recv() => break,
            _ = interrupt.recv() => break,
        }
    }
    if let Some(pairing) = daemon.pairing.take() {
        pairing.abort();
    }
    daemon.pair_status = None;
    daemon.clients.clear();
    if let Some(control) = daemon.control.take() {
        let _ = timeout(Duration::from_secs(4), control.stop()).await;
    }
    Ok(())
}

impl Daemon {
    fn state_data(&self) -> proto::StateData {
        proto::StateData {
            state: self.state,
            detail: self.detail.clone(),
            unread: self.sync.as_ref().map_or(0, sync::Sync::unread),
        }
    }

    fn set_state(&mut self, state: proto::State, detail: Option<String>) {
        self.state = state;
        self.detail = detail.clone();
        self.broadcast(proto::EventLine::new(
            "state",
            &proto::StateEvent { state, detail },
        ));
    }

    async fn try_key(&mut self) {
        let db = self.data.join("messages.db");
        let key = tokio::task::spawn_blocking(move || key::store_key(&db)).await;
        let key = match key {
            Ok(Ok(key)) => key,
            Ok(Err(error)) => {
                let state = match error {
                    key::KeyError::Missing(_) | key::KeyError::Refused(_) => {
                        proto::State::KeyringMissing
                    }
                    _ => proto::State::Locked,
                };
                let detail = match error {
                    key::KeyError::Empty => "empty_passphrase".to_owned(),
                    _ => error.to_string(),
                };
                self.set_state(state, Some(detail));
                return;
            }
            Err(error) => {
                self.set_state(proto::State::KeyringMissing, Some(error.to_string()));
                return;
            }
        };
        match store::Store::open(&self.data.join("messages.db"), &key) {
            Ok(store) => {
                let phone = phone::Phone::start(&self.data.join("session.json"));
                let (mut events, commands, control) = phone.split();
                self.sync = Some(sync::Sync::new(Rc::new(store), commands.clone()));
                self.commands = Some(commands);
                self.control = Some(control);
                self.set_state(proto::State::Login, None);
                let tx = self.tx.clone();
                tokio::task::spawn_local(async move {
                    while let Some(event) = events.next().await {
                        if tx.send(Incoming::Phone(event)).await.is_err() {
                            break;
                        }
                    }
                });
            }
            Err(error) => self.set_state(proto::State::StoreUnreadable, Some(error.to_string())),
        }
    }

    fn accept(&mut self, stream: UnixStream) {
        let conn = self.next_conn;
        self.next_conn += 1;
        let (mut read, mut write) = stream.into_split();
        let (tx, mut rx) = mpsc::channel::<String>(QUEUE);
        let hello = proto::EventLine::new(
            "hello",
            &proto::HelloEvent {
                protocol: proto::PROTOCOL,
                version: VERSION.to_owned(),
                state: self.state,
                detail: self.detail.clone(),
                unread: self.state_data().unread,
            },
        );
        let _ = tx.try_send(serde_json::to_string(&hello).expect("hello serializes"));
        let writer = tokio::task::spawn_local(async move {
            while let Some(line) = rx.recv().await {
                if write.write_all(line.as_bytes()).await.is_err()
                    || write.write_all(b"\n").await.is_err()
                {
                    break;
                }
            }
        });
        let events = self.tx.clone();
        let reader = tokio::task::spawn_local(async move {
            let mut chunk = [0u8; 8192];
            let mut line = Vec::new();
            'read: loop {
                let count = match read.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                for byte in &chunk[..count] {
                    if *byte == b'\n' {
                        let value =
                            String::from_utf8(std::mem::take(&mut line)).unwrap_or_default();
                        if events.send(Incoming::Line(conn, value)).await.is_err() {
                            break 'read;
                        }
                    } else {
                        line.push(*byte);
                        if line.len() > MAX_LINE {
                            break 'read;
                        }
                    }
                }
            }
            let _ = events.send(Incoming::Closed(conn)).await;
        });
        self.clients.insert(
            conn,
            Client {
                tx,
                reader,
                writer,
                handshaken: false,
                mismatch: false,
            },
        );
    }

    fn close(&mut self, conn: u64) {
        if let Some(client) = self.clients.remove(&conn) {
            client.reader.abort();
            client.writer.abort();
        }
        self.pending.retain(|_, pending| pending.conn != conn);
        if let Some(sync) = &mut self.sync {
            sync.drop_conn(conn);
        }
    }

    fn write<T: serde::Serialize>(&mut self, conn: u64, value: &T) {
        let Ok(line) = serde_json::to_string(value) else {
            self.close(conn);
            return;
        };
        if line.len() > MAX_LINE
            || self
                .clients
                .get(&conn)
                .is_some_and(|c| c.tx.try_send(line).is_err())
        {
            self.close(conn);
        }
    }

    fn broadcast(&mut self, event: proto::EventLine) {
        let clients: Vec<_> = self
            .clients
            .iter()
            .filter(|(_, c)| c.handshaken)
            .map(|(&id, _)| id)
            .collect();
        for conn in clients {
            self.write(conn, &event);
        }
    }

    fn outputs(&mut self, outputs: Vec<proto::Out>) {
        for output in outputs {
            match output {
                proto::Out::Broadcast(event) => {
                    if event.event == "state"
                        && let Ok(data) =
                            serde_json::from_value::<proto::StateEvent>(event.data.clone())
                    {
                        self.state = data.state;
                        self.detail = data.detail;
                    }
                    self.broadcast(event);
                }
                proto::Out::Reply { rid, result } => {
                    if let Some(pending) = self.pending.remove(&rid) {
                        self.write(
                            pending.conn,
                            &match result {
                                Ok(data) => proto::Reply::ok(Some(pending.id), data),
                                Err(error) => {
                                    proto::Reply::err(Some(pending.id), error.code, error.message)
                                }
                            },
                        );
                    }
                }
            }
        }
    }

    fn expire(&mut self) {
        let expired: Vec<_> = self
            .pending
            .iter()
            .filter(|(_, p)| p.until <= Instant::now())
            .map(|(&rid, _)| rid)
            .collect();
        for rid in expired {
            if let Some(pending) = self.pending.remove(&rid) {
                self.write(
                    pending.conn,
                    &proto::Reply::err(
                        Some(pending.id),
                        proto::ErrorCode::Timeout,
                        "request timed out",
                    ),
                );
            }
        }
    }

    async fn receive(&mut self, incoming: Incoming) {
        match incoming {
            Incoming::Closed(conn) => self.close(conn),
            Incoming::PairDone => {
                self.pairing = None;
                self.pair_status = None;
            }
            Incoming::Phone(event) => {
                if let Some(status) = &self.pair_status {
                    let status_event = match &event {
                        phone::Event::Emoji { emoji } => Some(PairStatus::Emoji(emoji.clone())),
                        phone::Event::PairFailed { message } => {
                            Some(PairStatus::Failed(message.clone()))
                        }
                        phone::Event::Paired => Some(PairStatus::Paired),
                        _ => None,
                    };
                    if let Some(status_event) = status_event {
                        let _ = status.try_send(status_event);
                    }
                }
                if let Some(sync) = &mut self.sync {
                    let outputs = sync.handle(event);
                    self.outputs(outputs);
                }
            }
            Incoming::Pasted(pasted) => match pasted {
                pair::Pasted::Cookies(cookies) => {
                    if let Some(status) = &self.pair_status {
                        let _ = status.try_send(PairStatus::Asking);
                    }
                    if let Some(commands) = &self.commands {
                        commands.send(phone::Command::PairGoogle { cookies });
                    }
                }
                pair::Pasted::Unavailable(message) => {
                    self.pairing = None;
                    self.pair_status = None;
                    self.set_state(proto::State::Login, None);
                    self.broadcast(proto::EventLine::new(
                        "pairing",
                        &proto::PairingEvent {
                            message: Some(message),
                            ..Default::default()
                        },
                    ));
                    if let Err(error) = self.start_pair(proto::PairMethod::Manual).await {
                        self.broadcast(proto::EventLine::new(
                            "pairing",
                            &proto::PairingEvent {
                                message: Some(error),
                                ..Default::default()
                            },
                        ));
                    }
                }
                pair::Pasted::Failed(message) => self.broadcast(proto::EventLine::new(
                    "pairing",
                    &proto::PairingEvent {
                        message: Some(message),
                        ..Default::default()
                    },
                )),
                pair::Pasted::Quit => {}
            },
            Incoming::Line(conn, line) => self.request(conn, &line).await,
        }
    }

    async fn request(&mut self, conn: u64, line: &str) {
        let request = match proto::parse_request(line) {
            Ok(request) => request,
            Err(reply) => {
                self.write(conn, &reply);
                return;
            }
        };
        let id = request.id;
        if request.method == "hello" {
            if self.clients.get(&conn).is_some_and(|c| c.mismatch) {
                self.write(
                    conn,
                    &proto::Reply::err(id, proto::ErrorCode::ProtocolMismatch, "protocol mismatch"),
                );
                return;
            }
            match params::<proto::HelloParams>(request.params) {
                Ok(hello) if hello.protocol == proto::PROTOCOL && id == Some(0) => {
                    if let Some(client) = self.clients.get_mut(&conn) {
                        client.handshaken = true;
                    }
                    self.write(conn, &proto::Reply::ok(id, json!({})));
                }
                Ok(_) => {
                    if let Some(client) = self.clients.get_mut(&conn) {
                        client.mismatch = true;
                    }
                    self.write(
                        conn,
                        &proto::Reply::err(
                            id,
                            proto::ErrorCode::ProtocolMismatch,
                            "protocol mismatch",
                        ),
                    );
                }
                Err(error) => self.write(
                    conn,
                    &proto::Reply::err(id, proto::ErrorCode::BadRequest, error),
                ),
            }
            return;
        }
        if self
            .clients
            .get(&conn)
            .is_none_or(|c| !c.handshaken || c.mismatch)
        {
            self.write(
                conn,
                &proto::Reply::err(id, proto::ErrorCode::ProtocolMismatch, "hello required"),
            );
            return;
        }
        let Some(id) = id else {
            self.write(conn, &proto::Reply::bad_request("id required"));
            return;
        };
        if self.sync.is_none()
            && !matches!(
                request.method.as_str(),
                "state" | "key.retry" | "pair.cancel"
            )
        {
            self.write(conn, &self.unready(Some(id)));
            return;
        }
        if self.pending.values().any(|p| p.conn == conn && p.id == id) {
            self.write(
                conn,
                &proto::Reply::err(Some(id), proto::ErrorCode::Busy, "id already pending"),
            );
            return;
        }
        let mut reply = Ok(json!({}));
        let mut outputs = Vec::new();
        match request.method.as_str() {
            "state" => reply = to_value(self.state_data()).map_err(|e| e.to_string()),
            "key.retry" => {
                if matches!(
                    self.state,
                    proto::State::Locked | proto::State::KeyringMissing
                ) && timeout(Duration::from_secs(15), self.try_key())
                    .await
                    .is_err()
                {
                    self.write(
                        conn,
                        &proto::Reply::err(
                            Some(id),
                            proto::ErrorCode::Timeout,
                            "key retry timed out",
                        ),
                    );
                    return;
                }
            }
            "pair.cancel" => {
                if let Some(pairing) = self.pairing.take() {
                    pairing.abort();
                }
                self.pair_status = None;
                if let Some(commands) = &self.commands {
                    commands.send(phone::Command::CancelPairing);
                }
                if self.state == proto::State::Pairing || self.pairing.is_some() {
                    self.set_state(proto::State::Login, None);
                }
            }
            "pair.start" => {
                if self.state == proto::State::Pairing {
                    self.write(
                        conn,
                        &proto::Reply::err(
                            Some(id),
                            proto::ErrorCode::Busy,
                            "pairing already in progress",
                        ),
                    );
                    return;
                }
                reply = match params::<proto::PairStartParams>(request.params) {
                    Ok(p) => {
                        match timeout(Duration::from_secs(15), self.start_pair(p.method)).await {
                            Ok(result) => result,
                            Err(_) => {
                                self.write(
                                    conn,
                                    &proto::Reply::err(
                                        Some(id),
                                        proto::ErrorCode::Timeout,
                                        "pairing timed out",
                                    ),
                                );
                                return;
                            }
                        }
                    }
                    Err(e) => Err(e),
                };
            }
            "conversations" => {
                reply = match params::<proto::ConversationsParams>(request.params) {
                    Ok(p) if p.offset >= 0 && (1..=200).contains(&p.limit) => self
                        .sync
                        .as_ref()
                        .map(|s| to_value(s.conversations(p.folder, p.offset, p.limit)).unwrap())
                        .ok_or_else(|| "store unavailable".to_owned()),
                    Ok(_) => Err("offset or limit out of range".into()),
                    Err(e) => Err(e),
                };
            }
            "history" => {
                reply = match params::<proto::ConversationParams>(request.params) {
                    Ok(p) if !p.conversation.is_empty() => self
                        .sync
                        .as_ref()
                        .map(|s| to_value(s.history(&p.conversation)).unwrap())
                        .ok_or_else(|| "store unavailable".into()),
                    Ok(_) => Err("conversation required".into()),
                    Err(e) => Err(e),
                }
            }
            "read" => {
                reply = match params::<proto::ConversationParams>(request.params) {
                    Ok(p) if !p.conversation.is_empty() => {
                        if let Some(sync) = &mut self.sync {
                            outputs = sync.read(&p.conversation);
                        }
                        Ok(json!({}))
                    }
                    Ok(_) => Err("conversation required".into()),
                    Err(e) => Err(e),
                }
            }
            "focus" => {
                reply = match params::<proto::FocusParams>(request.params) {
                    Ok(p) => {
                        if let Some(sync) = &mut self.sync {
                            sync.set_focus(conn, p.conversation);
                        }
                        Ok(json!({}))
                    }
                    Err(e) => Err(e),
                }
            }
            "send" => {
                reply = match params::<proto::SendParams>(request.params) {
                    Ok(p) if p.conversation.is_empty() || p.text.is_empty() => {
                        Err("conversation and text required".into())
                    }
                    Ok(p) if self.state == proto::State::Ready => {
                        let rid = self.next_rid;
                        self.next_rid += 1;
                        self.pending.insert(
                            rid,
                            Pending {
                                conn,
                                id,
                                until: Instant::now() + Duration::from_secs(120),
                            },
                        );
                        if let Some(sync) = &mut self.sync {
                            sync.send(rid, p.conversation, p.text, p.reply_to);
                        }
                        return;
                    }
                    Ok(_) => {
                        self.write(conn, &self.unready(Some(id)));
                        return;
                    }
                    Err(e) => Err(e),
                };
            }
            _ => {
                self.write(
                    conn,
                    &proto::Reply::err(Some(id), proto::ErrorCode::UnknownMethod, "unknown method"),
                );
                return;
            }
        }
        self.outputs(outputs);
        match reply {
            Ok(data) => self.write(conn, &proto::Reply::ok(Some(id), data)),
            Err(message) => self.write(
                conn,
                &proto::Reply::err(Some(id), proto::ErrorCode::BadRequest, message),
            ),
        }
    }

    fn unready(&self, id: Option<u64>) -> proto::Reply {
        let locked = matches!(
            self.state,
            proto::State::Locked | proto::State::KeyringMissing | proto::State::StoreUnreadable
        );
        proto::Reply::err(
            id,
            if locked {
                proto::ErrorCode::Locked
            } else {
                proto::ErrorCode::NotPaired
            },
            if locked {
                "store unavailable"
            } else {
                "phone not paired"
            },
        )
    }

    async fn start_pair(&mut self, method: proto::PairMethod) -> Result<Value, String> {
        if self.state == proto::State::Ready {
            return Err("already paired".into());
        }
        if self.sync.is_none() {
            return Err("store unavailable".into());
        }
        if self.pairing.is_some() {
            return Err("pairing already in progress".into());
        }
        match method {
            proto::PairMethod::Qr => {
                if let Some(commands) = &self.commands {
                    commands.send(phone::Command::PairQr);
                }
            }
            proto::PairMethod::Google | proto::PairMethod::Manual => {
                let mut pairing = pair::Pairing::start(method == proto::PairMethod::Manual)
                    .await
                    .map_err(|e| e.to_string())?;
                if let Some(page) = pairing.page() {
                    self.broadcast(proto::EventLine::new(
                        "pairing",
                        &proto::PairingEvent {
                            manual_url: Some(page.url().to_owned()),
                            ..Default::default()
                        },
                    ));
                }
                let tx = self.tx.clone();
                let (status_tx, mut status_rx) = mpsc::channel(16);
                self.pair_status = Some(status_tx);
                self.pairing = Some(tokio::task::spawn_local(async move {
                    loop {
                        tokio::select! {
                            pasted = pairing.next() => {
                                let Some(pasted) = pasted else { break };
                                let cookies = matches!(pasted, pair::Pasted::Cookies(_));
                                let unavailable = matches!(pasted, pair::Pasted::Unavailable(_));
                                if tx.send(Incoming::Pasted(pasted)).await.is_err() { return; }
                                if unavailable { return; }
                                if cookies { pairing.finish(); let _ = tx.send(Incoming::PairDone).await; return; }
                            }
                            status = status_rx.recv() => {
                                if let (Some(status), Some(page)) = (status, pairing.page()) {
                                    match status {
                                        PairStatus::Asking => page.asking(),
                                        PairStatus::Emoji(emoji) => page.emoji(&emoji),
                                        PairStatus::Failed(message) => page.failed(&message),
                                        PairStatus::Paired => page.paired(),
                                    }
                                }
                            }
                        }
                    }
                    let _ = tx.send(Incoming::PairDone).await;
                }));
            }
        }
        self.set_state(proto::State::Pairing, None);
        Ok(json!({}))
    }
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T, String> {
    serde_json::from_value(value).map_err(|e| e.to_string())
}

fn lock(path: &Path) -> io::Result<File> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        let mut pid = String::new();
        let _ = file.read_to_string(&mut pid);
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("already running (pid {})", pid.trim()),
        ));
    }
    file.set_len(0)?;
    file.write_all(std::process::id().to_string().as_bytes())?;
    file.sync_data()?;
    Ok(file)
}

fn clear_media(path: &Path) -> io::Result<()> {
    if path.exists() {
        std::fs::remove_dir_all(path)?;
    }
    paths::create_private_dir(path)
}

fn trim_log(path: &Path) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let len = file.metadata()?.len();
    if len <= MAX_LINE as u64 {
        return Ok(());
    }
    file.seek(SeekFrom::End(-(MAX_LINE as i64)))?;
    let mut tail = Vec::with_capacity(MAX_LINE);
    file.read_to_end(&mut tail)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&tail)
}
