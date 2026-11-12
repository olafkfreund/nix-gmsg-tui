//! Keeps the encrypted store, phone events and wire events in step.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use qrcode::QrCode;
use qrcode::render::svg;
use serde_json::to_value;
use sha2::{Digest, Sha256};

use crate::{phone, proto, store};

pub struct Sync {
    store: Rc<store::Store>,
    commands: phone::Commands,
    known: HashSet<String>,
    looked_up: HashSet<String>,
    fetching: HashSet<String>,
    stopped_ms: Option<i64>,
    missed: Vec<String>,
    focus: HashMap<u64, String>,
    meta: HashMap<String, (String, Option<String>)>,
    avatars: HashMap<String, String>,
    queue: VecDeque<String>,
    tried: HashSet<String>,
    failed_once: HashSet<String>,
    in_flight: Option<(u64, Vec<String>)>,
    next_token: u64,
    ready: bool,
    avatar_dir: PathBuf,
}

impl Sync {
    pub fn new(store: Rc<store::Store>, commands: phone::Commands, avatar_dir: PathBuf) -> Self {
        let stored = store.conversation_list().unwrap_or_default();
        let meta = stored
            .iter()
            .map(|c| (c.id.clone(), (c.colour.clone(), c.avatar_id.clone())))
            .collect();
        // The last message seen; a fresh store starts now, so what arrives later counts as missed.
        let stopped_ms = stored
            .iter()
            .flat_map(|c| c.messages.iter().map(|m| m.timestamp_ms))
            .max()
            .or_else(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_millis() as i64)
            });
        let known = stored.iter().map(|c| c.id.clone()).collect();
        Self {
            store,
            commands,
            known,
            looked_up: HashSet::new(),
            fetching: HashSet::new(),
            stopped_ms,
            missed: Vec::new(),
            focus: HashMap::new(),
            meta,
            avatars: HashMap::new(),
            queue: VecDeque::new(),
            tried: HashSet::new(),
            failed_once: HashSet::new(),
            in_flight: None,
            next_token: 0,
            ready: false,
            avatar_dir,
        }
    }

    fn merge_meta(&mut self, c: &phone::Conversation) {
        let entry = self.meta.entry(c.id.clone()).or_default();
        if !c.colour.is_empty() {
            entry.0.clone_from(&c.colour);
        }
        if c.group {
            entry.1 = None;
        } else if c.avatar_id.is_some() {
            entry.1.clone_from(&c.avatar_id);
        }
    }

    fn decoration(&self, id: &str) -> (String, Option<String>) {
        self.meta.get(id).map_or_else(
            || (String::new(), None),
            |(color, avatar_id)| {
                (
                    color.clone(),
                    avatar_id
                        .as_ref()
                        .and_then(|id| self.avatars.get(id))
                        .cloned(),
                )
            },
        )
    }

    fn enqueue(&mut self, id: &str) {
        if !self.queue.iter().any(|queued| queued == id)
            && !self
                .in_flight
                .as_ref()
                .is_some_and(|(_, ids)| ids.iter().any(|sent| sent == id))
            && !self.tried.contains(id)
            && !self.avatars.contains_key(id)
        {
            self.queue.push_back(id.to_owned());
        }
    }

    fn pump(&mut self) {
        if !self.ready || self.in_flight.is_some() || self.queue.is_empty() {
            return;
        }
        let ids: Vec<_> = self.queue.drain(..self.queue.len().min(50)).collect();
        self.next_token += 1;
        let id = self.next_token;
        self.commands.send(phone::Command::Avatars {
            id,
            ids: ids.clone(),
        });
        self.in_flight = Some((id, ids));
    }

    fn lose_ready(&mut self) {
        self.ready = false;
        if let Some((_, ids)) = self.in_flight.take() {
            for id in ids.into_iter().rev() {
                self.queue.push_front(id);
            }
        }
    }

    fn reset_avatars(&mut self) {
        self.avatars.clear();
        self.queue.clear();
        self.tried.clear();
        self.failed_once.clear();
        if let Err(error) = crate::serve::clear_media(&self.avatar_dir) {
            tracing::warn!(%error, "cannot clear avatar directory");
        }
    }

    fn avatar_result(
        &mut self,
        id: u64,
        images: Vec<(String, Vec<u8>)>,
        error: Option<String>,
    ) -> Vec<proto::Out> {
        let Some((token, requested)) = self.in_flight.take() else {
            return Vec::new();
        };
        if token != id {
            self.in_flight = Some((token, requested));
            return Vec::new();
        }
        let mut saved = HashMap::new();
        let mut bad = HashSet::new();
        for (key, image) in images {
            if !requested.contains(&key) || saved.contains_key(&key) {
                continue;
            }
            match save_avatar(&self.avatar_dir, &image) {
                Ok(Some(path)) => {
                    saved.insert(key, path);
                }
                Ok(None) => {
                    bad.insert(key);
                }
                Err(error) => {
                    tracing::warn!(%error, "cannot save avatar");
                    bad.insert(key);
                }
            }
        }
        let mut retry = Vec::new();
        let mut new_paths = HashMap::new();
        for key in requested {
            if let Some(path) = saved.remove(&key) {
                new_paths.insert(key.clone(), path.clone());
                self.avatars.insert(key, path);
            } else if bad.contains(&key) {
                self.tried.insert(key);
            } else if error.is_some() && self.failed_once.insert(key.clone()) {
                retry.push(key);
            } else {
                self.tried.insert(key);
            }
        }
        for key in retry {
            self.enqueue(&key);
        }
        tracing::debug!(
            saved = new_paths.len(),
            queued = self.queue.len(),
            tried = self.tried.len(),
            failed = error.is_some(),
            "avatar batch"
        );
        let items: BTreeMap<_, _> = self
            .meta
            .iter()
            .filter_map(|(conversation, (_, avatar_id))| {
                avatar_id
                    .as_ref()
                    .and_then(|id| new_paths.get(id))
                    .map(|path| (conversation.clone(), path.clone()))
            })
            .collect();
        self.pump();
        if items.is_empty() {
            Vec::new()
        } else {
            vec![Self::event("avatars", &proto::AvatarsEvent { items })]
        }
    }

    fn event(name: &str, data: &impl serde::Serialize) -> proto::Out {
        proto::Out::Broadcast(proto::EventLine::new(name, data))
    }

    fn state(state: proto::State, detail: Option<String>) -> proto::Out {
        Self::event("state", &proto::StateEvent { state, detail })
    }

    fn unread_event(&self) -> proto::Out {
        Self::event(
            "unread",
            &proto::UnreadEvent {
                count: self.unread(),
            },
        )
    }

    fn fetch(&mut self, conversations: Vec<String>) {
        let ids: Vec<_> = conversations
            .into_iter()
            .filter(|id| self.fetching.insert(id.clone()))
            .collect();
        if !ids.is_empty() {
            self.commands.send(phone::Command::Fetch(ids));
        }
    }

    fn finish_fetch(&mut self, conversation: &str, out: &mut Vec<proto::Out>) {
        self.fetching.remove(conversation);
        if self.fetching.is_empty() && !self.missed.is_empty() {
            out.push(Self::event(
                "missed",
                &proto::MissedEvent {
                    count: std::mem::take(&mut self.missed).len() as i64,
                },
            ));
        }
    }

    fn is_missed(&self, message: &phone::Message) -> bool {
        self.stopped_ms.is_some_and(|stopped| {
            !message.from_me
                && message.timestamp_ms > stopped
                && matches!(self.store.message(&message.id), Ok(None))
        })
    }

    fn remember_missed(&mut self, message: &phone::Message) {
        if self.is_missed(message) && !self.missed.contains(&message.id) {
            self.missed.push(message.id.clone());
        }
    }

    fn conversation(&mut self, conversation: phone::Conversation) -> Vec<proto::Out> {
        self.merge_meta(&conversation);
        if conversation.deleted
            && let Err(error) = self.store.forget(&conversation.id)
        {
            tracing::warn!(%error, "cannot forget conversation");
        }
        self.known.insert(conversation.id.clone());
        let (color, avatar) = self.decoration(&conversation.id);
        vec![Self::event(
            "conversation",
            &proto::ConversationEvent {
                conversation: proto::Conversation::from_phone(&conversation, color, avatar),
            },
        )]
    }

    fn live(
        &mut self,
        conversation: String,
        message: phone::Message,
        old: bool,
    ) -> Vec<proto::Out> {
        if !self.known.contains(&conversation) && self.looked_up.insert(conversation.clone()) {
            self.commands
                .send(phone::Command::Lookup(vec![conversation.clone()]));
        }
        let known = matches!(self.store.message(&message.id), Ok(Some(_)));
        if message.deleted && !known {
            return Vec::new();
        }
        let missed = old && self.is_missed(&message);
        if let Err(error) = self
            .store
            .save_messages(&conversation, std::slice::from_ref(&message))
            .and_then(|()| Ok(self.store.arrived(&conversation, &message.id)?))
        {
            tracing::warn!(%error, "cannot store message");
            return Vec::new();
        }
        if missed && !self.missed.contains(&message.id) {
            self.missed.push(message.id.clone());
        }
        if !old {
            self.stopped_ms = self.stopped_ms.max(Some(message.timestamp_ms));
        }
        let notify = !old
            && !known
            && !message.deleted
            && !message.from_me
            && !self.focus.values().any(|focused| focused == &conversation);
        let unread_before = self.unread();
        if notify && let Err(error) = self.store.set_unread(&conversation, true) {
            tracing::warn!(%error, "cannot mark conversation unread");
        }
        let mut out = vec![Self::event(
            "message",
            &proto::MessageEvent {
                conversation: conversation.clone(),
                message: proto::Message::from_phone(&conversation, &message, false),
                notify,
            },
        )];
        if self.unread() != unread_before {
            out.push(self.unread_event());
        }
        if missed && self.fetching.is_empty() {
            self.finish_fetch(&conversation, &mut out);
        }
        out
    }

    fn sent(
        &mut self,
        id: u64,
        conversation: String,
        outcome: phone::SendOutcome,
    ) -> Vec<proto::Out> {
        use phone::SendOutcome;
        let (status, message) = match outcome {
            SendOutcome::Sent(message) => (proto::SendStatus::Sent, message),
            SendOutcome::Accepted(message) => (proto::SendStatus::Accepted, message),
            SendOutcome::Unavailable(message) => {
                return vec![proto::Out::Reply {
                    rid: id,
                    result: Err(proto::ErrorBody {
                        code: proto::ErrorCode::PhoneOffline,
                        message,
                    }),
                }];
            }
            SendOutcome::Rejected(message) => {
                return vec![proto::Out::Reply {
                    rid: id,
                    result: Err(proto::ErrorBody {
                        code: proto::ErrorCode::SendRejected,
                        message,
                    }),
                }];
            }
            SendOutcome::Uncertain(message) => {
                return vec![proto::Out::Reply {
                    rid: id,
                    result: Err(proto::ErrorBody {
                        code: proto::ErrorCode::SendUncertain,
                        message,
                    }),
                }];
            }
        };
        if status == proto::SendStatus::Sent
            && let Err(error) = self
                .store
                .save_messages(&conversation, std::slice::from_ref(&message))
        {
            tracing::warn!(%error, "cannot store sent message");
        }
        let pending = status == proto::SendStatus::Accepted;
        let data = proto::SendData {
            status,
            message: proto::Message::from_phone(&conversation, &message, pending),
        };
        let mut out = vec![proto::Out::Reply {
            rid: id,
            result: Ok(to_value(&data).expect("send data serializes")),
        }];
        out.push(Self::event(
            "message",
            &proto::MessageEvent {
                conversation: conversation.clone(),
                message: data.message,
                notify: false,
            },
        ));
        if pending {
            self.fetch(vec![conversation]);
        }
        out
    }

    pub fn handle(&mut self, event: phone::Event) -> Vec<proto::Out> {
        match event {
            phone::Event::Login => {
                self.lose_ready();
                vec![Self::state(proto::State::Login, None)]
            }
            phone::Event::Qr { url } => match QrCode::new(url.as_bytes()) {
                Ok(code) => vec![Self::event(
                    "pairing",
                    &proto::PairingEvent {
                        qr_svg: Some(code.render::<svg::Color>().build()),
                        ..Default::default()
                    },
                )],
                Err(error) => vec![Self::event(
                    "pairing",
                    &proto::PairingEvent {
                        message: Some(format!("cannot render QR code: {error}")),
                        ..Default::default()
                    },
                )],
            },
            phone::Event::Emoji { emoji } => vec![Self::event(
                "pairing",
                &proto::PairingEvent {
                    emoji: Some(emoji),
                    ..Default::default()
                },
            )],
            // pair.cancel already set login; a late "cancelled" must not reset a new pairing.
            phone::Event::PairFailed { message } if message == "cancelled" => Vec::new(),
            phone::Event::PairFailed { message } => {
                self.lose_ready();
                vec![
                    Self::event(
                        "pairing",
                        &proto::PairingEvent {
                            message: Some(message),
                            ..Default::default()
                        },
                    ),
                    Self::state(proto::State::Login, None),
                ]
            }
            phone::Event::Paired => {
                self.lose_ready();
                self.reset_avatars();
                vec![Self::state(proto::State::Connecting, None)]
            }
            phone::Event::Connected => {
                self.lose_ready();
                vec![Self::state(proto::State::Connecting, None)]
            }
            phone::Event::Ready {
                conversations,
                since,
                partial,
            } => {
                self.ready = true;
                if let Err(error) = self.store.save_conversations(&conversations) {
                    tracing::warn!(%error, "cannot store conversations");
                }
                if !partial {
                    let listed: Vec<_> = conversations.iter().map(|c| c.id.as_str()).collect();
                    if let Ok(unlisted) = self.store.unlisted(&listed, since)
                        && !unlisted.is_empty()
                    {
                        self.looked_up.extend(unlisted.iter().cloned());
                        self.commands.send(phone::Command::Lookup(unlisted));
                    }
                }
                self.fetch(self.store.changed().unwrap_or_default());
                let mut out: Vec<_> = conversations
                    .into_iter()
                    .flat_map(|c| self.conversation(c))
                    .collect();
                out.push(Self::state(proto::State::Ready, None));
                out.push(self.unread_event());
                self.pump();
                out
            }
            phone::Event::History {
                conversation,
                messages,
                error,
            } => {
                let mut out = Vec::new();
                if let Some(error) = error {
                    tracing::warn!(%error, "cannot fetch history");
                } else {
                    for message in &messages {
                        self.remember_missed(message);
                    }
                    if let Err(error) = self
                        .store
                        .save_messages(&conversation, &messages)
                        .and_then(|()| Ok(self.store.fetched(&conversation)?))
                    {
                        tracing::warn!(%error, "cannot store history");
                    }
                    let (messages, _) = fit(&conversation, &messages);
                    out.push(Self::event(
                        "history",
                        &proto::HistoryEvent {
                            conversation: conversation.clone(),
                            messages,
                        },
                    ));
                }
                self.finish_fetch(&conversation, &mut out);
                out
            }
            phone::Event::Message {
                conversation,
                message,
                old,
            } => self.live(conversation, message, old),
            phone::Event::Conversation { conversation } => {
                let before = self.unread();
                if let Err(error) = self
                    .store
                    .save_conversations(std::slice::from_ref(&conversation))
                {
                    tracing::warn!(%error, "cannot store conversation");
                }
                if self.looked_up.remove(&conversation.id)
                    && !conversation.deleted
                    && self
                        .store
                        .changed()
                        .is_ok_and(|ids| ids.contains(&conversation.id))
                {
                    self.fetch(vec![conversation.id.clone()]);
                }
                let conversation_id = conversation.id.clone();
                let mut out = self.conversation(conversation);
                if self.ready
                    && let Some(id) = self.meta.get(&conversation_id).and_then(|m| m.1.clone())
                {
                    self.enqueue(&id);
                    self.pump();
                }
                if self.unread() != before {
                    out.push(self.unread_event());
                }
                out
            }
            phone::Event::Gone { conversation } => {
                self.looked_up.remove(&conversation);
                let before = self.unread();
                if let Err(error) = self.store.forget(&conversation) {
                    tracing::warn!(%error, "cannot forget conversation");
                }
                let mut out = self
                    .store
                    .conversations_hidden(&[conversation])
                    .unwrap_or_default()
                    .iter()
                    .map(|c| {
                        Self::event(
                            "conversation",
                            &proto::ConversationEvent {
                                conversation: {
                                    let (color, avatar) = self.decoration(&c.id);
                                    proto::Conversation::from_store(c, color, avatar)
                                },
                            },
                        )
                    })
                    .collect::<Vec<_>>();
                if self.unread() != before {
                    out.push(self.unread_event());
                }
                out
            }
            phone::Event::Typing {
                conversation,
                name,
                started,
                ..
            } => vec![Self::event(
                "typing",
                &proto::TypingEvent {
                    conversation,
                    name,
                    started,
                },
            )],
            phone::Event::Sent {
                id,
                conversation,
                outcome,
            } => self.sent(id, conversation, outcome),
            phone::Event::Expired { message } => {
                self.lose_ready();
                vec![Self::state(proto::State::Expired, Some(message))]
            }
            phone::Event::Unpaired { .. } => {
                self.lose_ready();
                self.reset_avatars();
                vec![Self::state(
                    proto::State::Login,
                    Some("unpaired_remotely".into()),
                )]
            }
            phone::Event::Fatal { message } => {
                self.lose_ready();
                vec![Self::state(proto::State::Fatal, Some(message))]
            }
            phone::Event::Avatars { id, images, error } => self.avatar_result(id, images, error),
            phone::Event::Status { message } => {
                tracing::info!(%message, "phone status");
                Vec::new()
            }
            phone::Event::Done { .. }
            | phone::Event::Downloaded { .. }
            | phone::Event::Contacts { .. }
            | phone::Event::Started { .. } => {
                tracing::debug!("stage-a phone event ignored");
                Vec::new()
            }
        }
    }

    pub fn send(&mut self, rid: u64, conversation: String, text: String, reply_to: Option<String>) {
        self.commands.send(phone::Command::Send {
            id: rid,
            conversation,
            text,
            reply: reply_to,
            attachments: Vec::new(),
        });
    }

    pub fn read(&mut self, conversation: &str) -> Vec<proto::Out> {
        let before = self.unread();
        if let Ok(messages) = self.store.messages(conversation)
            && let Some(last) = messages.last()
        {
            self.commands.send(phone::Command::Read {
                conversation: conversation.into(),
                up_to: last.id.clone(),
            });
        }
        if let Err(error) = self.store.set_unread(conversation, false) {
            tracing::warn!(%error, "cannot mark conversation read");
        }
        if self.unread() != before {
            vec![self.unread_event()]
        } else {
            Vec::new()
        }
    }

    pub fn history(&self, conversation: &str) -> proto::HistoryData {
        let messages = self.store.messages(conversation).unwrap_or_default();
        let (messages, truncated) = fit(conversation, &messages);
        proto::HistoryData {
            messages,
            truncated,
        }
    }

    pub fn conversations(
        &mut self,
        folder: proto::Folder,
        offset: i64,
        limit: i64,
    ) -> proto::ConversationsData {
        let folder = match folder {
            proto::Folder::Inbox => None,
            proto::Folder::Archive => Some("archive"),
            proto::Folder::Spam => Some("spam"),
            proto::Folder::Trash => Some("trash"),
        };
        let conversations: Vec<_> = self
            .store
            .conversation_list()
            .unwrap_or_default()
            .into_iter()
            .filter(|c| !c.hidden && c.folder.as_deref() == folder)
            .collect();
        let items: Vec<_> = conversations
            .iter()
            .skip(offset.max(0) as usize)
            .take(limit.clamp(0, 200) as usize)
            .collect();
        for c in &items {
            if let Some(id) = self.meta.get(&c.id).and_then(|m| m.1.clone()) {
                self.enqueue(&id);
            }
        }
        self.pump();
        proto::ConversationsData {
            total: conversations.len() as i64,
            items: items
                .into_iter()
                .map(|c| {
                    let (color, avatar) = self.decoration(&c.id);
                    proto::Conversation::from_store(c, color, avatar)
                })
                .collect(),
        }
    }

    pub fn set_focus(&mut self, conn: u64, conversation: Option<String>) {
        if let Some(conversation) = conversation {
            self.focus.insert(conn, conversation);
        } else {
            self.focus.remove(&conn);
        }
    }

    pub fn drop_conn(&mut self, conn: u64) {
        self.focus.remove(&conn);
    }

    pub fn unread(&self) -> i64 {
        self.store.unread_count().unwrap_or_default()
    }
}

fn save_avatar(dir: &Path, image: &[u8]) -> io::Result<Option<String>> {
    if image.len() > 256 * 1024 {
        return Ok(None);
    }
    let ext = if image.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if image.starts_with(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]) {
        "png"
    } else if image.starts_with(b"RIFF") && image.get(8..12) == Some(b"WEBP".as_slice()) {
        "webp"
    } else {
        return Ok(None);
    };
    let Ok(size) = imagesize::blob_size(image) else {
        return Ok(None);
    };
    if size.width > 1024 || size.height > 1024 {
        return Ok(None);
    }
    let name = format!("{:x}.{ext}", Sha256::digest(image));
    let path = dir.join(&name);
    if !path.exists() {
        let temp = dir.join(format!(".{name}.tmp"));
        if let Err(error) = fs::remove_file(&temp)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(image)?;
        fs::rename(&temp, &path)?;
    }
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Bytes of messages one history line may carry, well under the daemon's 1 MiB line limit.
const HISTORY_BYTES: usize = 768 * 1024;

/// The newest messages (at most 200) whose wire form fits [`HISTORY_BYTES`], in send order,
/// and whether older ones were left out.
fn fit(conversation: &str, messages: &[crate::phone::Message]) -> (Vec<proto::Message>, bool) {
    let mut kept = Vec::new();
    let mut bytes = 0;
    for m in messages.iter().rev().take(200) {
        let wire = proto::Message::from_phone(conversation, m, false);
        bytes += serde_json::to_string(&wire).map_or(0, |s| s.len() + 1);
        if bytes > HISTORY_BYTES && !kept.is_empty() {
            break;
        }
        kept.push(wire);
    }
    let truncated = kept.len() < messages.len();
    kept.reverse();
    (kept, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::channel::mpsc::UnboundedReceiver;
    use serde_json::{Value, json};
    use std::os::unix::fs::PermissionsExt;

    fn message(id: &str, ms: i64, from_me: bool, deleted: bool) -> phone::Message {
        serde_json::from_value(json!({
            "id": id, "timestamp_ms": ms, "from_me": from_me, "sender_id": "2",
            "sender": "Ann", "text": id, "deleted": deleted
        }))
        .unwrap()
    }

    #[test]
    fn history_fits_the_line_limit() {
        let long = "x".repeat(100 * 1024);
        let messages: Vec<_> = (0..20)
            .map(|i| {
                let mut m = message(&format!("m{i}"), i, false, false);
                m.text = long.clone();
                m
            })
            .collect();
        let (kept, truncated) = fit("c", &messages);
        assert!(truncated);
        assert!(kept.len() < 20 && !kept.is_empty());
        assert_eq!(kept.last().unwrap().id, "m19");
        let bytes: usize = kept
            .iter()
            .map(|m| serde_json::to_string(m).unwrap().len())
            .sum();
        assert!(bytes <= HISTORY_BYTES);
        let (all, truncated) = fit("c", &messages[..3]);
        assert_eq!((all.len(), truncated), (3, false));
    }

    fn conversation(unread: bool) -> phone::Conversation {
        serde_json::from_value(json!({
            "id": "c", "name": "C", "group": false, "timestamp_ms": 1,
            "unread": unread,
            "latest": {"id": "stored", "text": "stored", "from_me": false, "sender": "Ann"}
        }))
        .unwrap()
    }

    fn pictured(id: &str) -> phone::Conversation {
        let mut c = conversation(false);
        c.id = id.into();
        c.colour = "#aabbcc".into();
        c.avatar_id = Some("c:person".into());
        c
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut image = vec![0; 40];
        image[..8].copy_from_slice(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]);
        image[8..12].copy_from_slice(&13u32.to_be_bytes());
        image[12..16].copy_from_slice(b"IHDR");
        image[16..20].copy_from_slice(&width.to_be_bytes());
        image[20..24].copy_from_slice(&height.to_be_bytes());
        image
    }

    fn sync() -> (Sync, UnboundedReceiver<phone::Command>) {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "gmessages-sync-{:x}.db",
            u64::from_ne_bytes(random)
        ));
        let store = Rc::new(store::Store::open(&path, "secret").unwrap());
        store.save_conversations(&[conversation(false)]).unwrap();
        store
            .save_messages("c", &[message("stored", 100, false, false)])
            .unwrap();
        let (commands, received) = phone::Commands::detached();
        let avatar_dir = path.with_extension("avatars");
        crate::serve::clear_media(&avatar_dir).unwrap();
        (Sync::new(store, commands, avatar_dir), received)
    }

    fn data(out: &[proto::Out], event: &str) -> Vec<Value> {
        out.iter()
            .filter_map(|out| match out {
                proto::Out::Broadcast(line) if line.event == event => Some(line.data.clone()),
                _ => None,
            })
            .collect()
    }

    fn live(
        sync: &mut Sync,
        id: &str,
        ms: i64,
        from_me: bool,
        deleted: bool,
        old: bool,
    ) -> Vec<proto::Out> {
        sync.handle(phone::Event::Message {
            conversation: "c".into(),
            message: message(id, ms, from_me, deleted),
            old,
        })
    }

    #[test]
    fn live_notify_rules_and_focus() {
        let (mut sync, _) = sync();
        let out = live(&mut sync, "new", 200, false, false, false);
        assert_eq!(data(&out, "message")[0]["notify"], true);
        assert_eq!(data(&out, "unread")[0]["count"], 1);
        assert_eq!(
            data(&live(&mut sync, "new", 200, false, false, false), "message")[0]["notify"],
            false
        );
        assert!(live(&mut sync, "deleted", 210, false, true, false).is_empty());
        assert_eq!(
            data(&live(&mut sync, "mine", 220, true, false, false), "message")[0]["notify"],
            false
        );
        sync.set_focus(7, Some("c".into()));
        assert_eq!(
            data(
                &live(&mut sync, "focused", 230, false, false, false),
                "message"
            )[0]["notify"],
            false
        );
        sync.drop_conn(7);
        assert_eq!(
            data(
                &live(&mut sync, "unfocused", 240, false, false, false),
                "message"
            )[0]["notify"],
            true
        );
    }

    #[test]
    fn replayed_messages_are_missed_once_after_fetch() {
        let (mut sync, _) = sync();
        sync.fetching.insert("c".into());
        let out = live(&mut sync, "replayed", 200, false, false, true);
        assert_eq!(data(&out, "message")[0]["notify"], false);
        assert!(data(&out, "missed").is_empty());
        let out = sync.handle(phone::Event::History {
            conversation: "c".into(),
            messages: vec![message("fetched", 300, false, false)],
            error: None,
        });
        assert_eq!(data(&out, "history").len(), 1);
        assert_eq!(data(&out, "missed")[0]["count"], 2);
        let out = sync.handle(phone::Event::History {
            conversation: "c".into(),
            messages: vec![message("fetched", 300, false, false)],
            error: None,
        });
        assert!(data(&out, "missed").is_empty());
    }

    #[test]
    fn read_lowers_unread_and_tells_phone_last_message() {
        let (mut sync, mut commands) = sync();
        sync.store
            .save_conversations(&[conversation(true)])
            .unwrap();
        assert_eq!(sync.unread(), 1);
        let out = sync.read("c");
        assert_eq!(data(&out, "unread")[0]["count"], 0);
        assert!(
            matches!(commands.try_recv().unwrap(), phone::Command::Read { conversation, up_to }
            if conversation == "c" && up_to == "stored")
        );
    }

    #[test]
    fn phone_conversation_updates_unread() {
        let (mut sync, _) = sync();
        let out = sync.handle(phone::Event::Conversation {
            conversation: conversation(true),
        });
        assert_eq!(
            data(&out, "conversation")[0]["conversation"]["unread"],
            true
        );
        assert_eq!(data(&out, "unread")[0]["count"], 1);
        let out = sync.handle(phone::Event::Conversation {
            conversation: conversation(false),
        });
        assert_eq!(data(&out, "unread")[0]["count"], 0);
    }

    #[test]
    fn gone_conversation_is_marked_hidden() {
        let (mut sync, _) = sync();
        let out = sync.handle(phone::Event::Gone {
            conversation: "c".into(),
        });
        assert_eq!(
            data(&out, "conversation")[0]["conversation"]["hidden"],
            true
        );
    }

    #[test]
    fn send_outcomes_reply_and_accepted_fetches() {
        let (mut sync, mut commands) = sync();
        for (id, outcome, code) in [
            (
                1,
                phone::SendOutcome::Unavailable("offline".into()),
                proto::ErrorCode::PhoneOffline,
            ),
            (
                2,
                phone::SendOutcome::Rejected("rejected".into()),
                proto::ErrorCode::SendRejected,
            ),
            (
                3,
                phone::SendOutcome::Uncertain("uncertain".into()),
                proto::ErrorCode::SendUncertain,
            ),
        ] {
            let out = sync.handle(phone::Event::Sent {
                id,
                conversation: "c".into(),
                outcome,
            });
            assert!(
                matches!(&out[0], proto::Out::Reply { rid, result: Err(error) }
                if *rid == id && error.code == code)
            );
        }
        for (id, outcome, status) in [
            (
                4,
                phone::SendOutcome::Sent(message("sent", 400, true, false)),
                "sent",
            ),
            (
                5,
                phone::SendOutcome::Accepted(message("temporary", 500, true, false)),
                "accepted",
            ),
        ] {
            let out = sync.handle(phone::Event::Sent {
                id,
                conversation: "c".into(),
                outcome,
            });
            let proto::Out::Reply {
                rid,
                result: Ok(reply_data),
            } = &out[0]
            else {
                panic!("send reply")
            };
            assert_eq!(*rid, id);
            assert_eq!(reply_data["status"], status);
            assert_eq!(reply_data["message"]["pending"], status == "accepted");
            assert_eq!(data(&out, "message").len(), 1);
        }
        assert!(matches!(commands.try_recv().unwrap(), phone::Command::Fetch(ids) if ids == ["c"]));
    }

    #[test]
    fn history_and_conversations_page_from_store() {
        let (mut sync, _) = sync();
        assert_eq!(sync.history("c").messages.len(), 1);
        assert!(!sync.history("c").truncated);
        assert_eq!(sync.conversations(proto::Folder::Inbox, 0, 200).total, 1);
        assert!(
            sync.conversations(proto::Folder::Inbox, 1, 200)
                .items
                .is_empty()
        );
    }

    #[test]
    fn all_conversation_paths_keep_color_and_avatar() {
        let (mut sync, _) = sync();
        sync.handle(phone::Event::Conversation {
            conversation: pictured("c"),
        });
        sync.avatars
            .insert("c:person".into(), "/picture.png".into());
        let page = sync.conversations(proto::Folder::Inbox, 0, 200);
        assert_eq!(page.items[0].color, "#aabbcc");
        assert_eq!(page.items[0].avatar.as_deref(), Some("/picture.png"));
        let live = sync.handle(phone::Event::Conversation {
            conversation: pictured("c"),
        });
        assert_eq!(
            data(&live, "conversation")[0]["conversation"]["avatar"],
            "/picture.png"
        );
        let bulk = sync.handle(phone::Event::Ready {
            conversations: vec![pictured("c")],
            since: 0,
            partial: true,
        });
        assert_eq!(
            data(&bulk, "conversation")[0]["conversation"]["color"],
            "#aabbcc"
        );
        assert_eq!(
            data(&bulk, "conversation")[0]["conversation"]["avatar"],
            "/picture.png"
        );
        let partial = sync.handle(phone::Event::Conversation {
            conversation: conversation(false),
        });
        assert_eq!(
            data(&partial, "conversation")[0]["conversation"]["color"],
            "#aabbcc"
        );
        assert_eq!(
            data(&partial, "conversation")[0]["conversation"]["avatar"],
            "/picture.png"
        );
        let gone = sync.handle(phone::Event::Gone {
            conversation: "c".into(),
        });
        assert_eq!(
            data(&gone, "conversation")[0]["conversation"]["color"],
            "#aabbcc"
        );
        assert_eq!(
            data(&gone, "conversation")[0]["conversation"]["avatar"],
            "/picture.png"
        );
    }

    #[test]
    fn queue_skips_existing_ids_and_batches_with_fresh_tokens() {
        let (mut sync, mut commands) = sync();
        sync.ready = true;
        sync.enqueue("duplicate");
        sync.enqueue("duplicate");
        sync.tried.insert("tried".into());
        sync.enqueue("tried");
        sync.avatars.insert("saved".into(), "/saved".into());
        sync.enqueue("saved");
        for i in 0..50 {
            sync.enqueue(&format!("id{i}"));
        }
        sync.pump();
        let phone::Command::Avatars { id: first, ids } = commands.try_recv().unwrap() else {
            panic!("first batch")
        };
        assert_eq!(ids.len(), 50);
        sync.enqueue("duplicate");
        sync.pump();
        assert!(commands.try_recv().is_err());
        assert_eq!(sync.queue.len(), 1);
        assert!(data(&sync.avatar_result(first, Vec::new(), None), "avatars").is_empty());
        let phone::Command::Avatars { id: second, ids } = commands.try_recv().unwrap() else {
            panic!("second batch")
        };
        assert_ne!(first, second);
        assert_eq!(ids, ["id49"]);
    }

    #[test]
    fn ready_does_not_queue_bulk_twice_and_live_queues_once() {
        let (mut sync, mut commands) = sync();
        let ready = || phone::Event::Ready {
            conversations: vec![pictured("c")],
            since: 0,
            partial: false,
        };
        sync.handle(ready());
        assert!(
            !std::iter::from_fn(|| commands.try_recv().ok())
                .any(|c| matches!(c, phone::Command::Avatars { .. }))
        );
        sync.handle(ready());
        assert!(
            !std::iter::from_fn(|| commands.try_recv().ok())
                .any(|c| matches!(c, phone::Command::Avatars { .. }))
        );
        sync.handle(phone::Event::Conversation {
            conversation: pictured("c"),
        });
        assert!(
            std::iter::from_fn(|| commands.try_recv().ok())
                .any(|c| matches!(c, phone::Command::Avatars { .. }))
        );
        sync.handle(phone::Event::Conversation {
            conversation: pictured("c"),
        });
        assert!(
            !std::iter::from_fn(|| commands.try_recv().ok())
                .any(|c| matches!(c, phone::Command::Avatars { .. }))
        );
    }

    #[test]
    fn page_queues_stored_avatar_when_ready() {
        let (mut sync, mut commands) = sync();
        sync.store.save_conversations(&[pictured("c")]).unwrap();
        sync.meta
            .insert("c".into(), ("#aabbcc".into(), Some("c:person".into())));
        sync.ready = true;
        sync.conversations(proto::Folder::Inbox, 0, 200);
        assert!(
            matches!(commands.try_recv(), Ok(phone::Command::Avatars { ids, .. }) if ids == ["c:person"])
        );
    }

    #[test]
    fn avatar_results_validate_images_and_emit_once_per_batch() {
        let (mut sync, _) = sync();
        sync.ready = true;
        sync.meta
            .insert("c".into(), ("#aabbcc".into(), Some("c:good".into())));
        sync.meta
            .insert("d".into(), (String::new(), Some("c:good".into())));
        let valid = png(1, 1);
        let bad_size = png(20_000, 20_000);
        let mut huge = png(1, 1);
        huge.resize(300 * 1024, 0);
        sync.in_flight = Some((
            1,
            vec![
                "c:good".into(),
                "c:bad".into(),
                "c:huge".into(),
                "c:wide".into(),
                "c:missing".into(),
            ],
        ));
        let out = sync.avatar_result(
            1,
            vec![
                ("c:good".into(), valid.clone()),
                ("c:bad".into(), b"not an image".to_vec()),
                ("c:huge".into(), huge),
                ("c:wide".into(), bad_size),
                ("c:unrequested".into(), valid.clone()),
            ],
            None,
        );
        let events = data(&out, "avatars");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["items"].as_object().unwrap().len(), 2);
        assert_eq!(events[0]["items"]["c"], events[0]["items"]["d"]);
        assert!(
            sync.tried.is_superset(
                &[
                    "c:bad".into(),
                    "c:huge".into(),
                    "c:wide".into(),
                    "c:missing".into()
                ]
                .into_iter()
                .collect()
            )
        );
        assert!(!sync.avatars.contains_key("c:unrequested"));
        let path = PathBuf::from(sync.avatars.get("c:good").unwrap());
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&sync.avatar_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            format!("{:x}.png", Sha256::digest(&valid))
        );
        sync.in_flight = Some((2, vec!["c:again".into()]));
        let second = sync.avatar_result(2, vec![("c:again".into(), valid)], None);
        assert!(data(&second, "avatars").is_empty());
        assert_eq!(fs::read_dir(&sync.avatar_dir).unwrap().count(), 1);
    }

    #[test]
    fn failed_batches_retry_once_and_stale_results_are_dropped() {
        let (mut sync, mut commands) = sync();
        sync.ready = true;
        sync.enqueue("c:one");
        sync.pump();
        let phone::Command::Avatars { id: first, .. } = commands.try_recv().unwrap() else {
            panic!("first batch")
        };
        assert!(
            sync.avatar_result(first, Vec::new(), Some("offline".into()))
                .is_empty()
        );
        let phone::Command::Avatars { id: second, .. } = commands.try_recv().unwrap() else {
            panic!("retry")
        };
        assert_ne!(first, second);
        sync.avatar_result(second, Vec::new(), Some("offline".into()));
        assert!(sync.tried.contains("c:one"));
        assert!(commands.try_recv().is_err());
        sync.enqueue("c:later");
        sync.pump();
        let phone::Command::Avatars { id: third, .. } = commands.try_recv().unwrap() else {
            panic!("third batch")
        };
        sync.handle(phone::Event::Connected);
        assert!(!sync.ready);
        assert_eq!(sync.queue.front().map(String::as_str), Some("c:later"));
        assert!(
            sync.handle(phone::Event::Avatars {
                id: third,
                images: vec![("c:later".into(), png(1, 1))],
                error: None
            })
            .is_empty()
        );
        assert!(!sync.avatars.contains_key("c:later"));
    }

    #[test]
    fn pairing_resets_avatar_state_and_clears_files() {
        let (mut sync, _) = sync();
        for event in [
            phone::Event::Paired,
            phone::Event::Unpaired {
                reason: String::new(),
            },
            phone::Event::Paired,
        ] {
            fs::write(sync.avatar_dir.join("old"), b"old").unwrap();
            sync.ready = true;
            sync.avatars.insert("old".into(), "/old".into());
            sync.queue.push_back("queued".into());
            sync.tried.insert("tried".into());
            sync.failed_once.insert("failed".into());
            sync.in_flight = Some((4, vec!["inflight".into()]));
            sync.handle(event);
            assert!(!sync.ready);
            assert!(sync.in_flight.is_none());
            assert!(
                sync.avatars.is_empty()
                    && sync.queue.is_empty()
                    && sync.tried.is_empty()
                    && sync.failed_once.is_empty()
            );
            assert_eq!(fs::read_dir(&sync.avatar_dir).unwrap().count(), 0);
        }
    }
}
