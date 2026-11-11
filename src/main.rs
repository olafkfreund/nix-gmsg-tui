//! gmessages-tui: a terminal client for Google Messages, paired with your Android phone
//! like Google Messages for web.
//!
//!   gmessages-tui [--reset-login] [--manual | --qr] | --uninstall | --version | --help
//!
//! Not affiliated with Google.

mod cli;
mod key;
mod pair;
mod phone;
mod session;
#[cfg(any(feature = "wry-signin", test))]
mod signin;
mod store;
mod web;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::future::Future;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use chat_tui::backend::Result;
use chat_tui::data::{Attachment, ChatId, Discussion, Folder, Message, MessageId};
use chat_tui::recipients::Contact;
use chat_tui::{Backend, Capabilities, Downloaded, Draft, Event};
use futures::channel::mpsc::{UnboundedReceiver, unbounded};
use futures::channel::oneshot;

const NAME: &str = "gmessages-tui";
/// The last release tag, `+` the commit when built beyond it (see build.rs).
const VERSION: &str = env!("GMESSAGES_TUI_VERSION");

/// How long the interface waits to be connected before showing what is stored anyway.
const OFFLINE_AFTER: Duration = Duration::from_secs(10);

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let (qr, manual) = match cli::parse(args.iter().map(String::as_str)) {
        Ok(cli::Command::Run { qr, manual }) => (qr, manual),
        Ok(cli::Command::Unpair) => {
            if let Err(e) = unpair() {
                eprintln!("{NAME}: {e}");
                std::process::exit(1);
            }
            return;
        }
        #[cfg(feature = "wry-signin")]
        Ok(cli::Command::Signin) => {
            signin::run_child();
            return;
        }
        Ok(cli::Command::Version) => {
            println!("{VERSION}");
            return;
        }
        Ok(cli::Command::Help) => {
            println!("{}\n\nUsage: {}", cli::HELP, cli::USAGE);
            return;
        }
        Ok(cli::Command::Uninstall) => {
            if let Err(e) = uninstall() {
                eprintln!("{NAME}: {e}");
                std::process::exit(1);
            }
            return;
        }
        Err(usage) => {
            eprintln!("usage: {usage}\n(--help for details)");
            std::process::exit(2);
        }
    };
    let local = tokio::task::LocalSet::new();
    if let Err(e) = runtime().block_on(local.run_until(run(qr, manual))) {
        eprintln!("{NAME}: {e}");
        std::process::exit(1);
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("cannot create the Tokio runtime")
}

/// Unpairs the session in `state`, saying what was done: whether the phone was told.
fn unpair_session(state: &std::path::Path) -> std::result::Result<bool, Box<dyn Error>> {
    println!("Unpairing\u{2026}");
    let told = match runtime().block_on(phone::unpair(&state.join("session.json")))? {
        phone::Unpaired::NotPaired => {
            println!("Not paired.");
            true
        }
        phone::Unpaired::Revoked => {
            println!("Unpaired: your phone no longer lists {NAME} (Device pairing).");
            true
        }
        phone::Unpaired::Forgotten(why) => {
            println!("Unpaired here, but Google could not be told ({why}).");
            false
        }
    };
    Ok(told)
}

/// Unpairs: the phone forgets this device, the session is deleted; the store stays.
fn unpair() -> std::result::Result<(), Box<dyn Error>> {
    let config = chat_tui::Config::new(NAME)?;
    let state = config.read_marks.parent().ok_or("no state directory")?;
    if !unpair_session(state)? {
        println!("Also remove {NAME} from your phone (Device pairing).");
    }
    println!("Run {NAME} to pair again; the messages stored stay.");
    Ok(())
}

/// Removes what gmessages-tui left on this machine, after confirmation: the pairing (the phone
/// told), the store key in the keyring and the state (session, store, log, marks); the cached
/// gmd of earlier versions.
fn uninstall() -> std::result::Result<(), Box<dyn Error>> {
    let config = chat_tui::Config::new(NAME)?;
    let state = config.read_marks.parent().ok_or("no state directory")?;
    let cache = cache_dir();
    println!("This unpairs, then deletes for good:");
    println!("  - the store key in the keyring (service {NAME})");
    println!("  - {} (session, messages, media)", state.display());
    print!("Continue? [y/N] ");
    std::io::Write::flush(&mut std::io::stdout())?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim(), "y" | "Y" | "yes") {
        println!("Nothing removed.");
        return Ok(());
    }
    let told = unpair_session(state)?;
    // The key first: without it, a store left behind cannot be read.
    key::forget(&state.join("messages.db"))?;
    for dir in std::iter::once(state).chain(cache.as_deref()) {
        match std::fs::remove_dir_all(dir) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("cannot remove {}: {e}", dir.display()).into());
            }
            _ => {}
        }
    }
    if told {
        println!("Removed. Remove the program with `cargo uninstall gmessages-tui`,");
        println!("or with your package manager.");
    } else {
        println!("Removed. Also remove gmessages-tui from your phone (Device pairing),");
        println!("and the program with `cargo uninstall gmessages-tui`, or your package manager.");
    }
    Ok(())
}

/// Starts pairing, in the sign-in window unless `manual`; `manual` then says whether it is
/// from the browser (also when the window cannot open).
async fn start_pairing(intro: &str, manual: &mut bool) -> std::io::Result<pair::Pairing> {
    let pairing = pair::Pairing::start(intro, *manual).await?;
    *manual = pairing.is_manual();
    Ok(pairing)
}

/// `qr`: pair by QR code rather than with the Google account, if pairing.
async fn run(qr: bool, mut manual: bool) -> std::result::Result<(), Box<dyn Error>> {
    let mut config = chat_tui::Config::new(NAME)?;
    config.version = VERSION.to_string();
    let state = config.read_marks.parent().ok_or("no state directory")?;
    create_private_dir(state)?;
    let session = state.join("session.json");
    // The store first: its key must be available.
    let store_path = state.join("messages.db");
    let store = Rc::new(store::Store::open(
        &store_path,
        &key::store_key(&store_path)?,
    )?);
    let stored = store.conversations()?;
    phone::log::init(&state.join("phone.log"));
    let _ = std::fs::remove_file(state.join("gmd.log"));
    let mut phone = phone::Phone::start(&session);
    // Paired already: connecting and fetching the conversations take a while.
    if session.exists() {
        pair::say("Syncing\u{2026}");
    }

    // Until connected (with conversations stored) or until the phone sends the conversations:
    // pairing, if needed, in the terminal or the pairing page.
    let mut pairing: Option<pair::Pairing> = None;
    // With conversations stored, the interface opens even if not connected by then (offline,
    // phone or Google not answering): what is stored can be read.
    let offline_after = tokio::time::Instant::now() + OFFLINE_AFTER;
    let ready = loop {
        let may_open = !stored.is_empty() && pairing.is_none();
        let pasted = async {
            match pairing.as_mut() {
                Some(pairing) => pairing.next().await,
                None => std::future::pending().await,
            }
        };
        let event = tokio::select! {
            _ = tokio::signal::ctrl_c(), if !manual => return Ok(()),
            _ = tokio::time::sleep_until(offline_after), if may_open => {
                break None;
            }
            event = phone.next() => event,
            Some(pasted) = pasted => {
                let cookies = match pasted {
                    pair::Pasted::Cookies(cookies) => cookies,
                    pair::Pasted::Quit => return Ok(()),
                    pair::Pasted::Failed(error) => return Err(error.into()),
                    // No window: on from the browser, without restarting.
                    pair::Pasted::Unavailable(why) => {
                        pair::say(&format!("{why}: pairing from your browser instead."));
                        drop(pairing.take());
                        manual = true;
                        let intro = "Pair with the Google account of your phone";
                        pairing = Some(start_pairing(intro, &mut manual).await?);
                        continue;
                    }
                };
                pair::say("Asking the phone\u{2026}");
                if let Some(page) = pairing.as_ref().and_then(pair::Pairing::page) {
                    page.asking();
                }
                phone.send(phone::Command::PairGoogle {
                    cookies: cookies.into_iter().collect(),
                });
                continue;
            }
        };
        let page = pairing.as_ref().and_then(pair::Pairing::page);
        match event {
            Some(phone::Event::Login) if qr => {
                phone.send(phone::Command::PairQr);
            }
            Some(phone::Event::Login) => {
                let intro = "Pair with the Google account of your phone";
                drop(pairing.take());
                pairing = Some(start_pairing(intro, &mut manual).await?);
            }
            Some(phone::Event::Expired { message }) if pairing.is_none() || !manual => {
                drop(pairing.take());
                let intro = format!("Sign in again: {message}. Same Google account");
                pairing = Some(start_pairing(&intro, &mut manual).await?);
            }
            Some(phone::Event::Expired { message }) => {
                pair::say(&format!("Sign in again: {message}. Paste again."));
                page.inspect(|page| page.failed(&message));
            }
            Some(phone::Event::PairFailed { message }) if qr => {
                return Err(format!("pairing failed: {message}").into());
            }
            Some(phone::Event::PairFailed { message }) if !manual => {
                drop(pairing.take());
                pairing = Some(
                    start_pairing(
                        &format!("Pairing failed: {message}. Sign in again"),
                        &mut manual,
                    )
                    .await?,
                );
            }
            Some(phone::Event::PairFailed { message }) => {
                pair::say(&format!(
                    "Pairing failed: {}. Paste again.",
                    message.trim_end_matches('.')
                ));
                page.inspect(|page| page.failed(&message));
            }
            Some(phone::Event::Emoji { emoji }) => {
                pair::say(&format!(
                    "\nOn your phone, Google Messages asks to confirm: tap   {emoji}\n"
                ));
                page.inspect(|page| page.emoji(&emoji));
            }
            Some(phone::Event::Qr { url }) => {
                println!(
                    "Scan this QR code with the scanner of Google Messages on your phone \
                     (Device pairing \u{203a} QR code scanner), not with the camera: the camera \
                     only opens a Google help page. No scanner there? Use --manual instead."
                );
                chat_tui::qr::print_qr(&url);
            }
            Some(phone::Event::Paired) => {
                pair::say("Paired.\nSyncing\u{2026}");
                page.inspect(|page| page.paired());
            }
            // What is stored is shown at once, the rest follows.
            Some(phone::Event::Connected) if !stored.is_empty() => {
                if let Some(pairing) = pairing.take() {
                    pairing.finish();
                }
                break None;
            }
            Some(phone::Event::Ready {
                conversations,
                since,
                partial,
            }) => {
                if let Some(pairing) = pairing.take() {
                    pairing.finish();
                }
                break Some((conversations, since, partial));
            }
            Some(phone::Event::Status { message }) => pair::say(&message),
            Some(phone::Event::Fatal { message }) => return Err(message.into()),
            Some(phone::Event::Unpaired { reason }) => {
                return Err(format!("unpaired from the phone ({reason})").into());
            }
            Some(
                phone::Event::Connected
                | phone::Event::History { .. }
                | phone::Event::Downloaded { .. }
                | phone::Event::Done { .. }
                | phone::Event::Contacts { .. }
                | phone::Event::Started { .. }
                | phone::Event::Gone { .. }
                | phone::Event::Sent { .. }
                | phone::Event::Message { .. }
                | phone::Event::Conversation { .. }
                | phone::Event::Typing { .. },
            ) => {}
            None => return Err("the connection to the phone stopped".into()),
        }
    };
    // The last message stored: those received after it, fetched, were missed meanwhile.
    let stopped_ms = stored
        .iter()
        .flat_map(|c| c.messages.iter().map(|m| m.timestamp_ms))
        .max();
    let mut discussions: Vec<Discussion> = stored.into_iter().map(stored_discussion).collect();
    let (lines, commands, control) = phone.split();
    let mut sync = Sync {
        store: store.clone(),
        commands: commands.clone(),
        known: discussions.iter().map(|d| d.id.clone()).collect(),
        read: HashSet::new(),
        looked_up: HashSet::new(),
        fetching: HashSet::new(),
        stopped_ms,
        missed: Vec::new(),
    };
    // What changed since last time (hidden, renamed, read on the phone): the first events.
    let mut changes = Vec::new();
    if let Some((conversations, since, partial)) = ready {
        for event in sync.ready(conversations, since, partial) {
            match event {
                Event::Chat(discussion) if !discussions.iter().any(|d| d.id == discussion.id) => {
                    discussions.push(discussion)
                }
                event => changes.push(event),
            }
        }
    }
    let backend = GMessages {
        commands,
        store,
        waiting: Waiting::default(),
        next_id: Cell::new(0),
        read: RefCell::default(),
    };
    let events = events(lines, sync, backend.waiting.clone(), changes);
    let result = chat_tui::run(backend, discussions, events, config).await;
    control.stop().await;
    Ok(result?)
}

/// What keeps the store and the phone in step.
struct Sync {
    store: Rc<store::Store>,
    commands: phone::Commands,
    /// The conversations the interface has.
    known: HashSet<ChatId>,
    /// The conversations read on the phone: still read once their messages come.
    read: HashSet<ChatId>,
    /// The conversations asked about to the phone, missing from its lists.
    looked_up: HashSet<ChatId>,
    /// The conversations whose messages are being fetched.
    fetching: HashSet<ChatId>,
    /// When the last message stored was sent, on start (`None`: none was).
    stopped_ms: Option<i64>,
    /// Messages received after it, fetched: notified once every fetch is done.
    missed: Vec<(ChatId, MessageId)>,
}

impl Sync {
    /// The conversations listed by the phone (inbox, then hidden, the inbox complete from
    /// `since`; the most recent ones only if `partial`): stored, the messages of those that
    /// changed fetched. Returns them as interface events: those not known yet, the others
    /// updated, those read on the phone. Those missing from complete lists are asked about
    /// (in the trash, or deleted).
    fn ready(
        &mut self,
        conversations: Vec<phone::Conversation>,
        since: i64,
        partial: bool,
    ) -> Vec<Event> {
        if let Err(e) = self.store.save_conversations(&conversations) {
            pair::say(&format!("Cannot store the conversations: {e}"));
        }
        let listed: Vec<&str> = conversations.iter().map(|c| c.id.as_str()).collect();
        let unlisted = if partial {
            Vec::new()
        } else {
            self.store.unlisted(&listed, since).unwrap_or_default()
        };
        if !unlisted.is_empty() {
            self.looked_up.extend(unlisted.iter().cloned());
            self.commands.send(phone::Command::Lookup(unlisted));
        }
        let changed = self.store.changed().unwrap_or_default();
        self.fetch(changed.into_iter().collect());
        let mut events = Vec::new();
        for conversation in conversations {
            events.extend(self.conversation(conversation));
        }
        events
    }

    /// Fetches the messages of `conversations`, unless being fetched already.
    fn fetch(&mut self, conversations: Vec<ChatId>) {
        let conversations: Vec<ChatId> = conversations
            .into_iter()
            .filter(|c| self.fetching.insert(c.clone()))
            .collect();
        if !conversations.is_empty() {
            self.commands.send(phone::Command::Fetch(conversations));
        }
    }

    /// The messages of `conversation` fetched, or not (`error`): once every fetch is done,
    /// those missed, if any.
    fn fetched(&mut self, conversation: &str, error: Option<String>) -> Option<Event> {
        self.fetching.remove(conversation);
        if let Some(error) = error {
            return Some(Event::Status(format!("Cannot fetch the messages: {error}")));
        }
        (self.fetching.is_empty() && !self.missed.is_empty())
            .then(|| Event::Missed(std::mem::take(&mut self.missed)))
    }

    /// A conversation looked up the phone no longer has: forgotten, and hidden.
    fn gone(&mut self, conversation: &str) -> Option<Event> {
        self.looked_up.remove(conversation);
        if let Err(e) = self.store.forget(conversation) {
            pair::say(&format!("Cannot forget the conversation: {e}"));
        }
        let gone = self
            .store
            .conversations_hidden(&[conversation.to_string()])
            .ok()?
            .pop()?;
        Some(Event::Chat(stored_discussion(gone)))
    }

    /// A message arriving live: stored, then shown, as new (notified) unless known already.
    /// Old ones (replayed when the event stream opens again) are shown as history: those
    /// received meanwhile, not known, are notified with the missed ones.
    fn live(&mut self, conversation: String, m: phone::Message, old: bool) -> Vec<Event> {
        let missed = old && self.is_missed(&m);
        let known = matches!(self.store.message(&m.id), Ok(Some(_)));
        // Deleted, and not known (or forgotten, e.g. not sent and sent again): nothing to show.
        if m.deleted && !known {
            return vec![Event::History {
                chat: conversation,
                messages: Vec::new(),
            }];
        }
        let stored = self
            .store
            .save_messages(&conversation, std::slice::from_ref(&m))
            .and_then(|()| Ok(self.store.arrived(&conversation, &m.id)?));
        if let Err(e) = stored {
            return vec![Event::Status(format!("Cannot store the message: {e}"))];
        }
        if missed {
            self.missed.push((conversation.clone(), m.id.clone()));
        }
        let (message, quote) = message(m);
        if known || old {
            let history = Event::History {
                chat: conversation,
                messages: vec![(message, quote)],
            };
            // While fetching, they are notified with those the fetches find.
            let missed = (missed && self.fetching.is_empty())
                .then(|| Event::Missed(std::mem::take(&mut self.missed)));
            std::iter::once(history).chain(missed).collect()
        } else {
            vec![Event::Message {
                chat: conversation,
                message,
                quote,
            }]
        }
    }

    /// A conversation new or changed on the phone, as interface events: the discussion (new,
    /// or its title, folder and visibility for a known one), then whether it was read there
    /// (or is unread: marked so, or new messages).
    /// Deleted for good, its messages are forgotten.
    fn conversation(&mut self, conversation: phone::Conversation) -> Vec<Event> {
        if conversation.deleted
            && let Err(e) = self.store.forget(&conversation.id)
        {
            pair::say(&format!("Cannot forget the conversation: {e}"));
        }
        let read = if conversation.unread {
            self.read.remove(&conversation.id);
            conversation.latest.as_ref().map(|latest| Event::Unread {
                chat: conversation.id.clone(),
                latest: latest.id.clone(),
            })
        } else {
            self.read.insert(conversation.id.clone());
            Some(Event::Read {
                chat: conversation.id.clone(),
            })
        };
        let messages = if self.known.insert(conversation.id.clone()) {
            self.store.messages(&conversation.id).unwrap_or_default()
        } else {
            Vec::new()
        };
        let discussion = if messages.is_empty() {
            discussion(conversation)
        } else {
            stored_discussion(store::Conversation {
                id: conversation.id,
                name: conversation.name,
                group: conversation.group,
                timestamp_ms: conversation.timestamp_ms,
                unread: conversation.unread,
                hidden: conversation.deleted,
                folder: conversation.folder,
                messages,
            })
        };
        std::iter::once(Event::Chat(discussion))
            .chain(read)
            .collect()
    }

    /// Whether `m`, not stored yet, was received since the last message stored on start.
    fn is_missed(&self, m: &phone::Message) -> bool {
        self.stopped_ms.is_some_and(|stopped| {
            !m.from_me && m.timestamp_ms > stopped && matches!(self.store.message(&m.id), Ok(None))
        })
    }

    /// Messages of `conversation`: stored (those received since the last start, not stored
    /// yet, missed), then as the interface shows them.
    fn history(
        &mut self,
        conversation: &str,
        messages: Vec<phone::Message>,
    ) -> Vec<(Message, Option<MessageId>)> {
        let missed: Vec<_> = messages
            .iter()
            .filter(|m| self.is_missed(m))
            .map(|m| (conversation.to_string(), m.id.clone()))
            .collect();
        self.missed.extend(missed);
        if let Err(e) = self
            .store
            .save_messages(conversation, &messages)
            .and_then(|()| Ok(self.store.fetched(conversation)?))
        {
            pair::say(&format!("Cannot store the messages: {e}"));
        }
        messages.into_iter().map(message).collect()
    }
}

/// The interface folder of a conversation filed in `folder` by the phone.
fn folder(folder: Option<&str>) -> Option<Folder> {
    match folder? {
        "archive" => Some(Folder::Archive),
        "spam" => Some(Folder::Spam),
        "trash" => Some(Folder::Trash),
        _ => None,
    }
}

/// The interface discussion of a stored conversation.
fn stored_discussion(conversation: store::Conversation) -> Discussion {
    let mut discussion = Discussion::new(conversation.id, conversation.name);
    discussion.is_group = conversation.group;
    discussion.hidden = conversation.hidden;
    discussion.folder = folder(conversation.folder.as_deref());
    let messages: Vec<_> = conversation.messages.into_iter().map(message).collect();
    let index: HashMap<MessageId, usize> = messages
        .iter()
        .enumerate()
        .map(|(i, (m, _))| (m.id.clone(), i))
        .collect();
    discussion.messages = messages
        .into_iter()
        .map(|(mut m, quote)| {
            m.reply_to = quote.and_then(|id| index.get(&id).copied());
            m
        })
        .collect();
    discussion
}

/// The interface discussion of a conversation, with its latest message until its history
/// comes.
fn discussion(conversation: phone::Conversation) -> Discussion {
    let mut discussion = Discussion::new(conversation.id, conversation.name);
    discussion.is_group = conversation.group;
    discussion.hidden = conversation.deleted;
    discussion.folder = folder(conversation.folder.as_deref());
    if let Some(latest) = conversation.latest {
        let sent_ms = conversation.timestamp_ms.max(0) as u64;
        let mut message = Message::mine(latest.id, latest.text, None, false);
        message.sent_ms = sent_ms;
        if let Some(time) = chrono::DateTime::from_timestamp_millis(sent_ms as i64) {
            message.timestamp = time.with_timezone(&chrono::Local).naive_local();
        }
        message.from_me = latest.from_me;
        if !latest.from_me {
            message.author = Some(latest.sender.clone());
            message.sender_name = latest.sender;
        }
        discussion.messages.push(message);
    }
    discussion
}

/// The interface message of `m`, with the id of the one it quotes.
fn message(m: phone::Message) -> (Message, Option<MessageId>) {
    let mut message = Message::mine(m.id, m.text, None, false);
    message.sent_ms = m.timestamp_ms.max(0) as u64;
    if let Some(time) = chrono::DateTime::from_timestamp_millis(m.timestamp_ms) {
        message.timestamp = time.with_timezone(&chrono::Local).naive_local();
    }
    message.from_me = m.from_me;
    if !m.from_me {
        message.author = Some(m.sender_id);
        message.sender_name = m.sender;
    }
    message.remote_attachments = m
        .media
        .iter()
        .map(|medium| {
            medium["mime"]
                .as_str()
                .unwrap_or("application/octet-stream")
                .to_string()
        })
        .collect();
    for reaction in m.reactions {
        message.set_reaction(&reaction.sender_id, reaction.mine, Some(&reaction.emoji));
    }
    if m.deleted {
        message.mark_deleted();
    }
    message.failed = m.failed;
    (message, m.quote.filter(|id| !id.is_empty()))
}

/// The interface attachment of a downloaded medium.
fn attachment(a: phone::Attachment) -> Attachment {
    Attachment {
        content_type: a.content_type,
        file_name: a.name.filter(|name| !name.is_empty()),
        bytes: a.data,
        width: a.width.filter(|&w| w > 0),
        height: a.height.filter(|&h| h > 0),
    }
}

/// What the phone reports while the interface runs, as interface events.
fn events(
    mut lines: UnboundedReceiver<phone::Event>,
    mut sync: Sync,
    waiting: Waiting,
    first: Vec<Event>,
) -> UnboundedReceiver<Event> {
    let (events, received) = unbounded();
    for event in first {
        let _ = events.unbounded_send(event);
    }
    tokio::task::spawn_local(async move {
        while let Some(event) = futures::StreamExt::next(&mut lines).await {
            let event = match event {
                phone::Event::Ready {
                    conversations,
                    since,
                    partial,
                } => {
                    for event in sync.ready(conversations, since, partial) {
                        if events.unbounded_send(event).is_err() {
                            return;
                        }
                    }
                    continue;
                }
                phone::Event::Done { id, error } => {
                    if let Some(done) = waiting.actions.borrow_mut().remove(&id) {
                        let _ = done.send(error.map_or(Ok(()), Err));
                    }
                    continue;
                }
                phone::Event::Contacts {
                    id,
                    contacts,
                    error,
                } => {
                    if let Some(done) = waiting.contacts.borrow_mut().remove(&id) {
                        let _ = done.send(match error {
                            Some(error) => Err(error),
                            None => Ok(contacts
                                .into_iter()
                                .map(|c| Contact {
                                    name: c.name,
                                    number: c.number,
                                })
                                .collect()),
                        });
                    }
                    continue;
                }
                phone::Event::Started {
                    id,
                    conversation,
                    error,
                } => {
                    if let Some(done) = waiting.started.borrow_mut().remove(&id) {
                        let _ =
                            done.send(conversation.ok_or_else(|| {
                                error.unwrap_or_else(|| "no chat started".to_string())
                            }));
                    }
                    continue;
                }
                phone::Event::Downloaded {
                    id,
                    attachments,
                    error,
                } => {
                    if let Some(done) = waiting.downloads.borrow_mut().remove(&id) {
                        let _ = done.send(match error {
                            Some(error) => Err(error),
                            None => Ok(attachments.into_iter().map(attachment).collect()),
                        });
                    }
                    continue;
                }
                phone::Event::Sent {
                    id,
                    conversation,
                    outcome,
                } => {
                    let (message, error) = match outcome {
                        phone::SendOutcome::Sent(message)
                        | phone::SendOutcome::Accepted(message) => (Some(message), None),
                        phone::SendOutcome::Unavailable(error)
                        | phone::SendOutcome::Rejected(error)
                        | phone::SendOutcome::Uncertain(error) => (None, Some(error)),
                    };
                    let done = waiting.sends.borrow_mut().remove(&id);
                    match (done, message, error) {
                        (Some(done), Some(message), None) => {
                            let _ = done.send(Ok(message));
                            continue;
                        }
                        (Some(done), _, error) => {
                            let _ = done.send(Err(error.unwrap_or_else(|| "not sent".to_string())));
                            continue;
                        }
                        // Sent although given up (Esc): shown and stored all the same.
                        (None, Some(message), None) => Event::History {
                            messages: sync.history(&conversation, vec![message]),
                            chat: conversation,
                        },
                        (None, ..) => continue,
                    }
                }
                phone::Event::Message {
                    conversation,
                    message,
                    old,
                } => {
                    for event in sync.live(conversation, message, old) {
                        if events.unbounded_send(event).is_err() {
                            return;
                        }
                    }
                    continue;
                }
                phone::Event::Gone { conversation } => match sync.gone(&conversation) {
                    Some(event) => event,
                    None => continue,
                },
                phone::Event::Conversation { conversation } => {
                    if let Err(e) = sync
                        .store
                        .save_conversations(std::slice::from_ref(&conversation))
                    {
                        pair::say(&format!("Cannot store the conversation: {e}"));
                    }
                    // Found in the trash: its messages, forgotten if it was taken as deleted.
                    if sync.looked_up.remove(&conversation.id)
                        && !conversation.deleted
                        && sync
                            .store
                            .changed()
                            .is_ok_and(|c| c.contains(&conversation.id))
                    {
                        sync.fetch(vec![conversation.id.clone()]);
                    }
                    for event in sync.conversation(conversation) {
                        if events.unbounded_send(event).is_err() {
                            return;
                        }
                    }
                    continue;
                }
                phone::Event::Typing {
                    conversation,
                    author,
                    name,
                    started,
                } => Event::Typing {
                    chat: conversation,
                    author,
                    name,
                    started,
                },
                phone::Event::History {
                    conversation,
                    error: Some(error),
                    ..
                } => match sync.fetched(&conversation, Some(error)) {
                    Some(event) => event,
                    None => continue,
                },
                phone::Event::History {
                    conversation,
                    messages,
                    error: None,
                } => {
                    let read = sync.read.contains(&conversation).then(|| Event::Read {
                        chat: conversation.clone(),
                    });
                    let messages = sync.history(&conversation, messages);
                    let missed = sync.fetched(&conversation, None);
                    let history = Event::History {
                        messages,
                        chat: conversation,
                    };
                    for event in std::iter::once(history).chain(read).chain(missed) {
                        if events.unbounded_send(event).is_err() {
                            return;
                        }
                    }
                    continue;
                }
                phone::Event::Status { message } => Event::Status(message),
                phone::Event::Expired { message } => Event::Status(format!(
                    "Disconnected: {message}. Restart {NAME} to sign in again"
                )),
                phone::Event::Fatal { message } => {
                    Event::Status(format!("Disconnected: {message}"))
                }
                phone::Event::Unpaired { reason } => {
                    Event::Status(format!("Unpaired from the phone ({reason})"))
                }
                _ => continue,
            };
            if events.unbounded_send(event).is_err() {
                break;
            }
        }
    });
    received
}

/// Creates `dir`, readable by the user only: the session in it gives access to the messages.
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// Requests waiting for the phone's answer, by request id.
type Pending<T> = Rc<RefCell<HashMap<u64, oneshot::Sender<std::result::Result<T, String>>>>>;

/// What waits for the phone: downloads, messages being sent, conversations archived or deleted.
#[derive(Clone, Default)]
struct Waiting {
    downloads: Pending<Vec<Attachment>>,
    sends: Pending<phone::Message>,
    actions: Pending<()>,
    contacts: Pending<Vec<Contact>>,
    started: Pending<phone::Conversation>,
}

/// Waits for the answer to request `id` of `pending`, sent if `sent`.
async fn answer<T>(
    pending: &Pending<T>,
    id: u64,
    answer: oneshot::Receiver<std::result::Result<T, String>>,
    sent: bool,
    timeout: Duration,
) -> Result<T> {
    // Given up (dropped, e.g. Esc while sending) or answered: not waiting any more.
    struct Forget<'a, T>(&'a Pending<T>, u64);
    impl<T> Drop for Forget<'_, T> {
        fn drop(&mut self) {
            self.0.borrow_mut().remove(&self.1);
        }
    }
    let _forget = Forget(pending, id);
    let answer = if sent {
        tokio::time::timeout(timeout, answer).await
    } else {
        Ok(Err(oneshot::Canceled))
    };
    match answer {
        Ok(Ok(Ok(value))) => Ok(value),
        Ok(Ok(Err(error))) => Err(error.into()),
        Ok(Err(_)) => Err("the connection to the phone has stopped".into()),
        Err(_) => Err("no answer from the phone in time".into()),
    }
}

/// How long a download may take.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);
/// How long sending may take (uploading attachments included).
const SEND_TIMEOUT: Duration = Duration::from_secs(120);
/// Archiving or deleting a conversation, given up after.
const ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Google Messages, for the interface: shows the conversations and downloads their media
/// so far.
struct GMessages {
    commands: phone::Commands,
    store: Rc<store::Store>,
    waiting: Waiting,
    next_id: Cell<u64>,
    /// The last message said read on the phone, per conversation.
    read: RefCell<HashMap<ChatId, MessageId>>,
}

impl GMessages {
    /// Asks the phone `command` (made with its request id), until it is done.
    fn chat_action(
        &self,
        command: impl FnOnce(u64) -> phone::Command,
    ) -> impl Future<Output = Result<()>> + 'static {
        let id = self.request();
        let (done, done_rx) = oneshot::channel();
        self.waiting.actions.borrow_mut().insert(id, done);
        let sent = self.commands.send(command(id));
        let actions = self.waiting.actions.clone();
        async move { answer(&actions, id, done_rx, sent, ACTION_TIMEOUT).await }
    }

    /// A new request id.
    fn request(&self) -> u64 {
        let id = self.next_id.get() + 1;
        self.next_id.set(id);
        id
    }
}

/// The futures will own what they need (a handle to the client): `'static`, as the trait
/// wants.
#[allow(clippy::manual_async_fn)]
impl Backend for GMessages {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            chat_actions: true,
            new_chat: true,
            // Deleted on the phone, for us: as Google Messages does.
            delete_for_me: true,
            ..Capabilities::default()
        }
    }

    /// Deleted on the phone, then forgotten here.
    fn discard(
        &self,
        _chat: &ChatId,
        message: &Message,
    ) -> impl Future<Output = Result<()>> + 'static {
        let id = self.request();
        let (done, done_rx) = oneshot::channel();
        self.waiting.actions.borrow_mut().insert(id, done);
        let sent = self.commands.send(phone::Command::DeleteMessage {
            id,
            message: message.id.clone(),
        });
        let (store, actions, message) = (
            self.store.clone(),
            self.waiting.actions.clone(),
            message.id.clone(),
        );
        async move {
            answer(&actions, id, done_rx, sent, ACTION_TIMEOUT).await?;
            store.forget_message(&message)?;
            Ok(())
        }
    }

    fn contacts(&self) -> impl Future<Output = Result<Vec<Contact>>> + 'static {
        let id = self.request();
        let (done, done_rx) = oneshot::channel();
        self.waiting.contacts.borrow_mut().insert(id, done);
        let sent = self.commands.send(phone::Command::Contacts { id });
        let contacts = self.waiting.contacts.clone();
        async move { answer(&contacts, id, done_rx, sent, ACTION_TIMEOUT).await }
    }

    /// Asked to the phone, then stored; with its messages stored, those since fetched.
    fn start_chat(
        &self,
        numbers: Vec<String>,
    ) -> impl Future<Output = Result<Discussion>> + 'static {
        let id = self.request();
        let (done, done_rx) = oneshot::channel();
        self.waiting.started.borrow_mut().insert(id, done);
        let sent = self
            .commands
            .send(phone::Command::StartChat { id, numbers });
        let (store, commands, started) = (
            self.store.clone(),
            self.commands.clone(),
            self.waiting.started.clone(),
        );
        async move {
            let conversation = answer(&started, id, done_rx, sent, ACTION_TIMEOUT).await?;
            store.save_conversations(std::slice::from_ref(&conversation))?;
            if store.changed()?.contains(&conversation.id) {
                commands.send(phone::Command::Fetch(vec![conversation.id.clone()]));
            }
            let messages = store.messages(&conversation.id)?;
            Ok(if messages.is_empty() {
                discussion(conversation)
            } else {
                stored_discussion(store::Conversation {
                    id: conversation.id,
                    name: conversation.name,
                    group: conversation.group,
                    timestamp_ms: conversation.timestamp_ms,
                    unread: conversation.unread,
                    hidden: conversation.deleted,
                    folder: conversation.folder,
                    messages,
                })
            })
        }
    }

    fn archive(&self, chat: &ChatId) -> impl Future<Output = Result<()>> + 'static {
        let conversation = chat.clone();
        self.chat_action(|id| phone::Command::Archive { id, conversation })
    }

    /// To the trash of the phone; for good (its messages then forgotten here, see
    /// [`Sync::conversation`]) only when shown in the trash, where the interface asks
    /// "for good?": never on the phone's word alone.
    fn delete_chat(&self, chat: &ChatId) -> impl Future<Output = Result<()>> + 'static {
        let for_good = matches!(self.store.folder(chat), Ok(Some(folder)) if folder == "trash");
        let conversation = chat.clone();
        self.chat_action(move |id| phone::Command::DeleteChat {
            id,
            conversation,
            for_good,
        })
    }

    fn restore(&self, chat: &ChatId) -> impl Future<Output = Result<()>> + 'static {
        let conversation = chat.clone();
        self.chat_action(|id| phone::Command::Restore { id, conversation })
    }

    /// Sent to the phone, then stored with its attachments: as the phone echoed it, with its
    /// id.
    fn send(&self, chat: &ChatId, draft: Draft) -> impl Future<Output = Result<Message>> + 'static {
        let id = self.request();
        let (done, sent) = oneshot::channel();
        self.waiting.sends.borrow_mut().insert(id, done);
        let attachments = draft
            .attachments
            .iter()
            .map(|a| phone::Attachment {
                content_type: a.content_type.clone(),
                name: a.file_name.clone(),
                data: a.bytes.clone(),
                width: a.width,
                height: a.height,
            })
            .collect();
        let ok = self.commands.send(phone::Command::Send {
            id,
            conversation: chat.clone(),
            text: draft.text.clone(),
            reply: draft.reply_to.as_ref().map(|m| m.id.clone()),
            attachments,
        });
        let (store, sends, chat) = (self.store.clone(), self.waiting.sends.clone(), chat.clone());
        async move {
            let sent = answer(&sends, id, sent, ok, SEND_TIMEOUT).await?;
            store.save_messages(&chat, std::slice::from_ref(&sent))?;
            if !draft.attachments.is_empty() {
                store.save_attachments(&sent.id, &draft.attachments)?;
            }
            let (mut message, _) = message(sent);
            message.remote_attachments.clear();
            message.attachments = draft.attachments;
            Ok(message)
        }
    }

    /// Said to the phone: added, in place of ours if we had reacted, or removed.
    fn react(
        &self,
        chat: &ChatId,
        message: &Message,
        emoji: &str,
        remove: bool,
    ) -> impl Future<Output = Result<()>> + 'static {
        let op = if remove {
            phone::ReactionOp::Remove
        } else if message.reactions.iter().any(|r| r.mine) {
            phone::ReactionOp::Replace
        } else {
            phone::ReactionOp::Add
        };
        let (conversation, message, emoji) = (chat.clone(), message.id.clone(), emoji.to_string());
        self.chat_action(move |id| phone::Command::React {
            id,
            conversation,
            message,
            emoji,
            op,
        })
    }

    /// Deleted on the phone, for us, then forgotten here (as a message discarded).
    fn delete(
        &self,
        chat: &ChatId,
        message: &Message,
    ) -> impl Future<Output = Result<()>> + 'static {
        self.discard(chat, message)
    }

    /// Said to the phone, which tells the others.
    fn typing(&self, chat: &ChatId) {
        self.commands.send(phone::Command::Typing {
            conversation: chat.clone(),
        });
    }

    /// Said to the phone, once per last message.
    fn read(&self, chat: &ChatId, last: &Message) {
        if last.id.is_empty() || self.read.borrow().get(chat) == Some(&last.id) {
            return;
        }
        self.read.borrow_mut().insert(chat.clone(), last.id.clone());
        self.commands.send(phone::Command::Read {
            conversation: chat.clone(),
            up_to: last.id.clone(),
        });
    }

    /// From the store if downloaded already; else from the phone, then stored.
    fn download(
        &self,
        _chat: &ChatId,
        message: &Message,
    ) -> impl Future<Output = Result<Downloaded>> + 'static {
        let (store, commands, downloads) = (
            self.store.clone(),
            self.commands.clone(),
            self.waiting.downloads.clone(),
        );
        let message_id = message.id.clone();
        let id = self.request();
        async move {
            if let Some(attachments) = store.attachments(&message_id)? {
                return Ok((attachments, Vec::new()));
            }
            let media = store
                .message(&message_id)?
                .map(|m| m.media)
                .unwrap_or_default();
            if media.is_empty() {
                return Err("no media known for this message yet".into());
            }
            let (done, answer_rx) = oneshot::channel();
            downloads.borrow_mut().insert(id, done);
            let sent = commands.send(phone::Command::Download {
                id,
                message: message_id.clone(),
                media,
            });
            let attachments = answer(&downloads, id, answer_rx, sent, DOWNLOAD_TIMEOUT).await?;
            store.save_attachments(&message_id, &attachments)?;
            Ok((attachments, Vec::new()))
        }
    }
}

/// The cache directory where earlier versions kept gmd, if any.
fn cache_dir() -> Option<std::path::PathBuf> {
    let var = |name| {
        std::env::var_os(name)
            .map(std::path::PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|home| home.join("Library/Caches"))
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|home| home.join(".cache")))
    };
    base.map(|dir| dir.join(NAME))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, ms: i64, from_me: bool) -> phone::Message {
        serde_json::from_value(serde_json::json!({
            "id": id, "timestamp_ms": ms, "from_me": from_me, "sender_id": "2",
            "sender": "Ann", "text": "",
        }))
        .unwrap()
    }

    fn missed(events: &[Event]) -> Option<Vec<MessageId>> {
        events.iter().find_map(|e| match e {
            Event::Missed(missed) => Some(missed.iter().map(|(_, id)| id.clone()).collect()),
            _ => None,
        })
    }

    /// A sync over a new store (at the path returned) holding conversation "c" with one
    /// message received at 100, the last one on the previous start.
    fn sync() -> (Sync, std::path::PathBuf) {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "gmessages-sync-{:x}.db",
            u64::from_ne_bytes(random)
        ));
        let store = Rc::new(store::Store::open(&path, "secret").unwrap());
        let conversations: Vec<phone::Conversation> = serde_json::from_value(
            serde_json::json!([{"id": "c", "name": "C", "group": false, "timestamp_ms": 1}]),
        )
        .unwrap();
        store.save_conversations(&conversations).unwrap();
        store
            .save_messages("c", &[message("stored", 100, false)])
            .unwrap();
        let (commands, _sent) = phone::Commands::detached();
        let sync = Sync {
            store,
            commands,
            known: HashSet::new(),
            read: HashSet::new(),
            looked_up: HashSet::new(),
            fetching: HashSet::new(),
            stopped_ms: Some(100),
            missed: Vec::new(),
        };
        (sync, path)
    }

    #[test]
    fn read_or_unread_on_the_phone_is_told() {
        let (mut sync, path) = sync();
        let conversation = |unread: bool| {
            serde_json::from_value(serde_json::json!({
                "id": "c", "name": "C", "group": false, "timestamp_ms": 1, "unread": unread,
                "latest": {"id": "stored", "text": "", "from_me": false, "sender": "Ann"},
            }))
            .unwrap()
        };
        let events = sync.conversation(conversation(true));
        assert!(
            matches!(&events[..], [Event::Chat(_), Event::Unread { chat, latest }]
                if chat == "c" && latest == "stored"),
            "{events:?}"
        );
        let events = sync.conversation(conversation(false));
        assert!(
            matches!(&events[..], [Event::Chat(_), Event::Read { chat }] if chat == "c"),
            "{events:?}"
        );
        drop(sync);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn replayed_messages_received_meanwhile_are_notified() {
        let (mut sync, path) = sync();
        let live = |sync: &mut Sync, id, ms, from_me, old| {
            sync.live("c".to_string(), message(id, ms, from_me), old)
        };

        // Replayed, received meanwhile: shown, then notified.
        let events = live(&mut sync, "new", 200, false, true);
        assert!(matches!(events[0], Event::History { .. }));
        assert_eq!(missed(&events), Some(vec!["new".to_string()]));
        // Replayed again, ours, or older than the last one stored: only shown.
        assert_eq!(missed(&live(&mut sync, "new", 200, false, true)), None);
        assert_eq!(missed(&live(&mut sync, "mine", 300, true, true)), None);
        assert_eq!(missed(&live(&mut sync, "older", 50, false, true)), None);
        // Live: a message, notified as such.
        let events = live(&mut sync, "live", 400, false, false);
        assert!(matches!(events[..], [Event::Message { .. }]));

        // While fetching: notified with those the fetch finds.
        sync.fetching.insert("c".to_string());
        assert_eq!(missed(&live(&mut sync, "during", 500, false, true)), None);
        let fetched = sync.fetched("c", None).map(|e| vec![e]).unwrap_or_default();
        assert_eq!(missed(&fetched), Some(vec!["during".to_string()]));

        drop(sync);
        let _ = std::fs::remove_file(&path);
    }
}
