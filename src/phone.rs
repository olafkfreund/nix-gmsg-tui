//! The phone, through Google Messages for web (gmessages-proto): pairs, keeps the session,
//! and turns what the phone says into events for the interface, and the interface's
//! commands into calls to the phone.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gmessages_proto as gm;
use serde::{Deserialize, Serialize};

/// What the phone reports.
#[derive(Debug)]
pub enum Event {
    /// No session: tell how to pair (`PairGoogle` or `PairQr`).
    Login,
    /// Scan this from the phone to pair.
    Qr {
        url: String,
    },
    /// Tap this emoji on the phone to pair.
    Emoji {
        emoji: String,
    },
    /// The Google session has ended: new cookies of the account sign in again.
    Expired {
        message: String,
    },
    /// Pairing failed: try again.
    PairFailed {
        message: String,
    },
    Paired,
    /// Connected and active: the phone answers now, the conversations follow.
    Connected,
    /// `since`: the time from which every conversation of the inbox is listed (0: all are).
    /// `partial`: the most recent ones only, all of them follow.
    Ready {
        conversations: Vec<Conversation>,
        since: i64,
        partial: bool,
    },
    /// The media of a message, for the `Download` command of the same `id`.
    Downloaded {
        id: u64,
        attachments: Vec<Attachment>,
        error: Option<String>,
    },
    /// A conversation looked up (`Lookup`) the phone no longer has.
    Gone {
        conversation: String,
    },
    /// The address book, for the `Contacts` command of the same `id`.
    Contacts {
        id: u64,
        contacts: Vec<Contact>,
        error: Option<String>,
    },
    /// The conversation started by the `StartChat` command of the same `id`.
    Started {
        id: u64,
        conversation: Option<Conversation>,
        error: Option<String>,
    },
    /// The `Archive`, `Restore`, `DeleteChat`, `DeleteMessage` or `React` command of the same
    /// `id` done, or failed.
    Done {
        id: u64,
        error: Option<String>,
    },
    /// The result of the `Send` command of the same `id`.
    Sent {
        id: u64,
        conversation: String,
        outcome: SendOutcome,
    },
    /// A message received, sent from the phone, or updated; `old` if not news.
    Message {
        conversation: String,
        message: Message,
        old: bool,
    },
    /// A conversation new, or changed.
    Conversation {
        conversation: Conversation,
    },
    /// Someone started or stopped typing.
    Typing {
        conversation: String,
        author: String,
        name: String,
        started: bool,
    },
    /// The last messages of a conversation, or why they could not be fetched.
    History {
        conversation: String,
        messages: Vec<Message>,
        error: Option<String>,
    },
    Status {
        message: String,
    },
    /// The connection to the phone has stopped for good.
    Fatal {
        message: String,
    },
    Unpaired {
        reason: String,
    },
}

#[derive(Debug)]
pub enum SendOutcome {
    Sent(Message),
    Accepted(Message),
    Unavailable(String),
    Rejected(String),
    Uncertain(String),
}

/// What the interface asks.
#[derive(Debug)]
pub enum Command {
    /// Pairs with the Google account of the phone: the cookies of a browser signed in to it.
    PairGoogle {
        cookies: BTreeMap<String, String>,
    },
    PairQr,
    #[allow(dead_code, reason = "used by the daemon in step 6")]
    CancelPairing,
    /// Fetches the last messages of these conversations: a `History` each.
    Fetch(Vec<String>),
    /// Asks about these conversations, missing from the lists: `Conversation` or `Gone`.
    Lookup(Vec<String>),
    /// Downloads the media of `message` (as stored): `Downloaded`.
    Download {
        id: u64,
        message: String,
        media: Vec<serde_json::Value>,
    },
    /// Sends to `conversation`: `Sent`.
    Send {
        id: u64,
        conversation: String,
        text: String,
        reply: Option<String>,
        attachments: Vec<Attachment>,
    },
    /// The conversation is read up to this message.
    Read {
        conversation: String,
        up_to: String,
    },
    Archive {
        id: u64,
        conversation: String,
    },
    Restore {
        id: u64,
        conversation: String,
    },
    /// To the trash; `for_good` (from the trash), deleted with its messages.
    DeleteChat {
        id: u64,
        conversation: String,
        for_good: bool,
    },
    DeleteMessage {
        id: u64,
        message: String,
    },
    /// Reacts to `message` of `conversation` with `emoji` (adds, replaces ours, or removes it).
    React {
        id: u64,
        conversation: String,
        message: String,
        emoji: String,
        op: gm::ReactionOp,
    },
    /// We are typing in `conversation`.
    Typing {
        conversation: String,
    },
    Contacts {
        id: u64,
    },
    /// The conversation with these numbers, created if needed: `Started`.
    StartChat {
        id: u64,
        numbers: Vec<String>,
    },
}

/// Someone of the address book of the phone, at one of their numbers.
#[derive(Debug)]
pub struct Contact {
    pub name: String,
    pub number: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Conversation {
    pub id: String,
    pub name: String,
    pub group: bool,
    pub timestamp_ms: i64,
    pub latest: Option<Latest>,
    /// Out of the inbox of the phone: "archive", "spam" (or blocked), "trash".
    #[serde(default)]
    pub folder: Option<String>,
    /// Deleted for good.
    #[serde(default)]
    pub deleted: bool,
    /// Not read on the phone.
    #[serde(default)]
    pub unread: bool,
}

/// A message of a conversation, as the store keeps it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub id: String,
    pub timestamp_ms: i64,
    pub from_me: bool,
    /// Its author, as identified in the conversation.
    pub sender_id: String,
    pub sender: String,
    pub text: String,
    /// The id of the message it replies to.
    pub quote: Option<String>,
    /// Its media as the phone describes them ([`Medium`], content type in `mime`): what
    /// downloads them. They hold their decryption keys: kept in the encrypted store only.
    #[serde(default)]
    pub media: Vec<serde_json::Value>,
    #[serde(default)]
    pub reactions: Vec<Reaction>,
    #[serde(default)]
    pub deleted: bool,
    /// Ours, not sent: the phone gave up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

/// A medium of a message, as the phone describes it: what downloads it. Stored as JSON in
/// [`Message::media`] (the format gmd used: keys in base64).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Medium {
    pub mime: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub size: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub width: i64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub height: i64,
    /// Its part of the message, to ask the phone for it in full.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub part: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub media_id: String,
    #[serde(default, with = "b64", skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<u8>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub thumb_id: String,
    #[serde(default, with = "b64", skip_serializing_if = "Vec::is_empty")]
    pub thumb_key: Vec<u8>,
    /// A small preview, in the message itself.
    #[serde(default, with = "b64", skip_serializing_if = "Vec::is_empty")]
    pub inline: Vec<u8>,
}

fn is_zero(n: &i64) -> bool {
    *n == 0
}

/// Bytes as standard base64 in JSON.
mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = Option::<String>::deserialize(d)?.unwrap_or_default();
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

/// A medium, downloaded or to send.
#[derive(Debug)]
pub struct Attachment {
    pub content_type: String,
    pub name: Option<String>,
    pub data: Vec<u8>,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// One person's emoji on a message.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Reaction {
    pub emoji: String,
    pub sender_id: String,
    pub mine: bool,
}

/// The last message of a conversation, as the phone summarizes it.
#[derive(Debug, Deserialize, Serialize)]
pub struct Latest {
    pub id: String,
    pub text: String,
    pub from_me: bool,
    pub sender: String,
}

/// Conversations listed per page, and pages of a folder listed at most.
const CONVERSATION_COUNT: i64 = 100;
const FOLDER_PAGES: usize = 50;
/// Messages fetched per conversation when asked, and conversations fetched at once.
const HISTORY_COUNT: i64 = 30;
const HISTORY_WORKERS: usize = 3;
/// How long a message sent has to come back from the phone with its id.
const ECHO_WAIT: Duration = Duration::from_secs(10);
/// How long the phone has to send a medium in full, once asked.
const FULL_MEDIUM_WAIT: Duration = Duration::from_secs(30);
/// Between two attempts to make the session active, until it is.
const ACTIVATE_RETRY: Duration = Duration::from_secs(10);

/// The name of this device on the phone (Device pairing).
const DEVICE_NAME: &str = "gmessages-tui";
/// How long Google is given to revoke this device, when unpairing.
const UNPAIR_WAIT: Duration = Duration::from_secs(20);

/// The phone, running in tasks of the current [`tokio::task::LocalSet`].
pub struct Phone {
    events: UnboundedReceiver<Event>,
    commands: Commands,
    state: Rc<State>,
}

impl Phone {
    /// Starts: connects with the session kept in `session`, or says `Login`.
    pub fn start(session: &Path) -> Self {
        let (emit, events) = unbounded();
        let (commands, received) = unbounded();
        let state = Rc::new(State {
            session_path: session.to_path_buf(),
            config: gm::Config::with_device_name(DEVICE_NAME),
            emit,
            inner: RefCell::default(),
            pairing_cancel: RefCell::default(),
        });
        state.inner.borrow_mut().me.insert("1".to_string());
        tokio::task::spawn_local(start(state.clone()));
        tokio::task::spawn_local(run_commands(state.clone(), received));
        Phone {
            events,
            commands: Commands(commands),
            state,
        }
    }

    /// The next event; `None` once stopped.
    pub async fn next(&mut self) -> Option<Event> {
        self.events.next().await
    }

    /// Sends a command; false if stopped.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command)
    }

    /// Splits it: the events to read, what sends commands, and what stops it.
    pub fn split(self) -> (UnboundedReceiver<Event>, Commands, Control) {
        let control = Control {
            commands: self.commands.clone(),
            state: self.state,
        };
        (self.events, self.commands, control)
    }
}

/// What sends commands to the phone, once split.
#[derive(Clone)]
pub struct Commands(UnboundedSender<Command>);

impl Commands {
    /// Sends `command`; false if stopped.
    pub fn send(&self, command: Command) -> bool {
        self.0.unbounded_send(command).is_ok()
    }

    /// Commands going nowhere but to the receiver returned.
    #[cfg(test)]
    pub fn detached() -> (Self, futures::channel::mpsc::UnboundedReceiver<Command>) {
        let (tx, rx) = futures::channel::mpsc::unbounded();
        (Commands(tx), rx)
    }
}

/// What stops it.
pub struct Control {
    commands: Commands,
    state: Rc<State>,
}

impl Control {
    /// Disconnects, saving the session.
    pub async fn stop(self) {
        self.commands.0.close_channel();
        let client = self.state.inner.borrow_mut().client.take();
        if let Some(client) = client {
            self.state.save(&client.session());
            client.disconnect();
        }
    }
}

/// What the tasks share.
struct State {
    session_path: PathBuf,
    config: gm::Config,
    emit: UnboundedSender<Event>,
    inner: RefCell<Inner>,
    pairing_cancel: RefCell<Option<gm::CancelToken>>,
}

#[derive(Default)]
struct Inner {
    client: Option<gm::Client>,
    /// Paired, or being paired: the session of a pairing that ended (to sign it in again).
    session: Option<gm::Session>,
    pairing: bool,
    /// Active: the conversations are listed.
    ready: bool,
    /// The event stream broke once ready, and has not opened again yet.
    down: bool,
    /// Our participant ids: in conversations, and of the phone's SIMs.
    me: HashSet<String>,
    /// The conversations known, for the names of their participants and to send.
    conversations: HashMap<String, gm::Conversation>,
    /// When each conversation was last updated by the phone: a listing started before
    /// gives an older state of it.
    updated: HashMap<String, Instant>,
    /// The phone's SIMs, by their participant id.
    sims: HashMap<String, gm::SimSelector>,
    /// Messages whose media were asked in full, waiting for their update.
    full: HashMap<String, UnboundedSender<gm::Message>>,
    /// Messages sent, by temporary id, waiting for their echo (which gives their id).
    echoes: HashMap<String, UnboundedSender<gm::Message>>,
}

impl State {
    fn emit(&self, event: Event) {
        let _ = self.emit.unbounded_send(event);
    }

    fn status(&self, message: impl Into<String>) {
        self.emit(Event::Status {
            message: message.into(),
        });
    }

    fn client(&self) -> Result<gm::Client, String> {
        self.inner
            .borrow()
            .client
            .clone()
            .ok_or_else(|| "not connected to the phone".to_string())
    }

    /// Writes the session, readable by the user only.
    fn save(&self, session: &gm::Session) {
        if let Err(e) = write_private(&self.session_path, session.to_json().as_bytes()) {
            self.status(format!("cannot save the session: {e}"));
        }
    }

    /// Keeps `conversation` and our ids in it.
    fn remember(&self, conversation: &gm::Conversation) {
        let mut inner = self.inner.borrow_mut();
        for p in &conversation.participants {
            if p.is_me && !p.id.is_empty() {
                inner.me.insert(p.id.clone());
            }
        }
        inner
            .conversations
            .insert(conversation.id.clone(), conversation.clone());
    }

    /// The conversation `id`, asked to the phone when not known yet.
    async fn conversation(&self, id: &str) -> Result<gm::Conversation, String> {
        if let Some(c) = self.inner.borrow().conversations.get(id) {
            return Ok(c.clone());
        }
        let c = self
            .client()?
            .get_conversation(id)
            .await
            .map_err(|e| format!("unknown conversation: {e}"))?;
        self.remember(&c);
        Ok(c)
    }

    /// `m` as stored; `None` for what is not a message (protocol changes, group creation…).
    fn message(&self, m: &gm::Message) -> Option<Message> {
        if m.status.is_system_notice() {
            return None;
        }
        let inner = self.inner.borrow();
        let names = inner.conversations.get(&m.conversation_id).map(names);
        let name = |id: &str| {
            names
                .as_ref()
                .and_then(|n| n.get(id).cloned())
                .unwrap_or_default()
        };
        let mut sender = name(&m.sender_id);
        if sender.is_empty()
            && let Some(p) = &m.sender
        {
            sender = participant_name(p);
        }
        let mut text: Vec<String> = m
            .subject
            .iter()
            .filter(|s| !s.is_empty())
            .cloned()
            .collect();
        let mut media = Vec::new();
        for part in &m.parts {
            match &part.content {
                gm::PartContent::Text(t) if !t.is_empty() => text.push(t.clone()),
                gm::PartContent::Media(medium) => media.push(
                    serde_json::to_value(stored_medium(medium, part.part_id.as_deref()))
                        .unwrap_or_default(),
                ),
                _ => {}
            }
        }
        let reactions = m
            .reactions
            .iter()
            .filter(|r| !r.emoji.is_empty())
            .flat_map(|r| {
                r.participant_ids.iter().map(|id| Reaction {
                    emoji: r.emoji.clone(),
                    sender_id: id.clone(),
                    mine: inner.me.contains(id),
                })
            })
            .collect();
        Some(Message {
            id: m.id.clone(),
            timestamp_ms: m.timestamp.as_millis(),
            // Sent from the phone (or here): its status says so, whichever SIM sent it.
            from_me: m.status.is_outgoing()
                || inner.me.contains(&m.sender_id)
                || m.sender.as_ref().is_some_and(|p| p.is_me),
            sender_id: m.sender_id.clone(),
            sender,
            text: text.join("\n"),
            quote: m.reply_to.clone().filter(|id| !id.is_empty()),
            media,
            reactions,
            deleted: m.status.is_deleted(),
            failed: m.status.is_failed(),
        })
    }
}

/// Writes `data` to `path`, readable by the user only, aside then renamed.
fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    io::Write::write_all(&mut options.open(&tmp)?, data)?;
    std::fs::rename(&tmp, path)
}

/// The participants of `c`, by id, to their names.
fn names(c: &gm::Conversation) -> HashMap<String, String> {
    c.participants
        .iter()
        .map(|p| (p.id.clone(), participant_name(p)))
        .collect()
}

/// A participant's name: their full name, else their number.
fn participant_name(p: &gm::Participant) -> String {
    [&p.full_name, &p.formatted_number, &p.number]
        .into_iter()
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_default()
}

fn stored_medium(m: &gm::Media, part: Option<&str>) -> Medium {
    let mime = m.mime_type();
    let (width, height) = m
        .dimensions
        .as_ref()
        .map_or((0, 0), |d| (d.width, d.height));
    Medium {
        mime: if mime.is_empty() {
            "application/octet-stream".to_string()
        } else {
            mime.to_string()
        },
        name: m.file_name.clone(),
        size: m.size,
        width,
        height,
        part: part.unwrap_or_default().to_string(),
        media_id: m.media_id.clone(),
        key: m.key.clone(),
        thumb_id: m.preview_id.clone(),
        thumb_key: m.preview_key.clone(),
        inline: m.inline_preview.clone(),
    }
}

/// `c` as the interface and the store take it.
fn conversation(c: &gm::Conversation) -> Conversation {
    let folder = match c.status.folder() {
        Some(gm::Folder::Archive) => Some("archive"),
        Some(gm::Folder::SpamAndBlocked) => Some("spam"),
        Some(gm::Folder::Trash) => Some("trash"),
        _ => None,
    };
    Conversation {
        id: c.id.clone(),
        name: c.name.clone(),
        group: c.is_group,
        timestamp_ms: c.last_activity.as_millis(),
        latest: c.preview.as_ref().map(|p| Latest {
            id: c.latest_message_id.clone(),
            text: p.text.clone(),
            from_me: p.from_me,
            sender: p.sender_name.clone(),
        }),
        folder: folder.map(str::to_string),
        deleted: c.is_deleted(),
        unread: c.unread,
    }
}

/// Connects with the session kept, if any; else says `Login`.
async fn start(state: Rc<State>) {
    let session = match load_session(&state.session_path) {
        Ok(Some(session)) => session,
        Ok(None) => return state.emit(Event::Login),
        Err(e) => {
            return state.emit(Event::Fatal {
                message: format!("cannot read the session: {e}"),
            });
        }
    };
    connect(&state, session).await;
}

/// Connects with `session`, then follows the phone until disconnected.
async fn connect(state: &Rc<State>, session: gm::Session) {
    let google = session.is_google_account();
    state.inner.borrow_mut().session = Some(session.clone());
    let (client, events) = match gm::Client::connect(state.config.clone(), session).await {
        Ok(connected) => connected,
        // Signed out, or ended by Google: as an invalid credential, or a bare 401.
        Err(e) if google && (e.is_session_gone() || e.is_http_status(401)) => {
            return expired(state, &e.to_string());
        }
        Err(e) => {
            return state.emit(Event::Fatal {
                message: format!("cannot connect: {e}"),
            });
        }
    };
    state.save(&client.session());
    {
        let mut inner = state.inner.borrow_mut();
        inner.client = Some(client);
        inner.ready = false;
    }
    tokio::task::spawn_local(keep_activating(state.clone()));
    tokio::task::spawn_local(follow(state.clone(), events));
}

/// Says the Google session has ended: new cookies of the account sign in again.
fn expired(state: &State, error: &str) {
    log::warn(&format!("Google session ended: {error}"));
    let client = {
        let mut inner = state.inner.borrow_mut();
        inner.pairing = false;
        inner.client.take()
    };
    if let Some(client) = client {
        client.disconnect();
    }
    state.emit(Event::Expired {
        message: "the Google session has ended (signed out?)".to_string(),
    });
}

/// Asks again, every few seconds, to make the session active, until it is: the phone may
/// miss the first request.
async fn keep_activating(state: Rc<State>) {
    loop {
        tokio::time::sleep(ACTIVATE_RETRY).await;
        let client = {
            let inner = state.inner.borrow();
            if inner.ready {
                return;
            }
            inner.client.clone()
        };
        let Some(client) = client else { return };
        if client.is_closed() {
            return;
        }
        if let Err(e) = client.activate().await {
            log::warn(&format!("cannot make the session active: {e}"));
        }
    }
}

/// Follows the events of the client.
async fn follow(state: Rc<State>, mut events: gm::EventReceiver) {
    while let Some(event) = events.recv().await {
        match event {
            gm::Event::Active => {
                let first = !std::mem::replace(&mut state.inner.borrow_mut().ready, true);
                if first {
                    state.emit(Event::Connected);
                    tokio::task::spawn_local(list_conversations(state.clone()));
                }
            }
            gm::Event::SessionChanged(session) => {
                state.save(&session);
                state.inner.borrow_mut().session = Some(*session);
            }
            gm::Event::MessageUpserted { message, old } => {
                {
                    let inner = state.inner.borrow();
                    if let Some(full) = inner.full.get(&message.id) {
                        let _ = full.unbounded_send((*message).clone());
                    }
                    if let Some(echo) = message.tmp_id.as_ref().and_then(|t| inner.echoes.get(t)) {
                        let _ = echo.unbounded_send((*message).clone());
                    }
                }
                if let Some(m) = state.message(&message) {
                    state.emit(Event::Message {
                        conversation: message.conversation_id.clone(),
                        message: m,
                        old,
                    });
                }
            }
            gm::Event::ConversationUpdated(c) => {
                state.remember(&c);
                state
                    .inner
                    .borrow_mut()
                    .updated
                    .insert(c.id.clone(), Instant::now());
                state.emit(Event::Conversation {
                    conversation: conversation(&c),
                });
            }
            gm::Event::Typing(typing) => {
                let inner = state.inner.borrow();
                let participant = inner
                    .conversations
                    .get(&typing.conversation_id)
                    .and_then(|c| {
                        c.participants.iter().find(|p| {
                            !p.is_me
                                && (p.number == typing.number
                                    || p.formatted_number == typing.number)
                        })
                    });
                if let Some(p) = participant {
                    state.emit(Event::Typing {
                        conversation: typing.conversation_id.clone(),
                        author: p.id.clone(),
                        name: participant_name(p),
                        started: typing.typing,
                    });
                }
            }
            gm::Event::Settings(settings) => {
                let mut inner = state.inner.borrow_mut();
                for sim in &settings.sims {
                    if let Some(id) = &sim.participant_id {
                        inner.me.insert(id.clone());
                        if let Some(selector) = sim.selector {
                            inner.sims.insert(id.clone(), selector);
                        }
                    }
                }
            }
            gm::Event::PhoneNotResponding => state.status("The phone is not responding"),
            gm::Event::PhoneResponding => state.status("The phone is back"),
            gm::Event::Disconnected { error, retry_in } => {
                log::warn(&format!("disconnected: {error}"));
                let ready = state.inner.borrow().ready;
                if ready {
                    state.inner.borrow_mut().down = true;
                    state.status(format!(
                        "Disconnected from Google Messages, retrying in {} s",
                        retry_in.as_secs()
                    ));
                } else {
                    state.status("Google Messages is not answering, retrying\u{2026}");
                }
            }
            gm::Event::Connected => {
                let down = std::mem::take(&mut state.inner.borrow_mut().down);
                if down {
                    state.status("Reconnected to Google Messages");
                }
            }
            gm::Event::LoggedOut(reason) => {
                let google = state
                    .inner
                    .borrow()
                    .session
                    .as_ref()
                    .is_some_and(gm::Session::is_google_account);
                match reason {
                    gm::LogoutReason::CredentialsRejected(error) if google => {
                        expired(&state, &error);
                    }
                    reason => {
                        let _ = std::fs::remove_file(&state.session_path);
                        state.inner.borrow_mut().client = None;
                        state.emit(Event::Unpaired {
                            reason: match reason {
                                gm::LogoutReason::Unpaired => "unpaired",
                                gm::LogoutReason::AccountLoggedOut => "account_logged_out",
                                gm::LogoutReason::CredentialsRejected(_) => "credentials_rejected",
                                _ => "unknown",
                            }
                            .to_string(),
                        });
                    }
                }
                return;
            }
            _ => {}
        }
    }
}

/// Says the conversations of the inbox, then those archived, in spam or blocked, in the
/// trash: `Ready`, with `since` the time from which every inbox conversation is listed (0:
/// all of them are). The first page of the inbox, the most recent conversations, is said at
/// once (`partial`) when more follow: those changed meanwhile are fetched without waiting
/// for the rest.
async fn list_conversations(state: Rc<State>) {
    let start = Instant::now();
    let first = |page: &[gm::Conversation]| {
        state.emit(Event::Ready {
            conversations: remember_listed(&state, page.to_vec(), start),
            since: 0,
            partial: true,
        })
    };
    let (mut listed, all, error) = list_folder(&state, gm::Folder::Inbox, Some(&first)).await;
    if listed.is_empty()
        && let Some(e) = error
    {
        state.inner.borrow_mut().ready = false;
        state.status(format!("cannot list the conversations: {e}"));
        return;
    }
    let since = if all {
        0
    } else {
        listed
            .iter()
            .map(|c| c.last_activity.as_millis())
            .min()
            .unwrap_or(0)
    };
    for folder in [
        gm::Folder::Archive,
        gm::Folder::SpamAndBlocked,
        gm::Folder::Trash,
    ] {
        listed.extend(list_folder(&state, folder, None).await.0);
    }
    state.emit(Event::Ready {
        conversations: remember_listed(&state, listed, start),
        since,
        partial: false,
    });
}

/// Keeps the conversations listed from `start`, and returns them. The lists take a while: a
/// conversation updated by the phone meanwhile (moved to the trash…) keeps that update.
fn remember_listed(
    state: &State,
    listed: Vec<gm::Conversation>,
    start: Instant,
) -> Vec<Conversation> {
    listed
        .into_iter()
        .map(|c| {
            let newer = {
                let inner = state.inner.borrow();
                inner
                    .updated
                    .get(&c.id)
                    .filter(|&&t| t > start)
                    .and_then(|_| inner.conversations.get(&c.id).cloned())
            };
            match newer {
                Some(newer) => conversation(&newer),
                None => {
                    state.remember(&c);
                    conversation(&c)
                }
            }
        })
        .collect()
}

/// What gets the first page of a folder when more follow.
type FirstPage<'a> = dyn Fn(&[gm::Conversation]) + 'a;

/// Lists every conversation of `folder`, page by page: those listed (until an error, or a
/// page bringing nothing new), whether they are all of them, and the error. `first`, if
/// given, gets the first page when more follow.
async fn list_folder(
    state: &State,
    folder: gm::Folder,
    first: Option<&FirstPage<'_>>,
) -> (Vec<gm::Conversation>, bool, Option<String>) {
    let client = match state.client() {
        Ok(client) => client,
        Err(e) => return (Vec::new(), false, Some(e)),
    };
    let mut listed = Vec::new();
    let mut seen = HashSet::new();
    let mut cursor: Option<gm::Cursor> = None;
    for page in 0..FOLDER_PAGES {
        let result = client
            .list_conversations(folder, CONVERSATION_COUNT, cursor.as_ref())
            .await;
        let page_listed = match result {
            Ok(page) => page,
            Err(e) => {
                log::warn(&format!("cannot list the conversations of {folder:?}: {e}"));
                return (listed, false, Some(e.to_string()));
            }
        };
        let count = page_listed.conversations.len();
        let mut added = 0;
        for c in page_listed.conversations {
            if seen.insert(c.id.clone()) {
                listed.push(c);
                added += 1;
            }
        }
        let next = page_listed.next.filter(|c| !c.last_id.is_empty());
        tracing::debug!(?folder, page, listed = count, new = added, "folder listed");
        if (count as i64) < CONVERSATION_COUNT || next.is_none() {
            return (listed, true, None);
        } else if added == 0 {
            // The cursor ignored: the same page again.
            return (listed, false, None);
        }
        if page == 0
            && let Some(first) = first
        {
            first(&listed);
        }
        cursor = next;
    }
    (listed, false, None)
}

/// Handles the commands of the interface until it stops.
async fn run_commands(state: Rc<State>, mut commands: UnboundedReceiver<Command>) {
    while let Some(command) = commands.next().await {
        let state = state.clone();
        match command {
            Command::PairGoogle { cookies } => pair(state, Some(cookies)),
            Command::PairQr => pair(state, None),
            Command::CancelPairing => {
                if let Some(cancel) = state.pairing_cancel.borrow_mut().take() {
                    cancel.cancel();
                }
                state.inner.borrow_mut().pairing = false;
            }
            Command::Fetch(ids) => {
                tokio::task::spawn_local(fetch(state, ids));
            }
            Command::Lookup(ids) => {
                tokio::task::spawn_local(lookup(state, ids));
            }
            Command::Download { id, message, media } => {
                tokio::task::spawn_local(async move {
                    let event = match download(&state, &message, media).await {
                        Ok(attachments) => Event::Downloaded {
                            id,
                            attachments,
                            error: None,
                        },
                        Err(error) => Event::Downloaded {
                            id,
                            attachments: Vec::new(),
                            error: Some(error),
                        },
                    };
                    state.emit(event);
                });
            }
            Command::Send {
                id,
                conversation,
                text,
                reply,
                attachments,
            } => {
                tokio::task::spawn_local(async move {
                    let outcome = send(&state, &conversation, text, reply, attachments).await;
                    state.emit(Event::Sent {
                        id,
                        conversation,
                        outcome,
                    });
                });
            }
            Command::Read {
                conversation,
                up_to,
            } => {
                tokio::task::spawn_local(async move {
                    let result = match state.client() {
                        Ok(client) => client
                            .mark_read(&conversation, &up_to)
                            .await
                            .map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    };
                    if let Err(e) = result {
                        log::warn(&format!("cannot mark {conversation} read: {e}"));
                    }
                });
            }
            Command::Archive { id, conversation } => done(state, id, move |s| async move {
                act(&s, &conversation, Act::Archive).await
            }),
            Command::Restore { id, conversation } => done(state, id, move |s| async move {
                act(&s, &conversation, Act::Restore).await
            }),
            Command::DeleteChat {
                id,
                conversation,
                for_good,
            } => done(state, id, move |s| async move {
                let what = if for_good {
                    Act::DeleteForGood
                } else {
                    Act::Trash
                };
                act(&s, &conversation, what).await
            }),
            Command::DeleteMessage { id, message } => done(state, id, move |s| async move {
                s.client()?
                    .delete_message(&message)
                    .await
                    .map_err(|e| e.to_string())
            }),
            Command::React {
                id,
                conversation,
                message,
                emoji,
                op,
            } => done(state, id, move |s| async move {
                let client = s.client()?;
                let sim = s.conversation(&conversation).await?.sim_selector();
                client
                    .react(&message, &emoji, op, sim)
                    .await
                    .map_err(|e| e.to_string())
            }),
            Command::Typing { conversation } => {
                tokio::task::spawn_local(async move {
                    let said = async {
                        let client = state.client()?;
                        let sim = state.conversation(&conversation).await?.sim_selector();
                        client
                            .set_typing(&conversation, sim)
                            .await
                            .map_err(|e| e.to_string())
                    };
                    if let Err(e) = said.await {
                        log::warn(&format!("cannot say we are typing: {e}"));
                    }
                });
            }
            Command::Contacts { id } => {
                tokio::task::spawn_local(async move {
                    let listed = match state.client() {
                        Ok(client) => client.list_contacts().await.map_err(|e| e.to_string()),
                        Err(e) => Err(e),
                    };
                    let event = match listed {
                        Ok(contacts) => Event::Contacts {
                            id,
                            contacts: contacts
                                .into_iter()
                                .filter(|c| !c.number.is_empty())
                                .map(|c| Contact {
                                    name: c.name,
                                    number: c.number,
                                })
                                .collect(),
                            error: None,
                        },
                        Err(error) => Event::Contacts {
                            id,
                            contacts: Vec::new(),
                            error: Some(error),
                        },
                    };
                    state.emit(event);
                });
            }
            Command::StartChat { id, numbers } => {
                tokio::task::spawn_local(async move {
                    let event = match start_chat(&state, &numbers).await {
                        Ok(c) => Event::Started {
                            id,
                            conversation: Some(conversation(&c)),
                            error: None,
                        },
                        Err(error) => Event::Started {
                            id,
                            conversation: None,
                            error: Some(error),
                        },
                    };
                    state.emit(event);
                });
            }
        }
    }
}

/// Runs `action`, then says `Done` with its outcome.
fn done<F, Fut>(state: Rc<State>, id: u64, action: F)
where
    F: FnOnce(Rc<State>) -> Fut + 'static,
    Fut: Future<Output = Result<(), String>> + 'static,
{
    tokio::task::spawn_local(async move {
        let error = action(state.clone()).await.err();
        state.emit(Event::Done { id, error });
    });
}

enum Act {
    Archive,
    Restore,
    Trash,
    DeleteForGood,
}

/// Archives, restores, trashes or deletes the conversation `id`, as Messages for web does.
async fn act(state: &State, id: &str, what: Act) -> Result<(), String> {
    let client = state.client()?;
    let c = state.conversation(id).await?;
    let result = match what {
        Act::Archive => client.archive(&c).await,
        Act::Restore => client.unarchive(&c).await,
        Act::DeleteForGood => {
            // The phone wants the number of the other person of a one-to-one conversation:
            // asked afresh, the one known may lack its participants.
            let c = client.get_conversation(id).await.unwrap_or(c);
            client.delete_forever(&c).await
        }
        Act::Trash => match client.move_to_trash(&c).await {
            Ok(()) => Ok(()),
            // Refused when in the trash already, though shown elsewhere: shown there again.
            Err(e) => match client.get_conversation(id).await {
                Ok(c) if c.status == gm::ConversationStatus::Trash => {
                    state.remember(&c);
                    state.emit(Event::Conversation {
                        conversation: conversation(&c),
                    });
                    Ok(())
                }
                _ => Err(e),
            },
        },
    };
    result.map_err(|e| e.to_string())
}

/// Asks about the conversations `ids`, missing from the lists (in the trash, or deleted):
/// `Conversation` for those the phone has, `Gone` for those it has not.
async fn lookup(state: Rc<State>, ids: Vec<String>) {
    let Ok(client) = state.client() else { return };
    for id in ids {
        match client.get_conversation(&id).await {
            Err(e) => log::warn(&format!("cannot look the conversation {id} up: {e}")),
            Ok(c) if c.id.is_empty() || c.is_deleted() => {
                tracing::debug!(conversation = %id, "conversation gone");
                state.emit(Event::Gone { conversation: id })
            }
            Ok(c) => {
                state.remember(&c);
                state.emit(Event::Conversation {
                    conversation: conversation(&c),
                });
            }
        }
    }
}

/// Fetches the last messages of the conversations `ids`, in order, a few at a time: one
/// `History` each (with the error if it failed).
async fn fetch(state: Rc<State>, ids: Vec<String>) {
    let queue = Rc::new(RefCell::new(ids.into_iter()));
    let workers = (0..HISTORY_WORKERS).map(|_| {
        let (state, queue) = (state.clone(), queue.clone());
        async move {
            loop {
                let Some(id) = queue.borrow_mut().next() else {
                    return;
                };
                let listed = match state.client() {
                    Ok(client) => client
                        .list_messages(&id, HISTORY_COUNT, None)
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                };
                if let Ok(page) = &listed {
                    tracing::debug!(conversation = %id, fetched = page.messages.len(), "messages fetched");
                }
                let event = match listed {
                    Ok(page) => Event::History {
                        messages: page
                            .messages
                            .iter()
                            .filter_map(|m| state.message(m))
                            .collect(),
                        conversation: id,
                        error: None,
                    },
                    Err(error) => {
                        log::warn(&format!("cannot fetch the messages of {id}: {error}"));
                        Event::History {
                            conversation: id,
                            messages: Vec::new(),
                            error: Some(error),
                        }
                    }
                };
                state.emit(event);
            }
        }
    });
    futures::future::join_all(workers).await;
}

/// Sends `text` and `attachments` to `conversation`, from the SIM it uses.
async fn send(
    state: &State,
    conversation: &str,
    text: String,
    reply: Option<String>,
    attachments: Vec<Attachment>,
) -> SendOutcome {
    let client = match state.client() {
        Ok(client) => client,
        Err(error) => return SendOutcome::Unavailable(error),
    };
    let c = match state.conversation(conversation).await {
        Ok(conversation) => conversation,
        Err(error) => return SendOutcome::Unavailable(error),
    };
    let mut outgoing = gm::OutgoingMessage::new(&c, text.clone());
    for a in &attachments {
        let name = a.name.clone().unwrap_or_default();
        let media = match client.upload_media(&a.data, &name, &a.content_type).await {
            Ok(media) => media,
            Err(error) => {
                return SendOutcome::Unavailable(format!(
                    "cannot upload {}: {error}",
                    a.content_type
                ));
            }
        };
        outgoing = outgoing.with_media(media);
    }
    if outgoing.text.is_empty() && outgoing.media.is_empty() {
        return SendOutcome::Unavailable("nothing to send".to_string());
    }
    if let Some(reply) = &reply {
        outgoing = outgoing.reply_to(reply.clone());
    }
    if outgoing.sim.is_none() {
        outgoing.sim = state
            .inner
            .borrow()
            .sims
            .get(&c.our_participant_id)
            .copied();
    }
    let tmp = uuid::Uuid::new_v4().to_string();
    outgoing.tmp_id = Some(tmp.clone());

    let (echo, mut echoes) = unbounded();
    state.inner.borrow_mut().echoes.insert(tmp.clone(), echo);
    struct Forget<'a>(&'a State, String);
    impl Drop for Forget<'_> {
        fn drop(&mut self) {
            self.0.inner.borrow_mut().echoes.remove(&self.1);
        }
    }
    let _forget = Forget(state, tmp.clone());

    let outcome = match client.send_message(outgoing).await {
        Ok(outcome) => outcome,
        Err(error) => return SendOutcome::Uncertain(error.to_string()),
    };
    if outcome.result != gm::SendResult::Accepted {
        return SendOutcome::Rejected(format!("not sent ({:?})", outcome.result));
    }
    let echoed = tokio::time::timeout(ECHO_WAIT, async {
        while let Some(m) = echoes.next().await {
            if let Some(message) = state.message(&m) {
                return Some(message);
            }
        }
        None
    })
    .await;
    if let Ok(Some(message)) = echoed {
        return SendOutcome::Sent(message);
    }
    log::warn("no echo of the message sent");
    // Sent, but its id is not known: the temporary one, until the conversation is fetched.
    SendOutcome::Accepted(Message {
        id: tmp,
        timestamp_ms: gm::Timestamp::now().as_millis(),
        from_me: true,
        sender_id: c.our_participant_id.clone(),
        sender: String::new(),
        text,
        quote: reply,
        media: Vec::new(),
        reactions: Vec::new(),
        deleted: false,
        failed: false,
    })
}

/// Downloads `media`, of `message`; those the phone has sent only as previews are asked in
/// full first.
async fn download(
    state: &State,
    message: &str,
    media: Vec<serde_json::Value>,
) -> Result<Vec<Attachment>, String> {
    if media.is_empty() {
        return Err("no media for this message".to_string());
    }
    let client = state.client()?;
    let mut attachments = Vec::with_capacity(media.len());
    for medium in media {
        let medium: Medium =
            serde_json::from_value(medium).map_err(|e| format!("bad medium stored: {e}"))?;
        let medium = full(state, &client, message, medium).await;
        // The best there is: the medium, its preview, the small preview in the message.
        let (id, key) = if medium.media_id.is_empty() {
            (&medium.thumb_id, &medium.thumb_key)
        } else {
            (&medium.media_id, &medium.key)
        };
        let data = if !id.is_empty() {
            client
                .download_media(id, key)
                .await
                .map_err(|e| e.to_string())?
        } else if !medium.inline.is_empty() {
            medium.inline.clone()
        } else {
            return Err("not downloaded by the phone yet".to_string());
        };
        attachments.push(Attachment {
            content_type: medium.mime,
            name: Some(medium.name).filter(|n| !n.is_empty()),
            data,
            width: u32::try_from(medium.width).ok().filter(|&w| w > 0),
            height: u32::try_from(medium.height).ok().filter(|&h| h > 0),
        });
    }
    Ok(attachments)
}

/// `medium`, of `message`, in full if the phone sends it in time once asked.
async fn full(state: &State, client: &gm::Client, message: &str, medium: Medium) -> Medium {
    if !medium.media_id.is_empty() || medium.part.is_empty() {
        return medium;
    }
    let (updates, mut updated) = unbounded();
    state
        .inner
        .borrow_mut()
        .full
        .insert(message.to_string(), updates);
    struct Forget<'a>(&'a State, String);
    impl Drop for Forget<'_> {
        fn drop(&mut self) {
            self.0.inner.borrow_mut().full.remove(&self.1);
        }
    }
    let _forget = Forget(state, message.to_string());
    if let Err(e) = client.request_full_size_image(message, &medium.part).await {
        log::warn(&format!("cannot ask the phone for a full medium: {e}"));
        return medium;
    }
    let found = tokio::time::timeout(FULL_MEDIUM_WAIT, async {
        while let Some(update) = updated.next().await {
            for (part, m) in update.media() {
                if part == Some(medium.part.as_str()) && !m.media_id.is_empty() {
                    return Some(stored_medium(m, part));
                }
            }
        }
        None
    })
    .await;
    match found {
        Ok(Some(full)) => full,
        _ => {
            log::warn("the phone has not sent the full medium in time");
            medium
        }
    }
}

/// The conversation with `numbers`, created unless it exists already: a group for several
/// numbers, RCS if the phone asks for it, as Messages for web does.
async fn start_chat(state: &State, numbers: &[String]) -> Result<gm::Conversation, String> {
    if numbers.is_empty() {
        return Err("no number to start a chat with".to_string());
    }
    let numbers: Vec<&str> = numbers.iter().map(String::as_str).collect();
    let outcome = state
        .client()?
        .get_or_create_conversation(&numbers, Some(""))
        .await
        .map_err(|e| e.to_string())?;
    let c = match outcome {
        gm::GetOrCreateOutcome::Conversation(c) => *c,
        other => return Err(format!("the phone gave no conversation ({other:?})")),
    };
    if c.id.is_empty() {
        return Err("the phone gave no conversation".to_string());
    }
    state.remember(&c);
    state
        .inner
        .borrow_mut()
        .updated
        .insert(c.id.clone(), Instant::now());
    Ok(c)
}

/// Pairs: with the Google account of the phone (`cookies`), or by QR code. A pairing whose
/// Google session ended is signed in again with the new cookies, if they are accepted.
fn pair(state: Rc<State>, cookies: Option<BTreeMap<String, String>>) {
    if std::mem::replace(&mut state.inner.borrow_mut().pairing, true) {
        return state.status("already pairing");
    }
    let cancel = gm::CancelToken::new();
    state.pairing_cancel.replace(Some(cancel.clone()));
    tokio::task::spawn_local(async move {
        let paired = tokio::select! {
            _ = cancel.cancelled() => Err("cancelled".to_string()),
            result = async {
                let previous = state
                    .inner
                    .borrow()
                    .session
                    .clone()
                    .filter(gm::Session::is_google_account);
                match (cookies, previous) {
                    (Some(cookies), Some(mut session)) => {
                        session.cookies = cookies.clone();
                        match gm::Client::connect(state.config.clone(), session.clone()).await {
                            Ok((client, _)) => {
                                client.disconnect();
                                Ok(session)
                            }
                            Err(e) => {
                                log::warn(&format!("cannot sign the pairing in again: {e}"));
                                pair_google(&state, cookies, &cancel).await
                            }
                        }
                    }
                    (Some(cookies), None) => pair_google(&state, cookies, &cancel).await,
                    (None, _) => {
                        let emit = state.emit.clone();
                        gm::pair_with_qr(state.config.clone(), &cancel, move |url| {
                            let _ = emit.unbounded_send(Event::Qr {
                                url: url.to_string(),
                            });
                        })
                        .await
                        .map_err(|e| format!("cannot pair: {e}"))
                    }
                }
            } => result,
        };
        if cancel.is_cancelled() {
            return state.emit(Event::PairFailed {
                message: "cancelled".to_string(),
            });
        }
        state.pairing_cancel.borrow_mut().take();
        state.inner.borrow_mut().pairing = false;
        match paired {
            Ok(session) => {
                state.save(&session);
                state.emit(Event::Paired);
                connect(&state, session).await;
            }
            Err(message) => state.emit(Event::PairFailed { message }),
        }
    });
}

async fn pair_google(
    state: &State,
    cookies: BTreeMap<String, String>,
    cancel: &gm::CancelToken,
) -> Result<gm::Session, String> {
    let emit = state.emit.clone();
    let paired = gm::pair_with_google_account(
        state.config.clone(),
        cookies,
        gm::AccountPairingOptions::default(),
        cancel,
        move |prompt| {
            let _ = emit.unbounded_send(Event::Emoji {
                emoji: prompt.emoji.to_string(),
            });
        },
    )
    .await;
    paired.map_err(|e| match e {
        gm::Error::Pairing(gm::PairingError::NoPhone) => "no phone found for this Google account: \
            in Google Messages, Device pairing, is it signed in?"
            .to_string(),
        gm::Error::Pairing(gm::PairingError::WrongEmoji) => {
            "wrong emoji tapped on the phone".to_string()
        }
        gm::Error::Pairing(gm::PairingError::CancelledOnPhone) => {
            "cancelled on the phone".to_string()
        }
        gm::Error::Pairing(gm::PairingError::TimedOut) => {
            "the phone did not answer in time: is it online?".to_string()
        }
        gm::Error::Pairing(gm::PairingError::BadCookies(_)) => {
            "Google refused the cookies (signed out?)".to_string()
        }
        e => e.to_string(),
    })
}

/// What unpairing did.
pub enum Unpaired {
    /// There was no session.
    NotPaired,
    /// Google revoked this device (the phone no longer lists it), and the session is deleted.
    Revoked,
    /// The session is deleted, but Google could not be told: why.
    Forgotten(String),
}

/// Unpairs the session kept in `path`: Google revokes this device, so that the phone forgets
/// it, then the session is deleted (also when Google could not be told).
pub async fn unpair(path: &Path) -> Result<Unpaired, String> {
    let delete = || {
        crate::session::reset(path).map_err(|e| format!("cannot delete {}: {e}", path.display()))
    };
    let session = match load_session(path) {
        Ok(Some(session)) => session,
        Ok(None) => return Ok(Unpaired::NotPaired),
        // Unreadable: Google cannot be told, but it goes.
        Err(e) => {
            delete()?;
            return Ok(Unpaired::Forgotten(format!("unreadable session: {e}")));
        }
    };
    let revoke = async {
        let config = gm::Config::with_device_name(DEVICE_NAME);
        let (client, _events) = gm::Client::connect(config, session).await?;
        client.unpair().await
    };
    let unpaired = match tokio::time::timeout(UNPAIR_WAIT, revoke).await {
        Ok(Ok(())) => Unpaired::Revoked,
        Ok(Err(e)) => Unpaired::Forgotten(e.to_string()),
        Err(_) => Unpaired::Forgotten("Google did not answer".to_string()),
    };
    delete()?;
    Ok(unpaired)
}

/// The session kept in `path`; `None` when there is none. A session of gmd, the Go helper
/// of earlier versions, is converted.
fn load_session(path: &Path) -> Result<Option<gm::Session>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    if let Ok(session) = gm::Session::from_json(&text) {
        return Ok(Some(session));
    }
    let session = legacy::convert(&text)?;
    // Kept aside, in case.
    let _ = std::fs::rename(path, path.with_extension("gmd.json"));
    write_private(path, session.to_json().as_bytes()).map_err(|e| e.to_string())?;
    Ok(Some(session))
}

/// The session as gmd (the Go helper of earlier versions) kept it.
mod legacy {
    use std::collections::BTreeMap;

    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use gmessages_proto as gm;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Session {
        request_crypto: Keys,
        refresh_key: Jwk,
        browser: Device,
        mobile: Device,
        tachyon_token: String,
        #[serde(default)]
        tachyon_expiry: Option<String>,
        #[serde(default)]
        tachyon_ttl: i64,
        #[serde(default)]
        session_id: Option<String>,
        #[serde(default)]
        dest_reg_id: Option<String>,
        #[serde(default)]
        pairing_id: Option<String>,
        #[serde(default)]
        cookies: BTreeMap<String, String>,
    }

    #[derive(Deserialize)]
    struct Keys {
        aes_key: String,
        hmac_key: String,
    }

    #[derive(Deserialize)]
    struct Jwk {
        d: String,
    }

    #[derive(Deserialize)]
    struct Device {
        #[serde(default, rename = "userID")]
        user_id: i64,
        #[serde(default, rename = "sourceID")]
        source_id: String,
        #[serde(default)]
        network: String,
    }

    impl From<Device> for gm::Device {
        fn from(d: Device) -> Self {
            gm::Device {
                user_id: d.user_id,
                source_id: d.source_id,
                network: d.network,
            }
        }
    }

    /// An id, unless empty or nil.
    fn id(id: Option<String>) -> Option<String> {
        id.filter(|id| !id.is_empty() && !id.trim_matches(['0', '-']).is_empty())
    }

    pub fn convert(text: &str) -> Result<gm::Session, String> {
        let old: Session =
            serde_json::from_str(text).map_err(|e| format!("unknown session format: {e}"))?;
        let bytes = |what: &str, text: &str| {
            STANDARD
                .decode(text)
                .map_err(|e| format!("bad {what} in the session: {e}"))
        };
        let expiry_us = old
            .tachyon_expiry
            .as_deref()
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map_or(0, |t| t.timestamp_micros());
        let parts = gm::SessionParts {
            aes_key: bytes("AES key", &old.request_crypto.aes_key)?,
            hmac_key: bytes("HMAC key", &old.request_crypto.hmac_key)?,
            refresh_key_d: URL_SAFE_NO_PAD
                .decode(old.refresh_key.d.trim_end_matches('='))
                .map_err(|e| format!("bad refresh key in the session: {e}"))?,
            browser: old.browser.into(),
            phone: old.mobile.into(),
            token: bytes("token", &old.tachyon_token)?,
            token_ttl_us: old.tachyon_ttl,
            token_expiry_us: expiry_us,
            web_device_id: id(old.session_id),
            destination_id: id(old.dest_reg_id),
            pairing_attempt_id: id(old.pairing_id),
            cookies: old.cookies,
        };
        gm::Session::from_parts(parts).map_err(|e| format!("cannot convert the session: {e}"))
    }
}

/// The log of the phone, `phone.log` in the state directory: the warnings here, and those
/// of gmessages-proto (through `tracing`).
pub mod log {
    use std::path::Path;
    use std::sync::Mutex;

    /// Logs to `path`, appending; nothing if it cannot be opened.
    pub fn init(path: &Path) {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let Ok(file) = options.open(path) else { return };
        let filter = tracing_subscriber::EnvFilter::try_from_env("GMESSAGES_TUI_LOG")
            .unwrap_or_else(|_| "gmessages_proto=info,gmessages_tui=info".into());
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(Mutex::new(file))
            .try_init();
    }

    pub fn warn(message: &str) {
        tracing::warn!("{message}");
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};

    use super::*;

    fn disconnected_state() -> Rc<State> {
        let (emit, _) = unbounded();
        Rc::new(State {
            session_path: PathBuf::from("/unused"),
            config: gm::Config::with_device_name(DEVICE_NAME),
            emit,
            inner: RefCell::default(),
            pairing_cancel: RefCell::default(),
        })
    }

    #[tokio::test]
    async fn send_is_unavailable_before_a_phone_call() {
        let state = disconnected_state();
        let outcome = send(&state, "missing", "hello".into(), None, Vec::new()).await;
        assert!(matches!(outcome, SendOutcome::Unavailable(_)));
    }

    #[tokio::test]
    async fn cancel_pairing_cancels_the_active_token() {
        let state = disconnected_state();
        let token = gm::CancelToken::new();
        state.pairing_cancel.replace(Some(token.clone()));
        state.inner.borrow_mut().pairing = true;
        let (commands, received) = unbounded();
        commands.unbounded_send(Command::CancelPairing).unwrap();
        drop(commands);
        run_commands(state.clone(), received).await;
        assert!(token.is_cancelled());
        assert!(!state.inner.borrow().pairing);
    }

    fn random(n: usize) -> Vec<u8> {
        let mut bytes = vec![0; n];
        getrandom::fill(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn a_gmd_session_is_converted() {
        let (aes, hmac, d) = (random(32), random(32), random(32));
        let legacy = serde_json::json!({
            "request_crypto": {"aes_key": STANDARD.encode(&aes), "hmac_key": STANDARD.encode(&hmac)},
            "refresh_key": {"kty": "EC", "crv": "P-256", "d": URL_SAFE_NO_PAD.encode(&d), "x": "", "y": ""},
            "browser": {"userID": 7, "sourceID": "browser", "network": "GDitto"},
            "mobile": {"userID": 7, "sourceID": "phone", "network": "GDitto"},
            "tachyon_token": STANDARD.encode(b"token"),
            "tachyon_expiry": "2026-10-23T08:00:00.123456789Z",
            "tachyon_ttl": 86_400_000_000i64,
            "session_id": "0b9f7c3e-1d2a-4e5f-8a9b-0c1d2e3f4a5b",
            "dest_reg_id": "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f",
            "pairing_id": "00000000-0000-0000-0000-000000000000",
            "cookies": {"SID": "s", "SAPISID": "p"},
        });
        let session = legacy::convert(&legacy.to_string()).unwrap();
        assert_eq!(session.aes_key.to_vec(), aes);
        assert_eq!(session.hmac_key.to_vec(), hmac);
        assert_eq!(session.phone.source_id, "phone");
        assert_eq!(session.browser.user_id, 7);
        assert_eq!(session.token, b"token");
        assert_eq!(session.token_ttl_us, 86_400_000_000);
        assert_eq!(session.token_expiry_us, 1_792_742_400_123_456);
        assert!(session.is_google_account());
        assert_eq!(session.pairing_attempt_id, None, "a nil id is none");
        assert_eq!(session.cookies["SAPISID"], "p");
        // Kept in the new format from then on.
        let again = gm::Session::from_json(&session.to_json()).unwrap();
        assert_eq!(again, session);
    }

    #[test]
    fn a_medium_keeps_the_format_gmd_stored() {
        let stored = serde_json::json!({
            "mime": "image/jpeg", "name": "a.jpg", "size": 12, "part": "p1",
            "media_id": "m", "key": STANDARD.encode([1, 2, 3]),
        });
        let medium: Medium = serde_json::from_value(stored.clone()).unwrap();
        assert_eq!(medium.key, [1, 2, 3]);
        assert!(medium.thumb_key.is_empty());
        assert_eq!(serde_json::to_value(&medium).unwrap(), stored);
    }
}
