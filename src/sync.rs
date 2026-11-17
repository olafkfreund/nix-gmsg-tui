//! Keeps the encrypted store, phone events and wire events in step.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use qrcode::QrCode;
use qrcode::render::svg;
use serde_json::to_value;
use sha2::{Digest, Sha256};

use crate::{phone, proto, store};

const PREVIEW_BUDGET: u64 = 64 * 1024 * 1024;

struct PreviewJob {
    key: (String, u32),
    token: u64,
    thumb_id: String,
    thumb_key: Vec<u8>,
    inline: Vec<u8>,
    queued_at: Instant,
}

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
    media_dir: PathBuf,
    downloads: Option<PathBuf>,
    waiters: HashMap<u64, Waiter>,
    queues: HashMap<String, VecDeque<u32>>,
    downloads_in_flight: HashMap<u64, (String, u32)>,
    previews: HashMap<(String, u32), PathBuf>,
    preview_jobs: VecDeque<PreviewJob>,
    preview_running: HashMap<u64, PreviewJob>,
    preview_waiters: HashMap<(String, u32), Vec<u64>>,
    reacts: HashSet<u64>,
    sends: HashSet<u64>,
    uploading: Option<u64>,
    actions: HashMap<u64, Action>,
    forgotten_actions: HashSet<u64>,
    contacts_cache: Vec<proto::ContactDto>,
    deleted_messages: HashSet<String>,
}

enum Action {
    Contacts {
        offset: usize,
        limit: usize,
    },
    StartChat,
    Archive(String),
    Restore(String),
    DeleteChat(String),
    DeleteMessage {
        conversation: String,
        message: String,
    },
}

struct Waiter {
    message: String,
    index: u32,
    save: bool,
    ms: i64,
}

impl Sync {
    pub fn new(
        store: Rc<store::Store>,
        commands: phone::Commands,
        avatar_dir: PathBuf,
        media_dir: PathBuf,
        downloads: Option<PathBuf>,
    ) -> Self {
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
            media_dir,
            downloads,
            waiters: HashMap::new(),
            queues: HashMap::new(),
            downloads_in_flight: HashMap::new(),
            previews: HashMap::new(),
            preview_jobs: VecDeque::new(),
            preview_running: HashMap::new(),
            preview_waiters: HashMap::new(),
            reacts: HashSet::new(),
            sends: HashSet::new(),
            uploading: None,
            actions: HashMap::new(),
            forgotten_actions: HashSet::new(),
            contacts_cache: Vec::new(),
            deleted_messages: HashSet::new(),
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
            SendOutcome::Attachment(message) => {
                return vec![proto::Out::Reply {
                    rid: id,
                    result: Err(proto::ErrorBody {
                        code: proto::ErrorCode::Attachment,
                        message,
                    }),
                }];
            }
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

    fn reply(rid: u64, code: proto::ErrorCode, message: &'static str) -> proto::Out {
        proto::Out::Reply {
            rid,
            result: Err(proto::ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }

    fn start_download(&mut self, message: &str, index: u32) -> bool {
        let medium = self.store.message(message).ok().flatten().and_then(|m| {
            m.media
                .into_iter()
                .filter(|v| serde_json::from_value::<phone::Medium>(v.clone()).is_ok())
                .nth(index as usize)
        });
        if let Some(medium) = medium {
            self.next_token += 1;
            let id = self.next_token;
            self.downloads_in_flight.insert(id, (message.into(), index));
            self.commands.send(phone::Command::Download {
                id,
                message: message.into(),
                media: vec![medium],
            });
            true
        } else {
            false
        }
    }

    fn preview_reply(rid: u64, path: &Path) -> proto::Out {
        proto::Out::Reply {
            rid,
            result: Ok(to_value(proto::MediaData {
                path: path.to_string_lossy().into_owned(),
            })
            .expect("media data serializes")),
        }
    }

    pub fn preview(&mut self, rid: u64, message: &str, index: u32) -> Vec<proto::Out> {
        let medium = self
            .store
            .message(message)
            .ok()
            .flatten()
            .and_then(|stored| {
                stored
                    .media
                    .into_iter()
                    .filter_map(|v| serde_json::from_value::<phone::Medium>(v).ok())
                    .nth(index as usize)
            });
        let Some(medium) = medium.filter(|m| {
            m.mime.starts_with("image/") && (!m.thumb_id.is_empty() || !m.inline.is_empty())
        }) else {
            return vec![Self::reply(rid, proto::ErrorCode::NotFound, "no preview")];
        };
        let key = (message.to_owned(), index);
        if let Some(path) = self.previews.get(&key) {
            if path.exists() {
                return vec![Self::preview_reply(rid, path)];
            }
            self.previews.remove(&key);
        }
        if medium.thumb_id.is_empty() {
            return vec![
                match self.accept_preview(&key, &medium.inline, PREVIEW_BUDGET) {
                    Some(path) => Self::preview_reply(rid, &path),
                    None => Self::reply(rid, proto::ErrorCode::NotFound, "no preview"),
                },
            ];
        }
        if !self.ready {
            return vec![Self::reply(
                rid,
                proto::ErrorCode::PhoneOffline,
                "phone offline",
            )];
        }
        if self.preview_jobs.iter().any(|j| j.key == key)
            || self.preview_running.values().any(|j| j.key == key)
        {
            self.preview_waiters.entry(key).or_default().push(rid);
            return Vec::new();
        }
        if self.preview_jobs.len() >= 32 {
            return vec![Self::reply(rid, proto::ErrorCode::Busy, "queue full")];
        }
        self.preview_waiters.insert(key.clone(), vec![rid]);
        self.next_token += 1;
        self.preview_jobs.push_back(PreviewJob {
            key,
            token: self.next_token,
            thumb_id: medium.thumb_id,
            thumb_key: medium.thumb_key,
            inline: medium.inline,
            queued_at: Instant::now(),
        });
        self.pump_previews(Instant::now())
    }

    pub fn pump_previews(&mut self, now: Instant) -> Vec<proto::Out> {
        self.preview_jobs.retain(|j| {
            self.preview_waiters
                .get(&j.key)
                .is_some_and(|w| !w.is_empty())
        });
        let mut out = Vec::new();
        self.preview_jobs.retain(|j| {
            if now.saturating_duration_since(j.queued_at) > Duration::from_secs(25) {
                for rid in self.preview_waiters.remove(&j.key).unwrap_or_default() {
                    out.push(Self::reply(rid, proto::ErrorCode::Busy, "expired"));
                }
                false
            } else {
                true
            }
        });
        while self.preview_running.len() < 2 {
            let Some(job) = self.preview_jobs.pop_front() else {
                break;
            };
            if self.commands.send(phone::Command::Preview {
                id: job.token,
                thumb_id: job.thumb_id.clone(),
                thumb_key: job.thumb_key.clone(),
            }) {
                self.preview_running.insert(job.token, job);
            } else {
                for rid in self.preview_waiters.remove(&job.key).unwrap_or_default() {
                    out.push(Self::reply(
                        rid,
                        proto::ErrorCode::PhoneOffline,
                        "phone offline",
                    ));
                }
            }
        }
        out
    }

    fn accept_preview(
        &mut self,
        key: &(String, u32),
        bytes: &[u8],
        budget: u64,
    ) -> Option<PathBuf> {
        let ext = image_kind(bytes)?;
        if bytes.len() > phone::PREVIEW_MAX {
            return None;
        }
        let size = imagesize::blob_size(bytes).ok()?;
        let (w, h) = (size.width as u64, size.height as u64);
        if w == 0 || h == 0 || w > 16384 || h > 16384 || w * h > 40_000_000 {
            return None;
        }
        let dir = self.media_dir.join("previews");
        if let Err(error) = crate::paths::create_private_dir(&dir) {
            tracing::debug!(%error, "cannot create preview directory");
            return None;
        }
        let mut hash = Sha256::new();
        hash.update(key.0.as_bytes());
        hash.update([0]);
        hash.update(key.1.to_be_bytes());
        hash.update(b"preview");
        let path = dir.join(format!("{:x}.{ext}", hash.finalize()));
        if let Err(error) = write_media(&path, bytes) {
            tracing::debug!(%error, "cannot write preview");
            return None;
        }
        self.previews.insert(key.clone(), path.clone());
        self.evict_previews(&path, budget);
        Some(path)
    }

    fn evict_previews(&mut self, keep: &Path, budget: u64) {
        let Ok(entries) = fs::read_dir(self.media_dir.join("previews")) else {
            return;
        };
        let mut files = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().ends_with(".tmp") {
                continue;
            }
            if let Ok(meta) = entry.metadata()
                && meta.is_file()
            {
                files.push((
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                    name,
                    entry.path(),
                    meta.len(),
                ));
            }
        }
        files.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
        let mut total: u64 = files.iter().map(|f| f.3).sum();
        for (_, _, path, len) in files {
            if total <= budget {
                break;
            }
            if path == keep {
                continue;
            }
            match fs::remove_file(&path) {
                Ok(()) => {
                    total -= len;
                    self.previews.retain(|_, cached| cached != &path);
                }
                Err(error) => tracing::debug!(%error, "cannot evict preview"),
            }
        }
        if total > budget {
            tracing::warn!("preview cache over budget");
        }
    }

    pub fn cancel_preview(&mut self, message: &str, index: u32, rids: &[u64]) -> Vec<proto::Out> {
        let key = (message.to_owned(), index);
        let mut out = Vec::new();
        if let Some(waiters) = self.preview_waiters.get_mut(&key) {
            waiters.retain(|rid| {
                if rids.contains(rid) {
                    out.push(Self::reply(*rid, proto::ErrorCode::Busy, "cancelled"));
                    false
                } else {
                    true
                }
            });
            if waiters.is_empty() {
                self.preview_waiters.remove(&key);
                self.preview_jobs.retain(|j| j.key != key);
            }
        }
        out
    }

    pub fn media(&mut self, rid: u64, message: &str, index: u32, save: bool) -> Vec<proto::Out> {
        let Ok(Some(stored)) = self.store.message(message) else {
            return vec![Self::reply(
                rid,
                proto::ErrorCode::NotFound,
                "message not found",
            )];
        };
        if stored
            .media
            .iter()
            .filter(|v| serde_json::from_value::<phone::Medium>((*v).clone()).is_ok())
            .nth(index as usize)
            .is_none()
        {
            return vec![Self::reply(
                rid,
                proto::ErrorCode::NotFound,
                "no such attachment",
            )];
        }
        let waiter = Waiter {
            message: message.into(),
            index,
            save,
            ms: stored.timestamp_ms,
        };
        match self.store.attachment(message, index) {
            Ok(Some(attachment)) => return vec![self.finish(rid, &waiter, &attachment, true)],
            Err(error) => tracing::warn!(%error, "cannot read attachment"),
            Ok(None) => {}
        }
        if !self.ready {
            return vec![Self::reply(
                rid,
                proto::ErrorCode::PhoneOffline,
                "phone did not send it",
            )];
        }
        self.waiters.insert(rid, waiter);
        let queue = self.queues.entry(message.into()).or_default();
        if !queue.contains(&index) {
            let start = queue.is_empty();
            queue.push_back(index);
            if start && !self.start_download(message, index) {
                self.waiters.remove(&rid);
                self.queues.remove(message);
                return vec![Self::reply(
                    rid,
                    proto::ErrorCode::NotFound,
                    "no such attachment",
                )];
            }
        }
        Vec::new()
    }

    fn finish(&self, rid: u64, waiter: &Waiter, a: &store::Attachment, full: bool) -> proto::Out {
        if a.bytes.len() > 100 * 1024 * 1024 {
            return Self::reply(rid, proto::ErrorCode::Internal, "too large");
        }
        let ext = extension(&a.content_type);
        let mut hash = Sha256::new();
        hash.update(waiter.message.as_bytes());
        hash.update([0]);
        hash.update(waiter.index.to_be_bytes());
        let suffix = if full { "" } else { "-preview" };
        let path = self
            .media_dir
            .join(format!("{:x}{suffix}.{ext}", hash.finalize()));
        let result = if waiter.save {
            self.downloads
                .as_ref()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no Downloads folder"))
                .and_then(|downloads| save_download(downloads, a, waiter.ms, ext))
        } else {
            write_media(&path, &a.bytes).map(|()| path)
        };
        match result {
            Ok(path) => proto::Out::Reply {
                rid,
                result: Ok(to_value(proto::MediaData {
                    path: path.to_string_lossy().into_owned(),
                })
                .expect("media data serializes")),
            },
            Err(error) => {
                tracing::debug!(%error, "cannot write media file");
                let message = match error.to_string().as_str() {
                    "no Downloads folder" => "no Downloads folder",
                    "no free file name" => "no free file name",
                    _ => "cannot write the file",
                };
                Self::reply(rid, proto::ErrorCode::Internal, message)
            }
        }
    }

    fn downloaded(
        &mut self,
        id: u64,
        attachments: Vec<phone::Attachment>,
        error: Option<String>,
    ) -> Vec<proto::Out> {
        let Some((message, index)) = self.downloads_in_flight.remove(&id) else {
            return Vec::new();
        };
        if let Some(error) = &error {
            tracing::debug!(%error, "phone download failed");
        }
        let oversized = attachments
            .first()
            .is_some_and(|a| a.data.len() > 100 * 1024 * 1024)
            || error.as_deref() == Some("too large");
        let attachment = attachments.into_iter().next().filter(|_| !oversized);
        let attachment = attachment.map(|a| {
            let stored = store::Attachment {
                content_type: a.content_type,
                file_name: a.name,
                bytes: a.data,
                width: a.width,
                height: a.height,
            };
            if a.full
                && matches!(self.store.message(&message), Ok(Some(_)))
                && let Err(error) = self.store.save_attachment(&message, index, &stored)
            {
                tracing::warn!(%error, "cannot cache attachment");
            }
            (stored, a.full)
        });
        let mut ids: Vec<_> = self
            .waiters
            .iter()
            .filter(|(_, w)| w.message == message && w.index == index)
            .map(|(&rid, _)| rid)
            .collect();
        ids.sort_unstable();
        let mut out = Vec::new();
        for rid in ids {
            let waiter = self.waiters.remove(&rid).expect("waiter exists");
            out.push(match &attachment {
                Some((a, full)) => self.finish(rid, &waiter, a, *full),
                None if oversized => Self::reply(rid, proto::ErrorCode::Internal, "too large"),
                None => Self::reply(rid, proto::ErrorCode::PhoneOffline, "phone did not send it"),
            });
        }
        if let Some(queue) = self.queues.get_mut(&message) {
            queue.pop_front();
        }
        while let Some(next) = self.queues.get(&message).and_then(|q| q.front().copied()) {
            if self.start_download(&message, next) {
                break;
            }
            self.queues.get_mut(&message).unwrap().pop_front();
            let ids: Vec<_> = self
                .waiters
                .iter()
                .filter(|(_, w)| w.message == message && w.index == next)
                .map(|(&rid, _)| rid)
                .collect();
            for rid in ids {
                self.waiters.remove(&rid);
                out.push(Self::reply(
                    rid,
                    proto::ErrorCode::NotFound,
                    "no such attachment",
                ));
            }
        }
        if self.queues.get(&message).is_some_and(VecDeque::is_empty) {
            self.queues.remove(&message);
        }
        out
    }

    pub fn forget(&mut self, rid: u64) {
        self.waiters.remove(&rid);
        for waiters in self.preview_waiters.values_mut() {
            waiters.retain(|&waiting| waiting != rid);
        }
        self.reacts.remove(&rid);
        // Timed out or disconnected: no one waits for it, so it no longer holds off unpair.
        self.sends.remove(&rid);
        // Keep uploading: the task still holds the files in memory until its Sent event.
        if self.actions.contains_key(&rid) {
            self.forgotten_actions.insert(rid);
        }
    }

    pub fn busy(&self) -> bool {
        !self.sends.is_empty()
            || self
                .actions
                .keys()
                .any(|rid| !self.forgotten_actions.contains(rid))
    }

    pub fn contacts(&mut self, rid: u64, p: proto::ContactsParams) -> Vec<proto::Out> {
        if p.offset == 0 || self.contacts_cache.is_empty() {
            self.contacts_cache.clear();
            self.actions.insert(
                rid,
                Action::Contacts {
                    offset: p.offset,
                    limit: p.limit,
                },
            );
            self.commands.send(phone::Command::Contacts { id: rid });
            Vec::new()
        } else {
            vec![Self::ok(rid, &self.contacts_page(p.offset, p.limit))]
        }
    }

    fn contacts_page(&self, offset: usize, limit: usize) -> proto::ContactsData {
        proto::ContactsData {
            items: self
                .contacts_cache
                .iter()
                .skip(offset)
                .take(limit)
                .cloned()
                .collect(),
            total: self.contacts_cache.len(),
        }
    }

    pub fn start_chat(&mut self, rid: u64, p: proto::StartChatParams) {
        self.actions.insert(rid, Action::StartChat);
        self.commands.send(phone::Command::StartChat {
            id: rid,
            numbers: p.numbers,
        });
    }

    pub fn archive(&mut self, rid: u64, p: proto::ConversationParams) {
        self.actions
            .insert(rid, Action::Archive(p.conversation.clone()));
        self.commands.send(phone::Command::Archive {
            id: rid,
            conversation: p.conversation,
        });
    }

    pub fn restore(&mut self, rid: u64, p: proto::ConversationParams) {
        self.actions
            .insert(rid, Action::Restore(p.conversation.clone()));
        self.commands.send(phone::Command::Restore {
            id: rid,
            conversation: p.conversation,
        });
    }

    pub fn delete_chat(&mut self, rid: u64, p: proto::ConversationParams) {
        self.actions
            .insert(rid, Action::DeleteChat(p.conversation.clone()));
        self.commands.send(phone::Command::DeleteChat {
            id: rid,
            conversation: p.conversation,
            for_good: true,
        });
    }

    pub fn delete_message(&mut self, rid: u64, p: proto::DeleteMessageParams) {
        self.actions.insert(
            rid,
            Action::DeleteMessage {
                conversation: p.conversation,
                message: p.message.clone(),
            },
        );
        self.commands.send(phone::Command::DeleteMessage {
            id: rid,
            message: p.message,
        });
    }

    fn ok(rid: u64, data: &impl serde::Serialize) -> proto::Out {
        proto::Out::Reply {
            rid,
            result: Ok(to_value(data).expect("reply data serializes")),
        }
    }

    pub fn react(&mut self, rid: u64, p: proto::ReactParams) {
        self.reacts.insert(rid);
        self.commands.send(phone::Command::React {
            id: rid,
            conversation: p.conversation,
            message: p.message,
            emoji: p.emoji,
            op: p.op.into(),
        });
    }

    fn reset_media(&mut self) -> Vec<proto::Out> {
        if let Err(error) = crate::serve::clear_media(&self.media_dir) {
            tracing::warn!(%error, "cannot clear media directory");
        }
        let ids: Vec<_> = self.waiters.keys().copied().collect();
        self.waiters.clear();
        self.queues.clear();
        self.downloads_in_flight.clear();
        self.previews.clear();
        self.preview_jobs.clear();
        self.preview_running.clear();
        let preview_rids: Vec<_> = self
            .preview_waiters
            .drain()
            .flat_map(|(_, rids)| rids)
            .collect();
        let reacts: Vec<_> = self.reacts.drain().collect();
        let actions: Vec<_> = self.actions.drain().map(|(rid, _)| rid).collect();
        let forgotten = std::mem::take(&mut self.forgotten_actions);
        self.contacts_cache.clear();
        self.deleted_messages.clear();
        let mut out: Vec<_> = ids
            .into_iter()
            .chain(reacts)
            .chain(actions.into_iter().filter(|rid| !forgotten.contains(rid)))
            .map(|rid| Self::reply(rid, proto::ErrorCode::NotPaired, "phone not paired"))
            .collect();
        out.extend(
            preview_rids
                .into_iter()
                .map(|rid| Self::reply(rid, proto::ErrorCode::Busy, "reset")),
        );
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
                let mut out = self.reset_media();
                out.push(Self::state(proto::State::Connecting, None));
                out
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
                if error.is_some() {
                    tracing::warn!("cannot fetch history");
                } else {
                    let messages: Vec<_> = messages
                        .into_iter()
                        .filter(|message| !self.deleted_messages.contains(&message.id))
                        .collect();
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
            } => {
                if self.deleted_messages.contains(&message.id) {
                    Vec::new()
                } else {
                    self.live(conversation, message, old)
                }
            }
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
                    .conversations_hidden(std::slice::from_ref(&conversation))
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
                out.push(Self::event(
                    "gone",
                    &proto::GoneEvent {
                        conversation,
                        message: None,
                    },
                ));
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
            } => {
                self.sends.remove(&id);
                if self.uploading == Some(id) {
                    self.uploading = None;
                }
                self.sent(id, conversation, outcome)
            }
            phone::Event::Expired { message } => {
                self.lose_ready();
                vec![Self::state(proto::State::Expired, Some(message))]
            }
            phone::Event::Unpaired { .. } => {
                self.lose_ready();
                self.reset_avatars();
                let mut out = self.reset_media();
                out.push(Self::state(
                    proto::State::Login,
                    Some("unpaired_remotely".into()),
                ));
                out
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
            phone::Event::Downloaded {
                id,
                attachments,
                error,
            } => self.downloaded(id, attachments, error),
            phone::Event::Previewed { id, result } => {
                let Some(job) = self.preview_running.remove(&id) else {
                    return Vec::new();
                };
                let path = result
                    .as_ref()
                    .ok()
                    .and_then(|bytes| self.accept_preview(&job.key, bytes, PREVIEW_BUDGET))
                    .or_else(|| {
                        (!job.inline.is_empty())
                            .then(|| self.accept_preview(&job.key, &job.inline, PREVIEW_BUDGET))
                            .flatten()
                    });
                let mut out: Vec<_> = self
                    .preview_waiters
                    .remove(&job.key)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|rid| match &path {
                        Some(path) => Self::preview_reply(rid, path),
                        None if result.is_err() => Self::reply(
                            rid,
                            proto::ErrorCode::PhoneOffline,
                            "preview download failed",
                        ),
                        None => Self::reply(rid, proto::ErrorCode::NotFound, "no preview"),
                    })
                    .collect();
                out.extend(self.pump_previews(Instant::now()));
                out
            }
            phone::Event::Done { id, error } if self.reacts.remove(&id) => {
                if let Some(error) = error {
                    tracing::debug!(%error, "phone reaction failed");
                    vec![Self::reply(
                        id,
                        proto::ErrorCode::PhoneOffline,
                        "phone refused the reaction",
                    )]
                } else {
                    vec![proto::Out::Reply {
                        rid: id,
                        result: Ok(to_value(serde_json::json!({})).unwrap()),
                    }]
                }
            }
            phone::Event::Done { id, error } => {
                let Some(action) = self.actions.remove(&id) else {
                    return Vec::new();
                };
                let forgotten = self.forgotten_actions.remove(&id);
                if error.is_some() {
                    eprintln!("phone action failed");
                    return if forgotten {
                        Vec::new()
                    } else {
                        vec![Self::reply(
                            id,
                            proto::ErrorCode::PhoneOffline,
                            "the phone refused",
                        )]
                    };
                }
                let mut out = match action {
                    Action::Archive(conversation) | Action::Restore(conversation) => {
                        self.commands
                            .send(phone::Command::Lookup(vec![conversation]));
                        Vec::new()
                    }
                    Action::DeleteChat(conversation) => {
                        self.handle(phone::Event::Gone { conversation })
                    }
                    Action::DeleteMessage {
                        conversation,
                        message,
                    } => {
                        self.deleted_messages.insert(message.clone());
                        if self.store.forget_message(&message).is_err() {
                            eprintln!("cannot forget message");
                        }
                        vec![Self::event(
                            "gone",
                            &proto::GoneEvent {
                                conversation,
                                message: Some(message),
                            },
                        )]
                    }
                    _ => Vec::new(),
                };
                if !forgotten {
                    out.push(Self::ok(id, &serde_json::json!({})));
                }
                out
            }
            phone::Event::Contacts {
                id,
                contacts,
                error,
            } => {
                let Some(Action::Contacts { offset, limit }) = self.actions.remove(&id) else {
                    return Vec::new();
                };
                let forgotten = self.forgotten_actions.remove(&id);
                if error.is_some() {
                    eprintln!("phone contacts failed");
                    return if forgotten {
                        Vec::new()
                    } else {
                        vec![Self::reply(
                            id,
                            proto::ErrorCode::PhoneOffline,
                            "the phone refused",
                        )]
                    };
                }
                self.contacts_cache = contacts
                    .into_iter()
                    .map(|c| proto::ContactDto {
                        name: c.name,
                        number: c.number,
                    })
                    .collect();
                if forgotten {
                    Vec::new()
                } else {
                    vec![Self::ok(id, &self.contacts_page(offset, limit))]
                }
            }
            phone::Event::Started {
                id,
                conversation,
                error,
            } => {
                if !matches!(self.actions.remove(&id), Some(Action::StartChat)) {
                    return Vec::new();
                }
                let forgotten = self.forgotten_actions.remove(&id);
                if error.is_some() || conversation.is_none() {
                    eprintln!("phone start chat failed");
                    return if forgotten {
                        Vec::new()
                    } else {
                        vec![Self::reply(
                            id,
                            proto::ErrorCode::PhoneOffline,
                            "the phone refused",
                        )]
                    };
                }
                let conversation = conversation.expect("checked above");
                let name = conversation.id.clone();
                let mut out = self.handle(phone::Event::Conversation { conversation });
                if self
                    .store
                    .messages(&name)
                    .map_or(true, |messages| messages.is_empty())
                    || self.store.changed().is_ok_and(|ids| ids.contains(&name))
                {
                    self.fetch(vec![name.clone()]);
                }
                if !forgotten {
                    out.push(Self::ok(id, &proto::StartChatData { conversation: name }));
                }
                out
            }
        }
    }

    pub fn send(
        &mut self,
        rid: u64,
        conversation: String,
        text: String,
        reply_to: Option<String>,
        files: Vec<PathBuf>,
    ) -> bool {
        if !files.is_empty() && self.uploading.is_some() {
            return false;
        }
        if !files.is_empty() {
            self.uploading = Some(rid);
        }
        self.sends.insert(rid);
        self.commands.send(phone::Command::Send {
            id: rid,
            conversation,
            text,
            reply: reply_to,
            files,
        });
        true
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
    let Some(ext) = image_kind(image).filter(|&kind| kind != "gif") else {
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

fn image_kind(image: &[u8]) -> Option<&'static str> {
    let ext = if image.starts_with(&[0xff, 0xd8, 0xff]) {
        "jpg"
    } else if image.starts_with(&[0x89, b'P', b'N', b'G', 13, 10, 26, 10]) {
        "png"
    } else if image.starts_with(b"RIFF") && image.get(8..12) == Some(b"WEBP".as_slice()) {
        "webp"
    } else if image.starts_with(b"GIF87a") || image.starts_with(b"GIF89a") {
        "gif"
    } else {
        return None;
    };
    Some(ext)
}

fn extension(mime: &str) -> &'static str {
    match mime
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "video/mp4" => "mp4",
        "video/3gpp" => "3gp",
        "audio/mpeg" => "mp3",
        "audio/ogg" => "ogg",
        "application/pdf" => "pdf",
        "text/x-vcard" | "text/vcard" => "vcf",
        _ => "bin",
    }
}

/// Writes `bytes` to `path` through a private temp file, so no reader sees a partial file.
fn write_media(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if path.exists() {
        return Ok(());
    }
    let mut random = [0u8; 8];
    getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
    let temp = path.with_extension(format!("{:016x}.tmp", u64::from_be_bytes(random)));
    let result = (|| {
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)?
            .write_all(bytes)?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

fn save_download(dir: &Path, a: &store::Attachment, ms: i64, ext: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let raw = a
        .file_name
        .as_deref()
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("");
    let mut name: String = raw
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
        })
        .collect();
    if name.len() > 200 {
        // Cut the stem, keep a plausible extension.
        let (stem, dot_ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() && e.len() <= 20 => (s, &name[s.len()..]),
            _ => (name.as_str(), ""),
        };
        let mut end = 200 - dot_ext.len();
        while !stem.is_char_boundary(end) {
            end -= 1;
        }
        name = format!("{}{dot_ext}", &stem[..end]);
    }
    if name.is_empty() || name == "." || name == ".." || name.starts_with('.') {
        name = format!("message-{ms}.{ext}");
    } else if Path::new(&name).extension().is_none_or(|e| e.is_empty()) {
        name.push('.');
        name.push_str(ext);
    }
    let (stem, suffix) = name.rsplit_once('.').unwrap_or((&name, ""));
    let mut temp = None;
    for _ in 0..10 {
        let mut random = [0u8; 8];
        getrandom::fill(&mut random).map_err(|e| io::Error::other(e.to_string()))?;
        let path = dir.join(format!(
            ".gmessages-{:016x}.part",
            u64::from_be_bytes(random)
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => {
                temp = Some((path, file));
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let (temp_path, mut file) = temp.ok_or_else(|| io::Error::other("cannot write the file"))?;
    let result = (|| -> io::Result<PathBuf> {
        file.write_all(&a.bytes)?;
        drop(file);
        for n in 1..=99 {
            let candidate = if n == 1 {
                name.clone()
            } else {
                format!("{stem} ({n}).{suffix}")
            };
            let path = dir.join(candidate);
            match fs::hard_link(&temp_path, &path) {
                Ok(()) => return Ok(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::other("no free file name"))
    })();
    let _ = fs::remove_file(temp_path);
    result
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

    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 13, 0x49, 0x48, 0x44, 0x52, 0, 0,
        0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 0x1f, 0x15, 0xc4, 0x89, 0, 0, 0, 10, 0x49, 0x44, 0x41,
        0x54, 0x78, 0x9c, 0x63, 0, 1, 0, 0, 5, 0, 1, 0xa5, 0xf6, 0x45, 0x40, 0, 0, 0, 0, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    fn preview_medium(id: &str, inline: &[u8]) -> Value {
        serde_json::to_value(phone::Medium {
            mime: "image/png".into(),
            thumb_id: id.into(),
            inline: inline.to_vec(),
            ..Default::default()
        })
        .unwrap()
    }

    fn with_preview_messages(sync: &Sync, count: usize) {
        let messages: Vec<_> = (0..count)
            .map(|n| {
                let mut m = message(&format!("preview-{n}"), n as i64 + 1, false, false);
                m.media = vec![preview_medium("thumb", &[])];
                m
            })
            .collect();
        sync.store.save_messages("c", &messages).unwrap();
    }

    #[test]
    fn preview_image_checks_and_protocol_metadata() {
        let (mut sync, _) = sync();
        let key = ("image".into(), 0);
        assert_eq!(image_kind(TINY_PNG), Some("png"));
        assert!(
            sync.accept_preview(&key, TINY_PNG, PREVIEW_BUDGET)
                .unwrap()
                .extension()
                .unwrap()
                == "png"
        );
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0, 4, 0, 0, 0xff, 0xc0, 0, 11, 8, 0, 6, 0, 8, 1, 1, 0x11, 0,
        ];
        assert_eq!(image_kind(&jpeg), Some("jpg"));
        assert_eq!(
            sync.accept_preview(&("jpeg".into(), 0), &jpeg, PREVIEW_BUDGET)
                .unwrap()
                .extension()
                .unwrap(),
            "jpg"
        );
        let webp = [
            b'R', b'I', b'F', b'F', 17, 0, 0, 0, b'W', b'E', b'B', b'P', b'V', b'P', b'8', b'L', 5,
            0, 0, 0, 0x2f, 1, 0x40, 0, 0,
        ];
        assert_eq!(image_kind(&webp), Some("webp"));
        assert_eq!(
            sync.accept_preview(&("webp".into(), 0), &webp, PREVIEW_BUDGET)
                .unwrap()
                .extension()
                .unwrap(),
            "webp"
        );
        assert_eq!(image_kind(b"GIF89a\x03\0\x02\0\0\0\0"), Some("gif"));
        let gif_size = imagesize::blob_size(b"GIF89a\x03\0\x02\0\0\0\0").unwrap();
        assert_eq!((gif_size.width, gif_size.height), (3, 2));
        assert!(
            sync.accept_preview(
                &("gif".into(), 0),
                b"GIF89a\x03\0\x02\0\0\0\0",
                PREVIEW_BUDGET
            )
            .is_some()
        );
        assert!(
            sync.accept_preview(&("junk".into(), 0), b"hello", PREVIEW_BUDGET)
                .is_none()
        );
        assert!(
            sync.accept_preview(
                &("huge".into(), 0),
                &vec![b'x'; phone::PREVIEW_MAX + 1],
                PREVIEW_BUDGET
            )
            .is_none()
        );
        for (w, h) in [(10000, 5000), (20000, 1), (0, 10)] {
            assert!(
                sync.accept_preview(&(format!("{w}-{h}"), 0), &png(w, h), PREVIEW_BUDGET)
                    .is_none()
            );
        }
        assert!(
            sync.accept_preview(&("truncated".into(), 0), &TINY_PNG[..15], PREVIEW_BUDGET)
                .is_none()
        );
        let mut m = message("meta", 1, false, false);
        m.media = vec![
            serde_json::to_value(phone::Medium {
                mime: "image/jpeg".into(),
                thumb_id: "t".into(),
                width: 640,
                height: 480,
                ..Default::default()
            })
            .unwrap(),
            serde_json::to_value(phone::Medium {
                mime: "application/pdf".into(),
                thumb_id: "t".into(),
                ..Default::default()
            })
            .unwrap(),
            serde_json::to_value(phone::Medium {
                mime: "image/png".into(),
                ..Default::default()
            })
            .unwrap(),
            serde_json::to_value(phone::Medium {
                mime: "image/png".into(),
                width: 0,
                height: 480,
                ..Default::default()
            })
            .unwrap(),
            serde_json::to_value(phone::Medium {
                mime: "image/png".into(),
                width: 20000,
                height: 10,
                ..Default::default()
            })
            .unwrap(),
        ];
        let wire = proto::Message::from_phone("c", &m, false);
        assert_eq!(
            (
                wire.attachments[0].width,
                wire.attachments[0].height,
                wire.attachments[0].preview
            ),
            (640, 480, true)
        );
        assert!(!wire.attachments[1].preview && !wire.attachments[2].preview);
        assert_eq!(
            (wire.attachments[3].width, wire.attachments[3].height),
            (0, 0)
        );
        assert_eq!(
            (wire.attachments[4].width, wire.attachments[4].height),
            (0, 0)
        );
        let empty = serde_json::to_value(&wire.attachments[2]).unwrap();
        assert!(
            empty.get("width").is_none()
                && empty.get("height").is_none()
                && empty.get("preview").is_none()
        );
        let inline = proto::Message::from_phone(
            "c",
            &phone::Message {
                media: vec![preview_medium("", TINY_PNG)],
                ..m
            },
            false,
        );
        assert!(inline.attachments[0].preview);
    }

    #[test]
    fn preview_queue_waiters_and_results() {
        let (mut sync, mut commands) = sync();
        with_preview_messages(&sync, 36);
        sync.ready = true;
        error(&sync.preview(1, "missing", 0), proto::ErrorCode::NotFound);
        error(&sync.preview(2, "preview-0", 1), proto::ErrorCode::NotFound);
        let mut pdf = message("pdf", 123, false, false);
        pdf.media = vec![
            serde_json::to_value(phone::Medium {
                mime: "application/pdf".into(),
                thumb_id: "thumb".into(),
                ..Default::default()
            })
            .unwrap(),
        ];
        sync.store.save_messages("c", &[pdf]).unwrap();
        error(&sync.preview(20, "pdf", 0), proto::ErrorCode::NotFound);
        assert!(sync.preview(3, "preview-0", 0).is_empty());
        let token = match commands.try_recv().unwrap() {
            phone::Command::Preview { id, .. } => id,
            _ => panic!("preview command"),
        };
        assert!(sync.preview(4, "preview-0", 0).is_empty());
        assert!(commands.try_recv().is_err());
        assert!(sync.preview(5, "preview-1", 0).is_empty());
        let _ = commands.try_recv().unwrap();
        assert!(sync.preview(6, "preview-2", 0).is_empty());
        assert_eq!(sync.preview_jobs.len(), 1);
        for n in 3..34 {
            assert!(
                sync.preview(100 + n as u64, &format!("preview-{n}"), 0)
                    .is_empty()
            );
        }
        error(&sync.preview(200, "preview-34", 0), proto::ErrorCode::Busy);
        assert!(!sync.preview_waiters.contains_key(&("preview-34".into(), 0)));
        let out = sync.handle(phone::Event::Previewed {
            id: token,
            result: Ok(TINY_PNG.to_vec()),
        });
        assert_eq!(path(&out, 3), path(&out[1..], 4));
        assert!(matches!(
            commands.try_recv().unwrap(),
            phone::Command::Preview { .. }
        ));
        assert!(
            sync.handle(phone::Event::Previewed {
                id: u64::MAX,
                result: Ok(vec![])
            })
            .is_empty()
        );
        let saved = path(&sync.preview(7, "preview-0", 0), 7);
        fs::remove_file(saved).unwrap();
        assert!(sync.preview(8, "preview-0", 0).is_empty());
    }

    #[test]
    fn preview_inline_fallback_cancel_expiry_and_reset() {
        let (mut sync, mut commands) = sync();
        with_preview_messages(&sync, 4);
        let mut inline = message("inline", 8, false, false);
        inline.media = vec![preview_medium("", TINY_PNG)];
        sync.store.save_messages("c", &[inline]).unwrap();
        let inline_path = path(&sync.preview(1, "inline", 0), 1);
        assert!(inline_path.starts_with(sync.media_dir.join("previews")));
        let mut wrong_mime = message("wrong-mime", 9, false, false);
        let mut medium = preview_medium("", TINY_PNG);
        medium["mime"] = json!("image/jpeg");
        wrong_mime.media = vec![medium];
        sync.store.save_messages("c", &[wrong_mime]).unwrap();
        assert_eq!(
            path(&sync.preview(11, "wrong-mime", 0), 11)
                .extension()
                .unwrap(),
            "png"
        );
        sync.ready = true;
        assert!(sync.preview(2, "preview-0", 0).is_empty());
        let token = match commands.try_recv().unwrap() {
            phone::Command::Preview { id, .. } => id,
            _ => panic!(),
        };
        assert!(sync.preview(3, "preview-0", 0).is_empty());
        error(
            &sync.cancel_preview("preview-0", 0, &[2]),
            proto::ErrorCode::Busy,
        );
        let out = sync.handle(phone::Event::Previewed {
            id: token,
            result: Ok(TINY_PNG.to_vec()),
        });
        assert_eq!(path(&out, 3).extension().unwrap(), "png");
        assert!(sync.preview(4, "preview-1", 0).is_empty());
        let token = match commands.try_recv().unwrap() {
            phone::Command::Preview { id, .. } => id,
            _ => panic!(),
        };
        error(
            &sync.handle(phone::Event::Previewed {
                id: token,
                result: Ok(b"junk".to_vec()),
            }),
            proto::ErrorCode::NotFound,
        );
        assert!(sync.preview(5, "preview-2", 0).is_empty());
        let token = match commands.try_recv().unwrap() {
            phone::Command::Preview { id, .. } => id,
            _ => panic!(),
        };
        error(
            &sync.handle(phone::Event::Previewed {
                id: token,
                result: Err("failed".into()),
            }),
            proto::ErrorCode::PhoneOffline,
        );

        let mut fallback = message("fallback", 9, false, false);
        fallback.media = vec![preview_medium("thumb", TINY_PNG)];
        sync.store.save_messages("c", &[fallback]).unwrap();
        assert!(sync.preview(6, "fallback", 0).is_empty());
        let token = match commands.try_recv().unwrap() {
            phone::Command::Preview { id, .. } => id,
            _ => panic!(),
        };
        assert_eq!(
            path(
                &sync.handle(phone::Event::Previewed {
                    id: token,
                    result: Ok(b"junk".to_vec())
                }),
                6
            )
            .extension()
            .unwrap(),
            "png"
        );

        assert!(sync.preview(7, "preview-0", 0).len() == 1); // cached
        assert!(sync.preview(8, "preview-1", 0).is_empty());
        let _ = commands.try_recv().unwrap();
        assert!(sync.preview(9, "preview-2", 0).is_empty());
        let _ = commands.try_recv().unwrap();
        assert!(sync.preview(10, "preview-3", 0).is_empty());
        let out = sync.pump_previews(Instant::now() + Duration::from_secs(26));
        error(&out, proto::ErrorCode::Busy);
        assert!(!sync.preview_waiters.contains_key(&("preview-3".into(), 0)));
        let out = sync.reset_media();
        assert_eq!(out.iter().filter(|o| matches!(o, proto::Out::Reply { result: Err(e), .. } if e.code == proto::ErrorCode::Busy)).count(), 2);
        assert!(
            sync.previews.is_empty()
                && sync.preview_jobs.is_empty()
                && sync.preview_running.is_empty()
                && sync.preview_waiters.is_empty()
        );
    }

    #[test]
    fn preview_forget_and_closed_channel() {
        let (mut sync, mut commands) = sync();
        with_preview_messages(&sync, 3);
        sync.ready = true;
        sync.preview(1, "preview-0", 0);
        sync.preview(2, "preview-1", 0);
        let _ = commands.try_recv().unwrap();
        let _ = commands.try_recv().unwrap();
        sync.preview(3, "preview-2", 0);
        sync.forget(3);
        assert!(sync.pump_previews(Instant::now()).is_empty());
        assert!(sync.preview_jobs.is_empty());
        drop(commands);
        let (mut sync, commands) = self::sync();
        with_preview_messages(&sync, 1);
        sync.ready = true;
        drop(commands);
        error(
            &sync.preview(4, "preview-0", 0),
            proto::ErrorCode::PhoneOffline,
        );
        assert!(sync.preview_running.is_empty());
    }

    #[test]
    fn preview_eviction_oldest_first_keeps_current_file() {
        use std::fs::File;
        use std::time::SystemTime;
        let (mut sync, _) = sync();
        let dir = sync.media_dir.join("previews");
        crate::paths::create_private_dir(&dir).unwrap();
        let paths: Vec<_> = ["old", "middle", "keep"]
            .into_iter()
            .map(|name| dir.join(name))
            .collect();
        for (n, path) in paths.iter().enumerate() {
            fs::write(path, b"123456").unwrap();
            File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(n as u64 + 1))
                .unwrap();
            sync.previews.insert((format!("p{n}"), 0), path.clone());
        }
        sync.evict_previews(&paths[2], 10);
        assert!(!paths[0].exists() && !paths[1].exists() && paths[2].exists());
        assert_eq!(sync.previews.len(), 1);

        fs::write(&paths[0], b"123456").unwrap();
        fs::write(&paths[1], b"123456").unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        sync.evict_previews(&paths[2], 10);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(paths.iter().all(|p| p.exists()));
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
        let media_dir = path.with_extension("media");
        let downloads = path.with_extension("downloads");
        crate::serve::clear_media(&avatar_dir).unwrap();
        crate::serve::clear_media(&media_dir).unwrap();
        crate::serve::clear_media(&downloads).unwrap();
        (
            Sync::new(store, commands, avatar_dir, media_dir, Some(downloads)),
            received,
        )
    }

    fn with_media(sync: &Sync, media: Vec<Value>) {
        let mut m = message("media", 1234, false, false);
        m.media = media;
        sync.store.save_messages("c", &[m]).unwrap();
    }

    fn medium(name: &str, mime: &str) -> Value {
        json!({"name": name, "mime": mime, "media_id": "secret"})
    }

    fn attachment(name: &str, full: bool) -> phone::Attachment {
        phone::Attachment {
            content_type: "image/png".into(),
            name: Some(name.into()),
            data: b"image".to_vec(),
            width: None,
            height: None,
            full,
        }
    }

    fn path(out: &[proto::Out], rid: u64) -> PathBuf {
        let proto::Out::Reply {
            rid: got,
            result: Ok(data),
        } = &out[0]
        else {
            panic!("media reply")
        };
        assert_eq!(*got, rid);
        PathBuf::from(data["path"].as_str().unwrap())
    }

    fn error(out: &[proto::Out], code: proto::ErrorCode) {
        assert!(matches!(&out[0], proto::Out::Reply { result: Err(e), .. } if e.code == code));
    }

    #[test]
    fn media_cached_offline_and_valid_index_filter() {
        let (mut sync, mut commands) = sync();
        with_media(
            &sync,
            vec![json!({"bad": true}), medium("x.png", "image/png")],
        );
        error(
            &sync.media(1, "missing", 0, false),
            proto::ErrorCode::NotFound,
        );
        error(
            &sync.media(2, "media", 1, false),
            proto::ErrorCode::NotFound,
        );
        error(
            &sync.media(3, "media", 0, false),
            proto::ErrorCode::PhoneOffline,
        );
        sync.store
            .save_attachment(
                "media",
                0,
                &store::Attachment {
                    content_type: "image/png".into(),
                    file_name: Some("x.png".into()),
                    bytes: b"cached".to_vec(),
                    width: None,
                    height: None,
                },
            )
            .unwrap();
        let out = sync.media(4, "media", 0, false);
        assert_eq!(fs::read(path(&out, 4)).unwrap(), b"cached");
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn media_miss_shares_download_and_serializes_indexes() {
        let (mut sync, mut commands) = sync();
        with_media(
            &sync,
            vec![medium("a.png", "image/png"), medium("b.png", "image/png")],
        );
        sync.ready = true;
        assert!(sync.media(1, "media", 0, false).is_empty());
        assert!(sync.media(2, "media", 0, false).is_empty());
        assert!(sync.media(3, "media", 1, false).is_empty());
        let phone::Command::Download { id, media, .. } = commands.try_recv().unwrap() else {
            panic!("download")
        };
        assert_eq!(media.len(), 1);
        assert!(commands.try_recv().is_err());
        let out = sync.handle(phone::Event::Downloaded {
            id,
            attachments: vec![attachment("a.png", true)],
            error: None,
        });
        assert_eq!(out.len(), 2);
        assert_eq!(path(&out[..1], 1), path(&out[1..], 2));
        assert_eq!(
            fs::metadata(path(&out[..1], 1))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let phone::Command::Download { id, media, .. } = commands.try_recv().unwrap() else {
            panic!("second download")
        };
        assert_eq!(media.len(), 1);
        let out = sync.handle(phone::Event::Downloaded {
            id,
            attachments: vec![attachment("b.png", true)],
            error: None,
        });
        assert!(path(&out, 3).exists());
        assert!(sync.store.attachment("media", 0).unwrap().is_some());
    }

    #[test]
    fn forgotten_download_caches_without_writing_and_preview_does_not_cache() {
        let (mut sync, mut commands) = sync();
        with_media(&sync, vec![medium("x.png", "image/png")]);
        sync.ready = true;
        sync.media(1, "media", 0, true);
        let phone::Command::Download { id, .. } = commands.try_recv().unwrap() else {
            panic!("download")
        };
        sync.forget(1);
        assert!(
            sync.handle(phone::Event::Downloaded {
                id,
                attachments: vec![attachment("x.png", true)],
                error: None
            })
            .is_empty()
        );
        assert!(sync.store.attachment("media", 0).unwrap().is_some());
        assert_eq!(fs::read_dir(&sync.media_dir).unwrap().count(), 0);
        assert_eq!(
            fs::read_dir(sync.downloads.as_ref().unwrap())
                .unwrap()
                .count(),
            0
        );

        sync.store
            .save_messages(
                "c",
                &[{
                    let mut m = message("preview", 1234, false, false);
                    m.media = vec![medium("x.png", "image/png")];
                    m
                }],
            )
            .unwrap();
        sync.media(2, "preview", 0, false);
        let phone::Command::Download { id, .. } = commands.try_recv().unwrap() else {
            panic!("preview download")
        };
        let out = sync.handle(phone::Event::Downloaded {
            id,
            attachments: vec![attachment("x.png", false)],
            error: None,
        });
        assert!(path(&out, 2).to_string_lossy().contains("-preview.png"));
        assert!(sync.store.attachment("preview", 0).unwrap().is_none());
    }

    #[test]
    fn media_save_names_and_does_not_overwrite() {
        let (mut sync, _) = sync();
        with_media(&sync, vec![medium("name.png", "image/png")]);
        sync.store
            .save_attachment(
                "media",
                0,
                &store::Attachment {
                    content_type: "image/png".into(),
                    file_name: Some("name.png".into()),
                    bytes: b"new".to_vec(),
                    width: None,
                    height: None,
                },
            )
            .unwrap();
        let downloads = sync.downloads.as_ref().unwrap().clone();
        fs::write(downloads.join("name.png"), b"old").unwrap();
        let first = path(&sync.media(1, "media", 0, true), 1);
        let second = path(&sync.media(2, "media", 0, true), 2);
        assert_eq!(first.file_name().unwrap(), "name (2).png");
        assert_eq!(second.file_name().unwrap(), "name (3).png");
        assert_eq!(fs::read(downloads.join("name.png")).unwrap(), b"old");
        assert_eq!(fs::read(first).unwrap(), b"new");
        for (name, mime, expected) in [
            ("../x.png", "image/png", "x.png"),
            (".hidden", "image/png", "message-1234.png"),
            ("", "IMAGE/JPEG; q=1", "message-1234.jpg"),
        ] {
            let a = store::Attachment {
                content_type: mime.into(),
                file_name: Some(name.into()),
                bytes: b"x".to_vec(),
                width: None,
                height: None,
            };
            let dir = downloads.join(format!("case-{expected}"));
            assert_eq!(
                save_download(&dir, &a, 1234, extension(mime))
                    .unwrap()
                    .file_name()
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn save_name_drops_bidi_and_keeps_extension() {
        let dir = std::env::temp_dir().join(format!("gm-name-{}", std::process::id()));
        let long = format!("{}.jpeg", "é".repeat(150));
        for (name, check) in [
            ("a\u{202E}gpj.\u{200B}exe\u{FEFF}", "agpj.exe".to_string()),
            (long.as_str(), String::new()),
        ] {
            let a = store::Attachment {
                content_type: "image/jpeg".into(),
                file_name: Some(name.into()),
                bytes: b"x".to_vec(),
                width: None,
                height: None,
            };
            let sub = dir.join(format!("{}", check.len()));
            let got = save_download(&sub, &a, 1, "jpg").unwrap();
            let got = got.file_name().unwrap().to_str().unwrap().to_string();
            if check.is_empty() {
                assert!(got.len() <= 200 && got.ends_with(".jpeg"), "{got}");
            } else {
                assert_eq!(got, check);
            }
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn react_done_and_unpair_waiters() {
        let (mut sync, mut commands) = sync();
        for (rid, failed) in [(1, false), (2, true)] {
            sync.react(
                rid,
                proto::ReactParams {
                    conversation: "c".into(),
                    message: "media".into(),
                    emoji: "👍".into(),
                    op: proto::ReactOp::Add,
                },
            );
            assert!(
                matches!(commands.try_recv().unwrap(), phone::Command::React { id, .. } if id == rid)
            );
            let out = sync.handle(phone::Event::Done {
                id: rid,
                error: failed.then(|| "secret error".into()),
            });
            if failed {
                error(&out, proto::ErrorCode::PhoneOffline);
            } else {
                assert!(matches!(&out[0], proto::Out::Reply { result: Ok(_), .. }));
            }
        }
        with_media(&sync, vec![medium("x.png", "image/png")]);
        sync.ready = true;
        sync.media(3, "media", 0, false);
        fs::write(sync.media_dir.join("old"), b"x").unwrap();
        let out = sync.handle(phone::Event::Unpaired {
            reason: "bye".into(),
        });
        error(&out, proto::ErrorCode::NotPaired);
        assert_eq!(fs::read_dir(&sync.media_dir).unwrap().count(), 0);
    }

    #[test]
    fn action_done_updates_store_and_outlives_reply() {
        let (mut sync, mut commands) = sync();
        for (id, archive) in [(1, true), (2, false)] {
            let p = proto::ConversationParams {
                conversation: "c".into(),
            };
            if archive {
                sync.archive(id, p)
            } else {
                sync.restore(id, p)
            }
            assert!(sync.busy());
            assert!(matches!(
                commands.try_recv().unwrap(),
                phone::Command::Archive { .. } | phone::Command::Restore { .. }
            ));
            let out = sync.handle(phone::Event::Done { id, error: None });
            assert!(matches!(&out[0], proto::Out::Reply { result: Ok(v), .. } if *v == json!({})));
            assert!(
                matches!(commands.try_recv().unwrap(), phone::Command::Lookup(ids) if ids == ["c"])
            );
        }
        sync.delete_chat(
            3,
            proto::ConversationParams {
                conversation: "c".into(),
            },
        );
        assert!(matches!(
            commands.try_recv().unwrap(),
            phone::Command::DeleteChat { for_good: true, .. }
        ));
        sync.forget(3);
        let out = sync.handle(phone::Event::Done { id: 3, error: None });
        assert_eq!(data(&out, "gone")[0]["conversation"], "c");
        assert!(
            !out.iter()
                .any(|out| matches!(out, proto::Out::Reply { .. }))
        );
        assert!(
            sync.store
                .conversation_list()
                .unwrap()
                .iter()
                .any(|c| c.id == "c" && c.hidden)
        );
        assert!(!sync.busy());
    }

    #[test]
    fn delete_chat_broadcasts_gone_and_send_counts_as_busy() {
        let (mut sync, mut commands) = sync();
        sync.delete_chat(
            1,
            proto::ConversationParams {
                conversation: "c".into(),
            },
        );
        commands.try_recv().unwrap();
        let out = sync.handle(phone::Event::Done { id: 1, error: None });
        assert_eq!(data(&out, "gone")[0]["conversation"], "c");
        assert!(
            matches!(out.last().unwrap(), proto::Out::Reply { result: Ok(v), .. } if *v == json!({}))
        );
        assert!(
            sync.store
                .conversation_list()
                .unwrap()
                .iter()
                .any(|c| c.id == "c" && c.hidden)
        );
        assert!(sync.send(2, "c".into(), "hi".into(), None, Vec::new()));
        assert!(sync.busy());
        commands.try_recv().unwrap();
        sync.handle(phone::Event::Sent {
            id: 2,
            conversation: "c".into(),
            outcome: phone::SendOutcome::Unavailable("offline".into()),
        });
        assert!(!sync.busy());
    }

    #[test]
    fn delete_message_failure_and_reset() {
        let (mut sync, mut commands) = sync();
        sync.delete_message(
            1,
            proto::DeleteMessageParams {
                conversation: "c".into(),
                message: "stored".into(),
            },
        );
        assert!(matches!(
            commands.try_recv().unwrap(),
            phone::Command::DeleteMessage { .. }
        ));
        let out = sync.handle(phone::Event::Done { id: 1, error: None });
        assert_eq!(data(&out, "gone")[0]["message"], "stored");
        assert!(sync.store.message("stored").unwrap().is_none());
        sync.archive(
            2,
            proto::ConversationParams {
                conversation: "c".into(),
            },
        );
        let out = sync.handle(phone::Event::Done {
            id: 2,
            error: Some("SECRET".into()),
        });
        error(&out, proto::ErrorCode::PhoneOffline);
        assert!(
            matches!(&out[0], proto::Out::Reply { result: Err(e), .. } if e.message == "the phone refused")
        );
        sync.restore(
            3,
            proto::ConversationParams {
                conversation: "c".into(),
            },
        );
        let out = sync.handle(phone::Event::Paired);
        error(&out, proto::ErrorCode::NotPaired);
        assert!(!sync.busy());
    }

    #[test]
    fn deleted_message_cannot_return_from_late_history_or_live_event() {
        let (mut sync, mut commands) = sync();
        sync.delete_message(
            1,
            proto::DeleteMessageParams {
                conversation: "c".into(),
                message: "stored".into(),
            },
        );
        commands.try_recv().unwrap();
        sync.handle(phone::Event::Done { id: 1, error: None });
        let late = message("stored", 100, false, false);
        let out = sync.handle(phone::Event::History {
            conversation: "c".into(),
            messages: vec![late.clone()],
            error: None,
        });
        assert_eq!(data(&out, "history")[0]["messages"], json!([]));
        assert!(sync.store.message("stored").unwrap().is_none());
        assert!(
            sync.handle(phone::Event::Message {
                conversation: "c".into(),
                message: late.clone(),
                old: false,
            })
            .is_empty()
        );
        assert!(sync.store.message("stored").unwrap().is_none());
        sync.handle(phone::Event::Paired);
        sync.handle(phone::Event::History {
            conversation: "c".into(),
            messages: vec![late],
            error: None,
        });
        assert!(sync.store.message("stored").unwrap().is_some());
    }

    #[test]
    fn contacts_page_and_started_chat() {
        let (mut sync, mut commands) = sync();
        assert!(
            sync.contacts(
                1,
                proto::ContactsParams {
                    offset: 0,
                    limit: 1
                }
            )
            .is_empty()
        );
        assert!(matches!(
            commands.try_recv().unwrap(),
            phone::Command::Contacts { id: 1 }
        ));
        let out = sync.handle(phone::Event::Contacts {
            id: 1,
            contacts: vec![
                phone::Contact {
                    name: "Ann".into(),
                    number: "123".into(),
                },
                phone::Contact {
                    name: "Bea".into(),
                    number: "456".into(),
                },
            ],
            error: None,
        });
        assert!(
            matches!(&out[0], proto::Out::Reply { result: Ok(v), .. } if v["total"] == 2 && v["items"][0]["name"] == "Ann")
        );
        let out = sync.contacts(
            2,
            proto::ContactsParams {
                offset: 1,
                limit: 1,
            },
        );
        assert!(
            matches!(&out[0], proto::Out::Reply { result: Ok(v), .. } if v["items"][0]["name"] == "Bea")
        );
        assert!(commands.try_recv().is_err());
        sync.contacts(
            3,
            proto::ContactsParams {
                offset: 0,
                limit: 2,
            },
        );
        commands.try_recv().unwrap();
        let out = sync.handle(phone::Event::Contacts {
            id: 3,
            contacts: vec![],
            error: Some("SECRET".into()),
        });
        error(&out, proto::ErrorCode::PhoneOffline);
        sync.start_chat(
            4,
            proto::StartChatParams {
                numbers: vec!["123".into()],
            },
        );
        assert!(matches!(
            commands.try_recv().unwrap(),
            phone::Command::StartChat { id: 4, .. }
        ));
        let mut chat = conversation(false);
        chat.id = "new".into();
        let out = sync.handle(phone::Event::Started {
            id: 4,
            conversation: Some(chat),
            error: None,
        });
        assert_eq!(data(&out, "conversation")[0]["conversation"]["id"], "new");
        assert!(
            matches!(out.last().unwrap(), proto::Out::Reply { result: Ok(v), .. } if v["conversation"] == "new")
        );
        sync.start_chat(
            5,
            proto::StartChatParams {
                numbers: vec!["456".into()],
            },
        );
        commands.try_recv().unwrap();
        let out = sync.handle(phone::Event::Started {
            id: 5,
            conversation: None,
            error: None,
        });
        error(&out, proto::ErrorCode::PhoneOffline);
    }

    #[test]
    fn started_new_conversation_fetches_history() {
        let (mut sync, mut commands) = sync();
        sync.start_chat(
            1,
            proto::StartChatParams {
                numbers: vec!["123".into()],
            },
        );
        assert!(matches!(
            commands.try_recv().unwrap(),
            phone::Command::StartChat { id: 1, .. }
        ));
        let mut chat = conversation(false);
        chat.id = "new".into();
        sync.handle(phone::Event::Started {
            id: 1,
            conversation: Some(chat),
            error: None,
        });
        assert!(
            matches!(commands.try_recv().unwrap(), phone::Command::Fetch(ids) if ids == ["new"])
        );
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
    fn upload_guard_outlives_forget_and_clears_only_on_matching_sent() {
        let (mut sync, mut commands) = sync();
        let file = PathBuf::from("/tmp/photo.png");
        assert!(sync.send(1, "c".into(), "".into(), None, vec![file.clone()]));
        assert!(
            matches!(commands.try_recv().unwrap(), phone::Command::Send { id: 1, files, .. } if files == [file.clone()])
        );
        assert!(!sync.send(2, "c".into(), "".into(), None, vec![file.clone()]));
        assert!(commands.try_recv().is_err());
        assert!(sync.send(3, "c".into(), "text".into(), None, Vec::new()));
        assert!(
            matches!(commands.try_recv().unwrap(), phone::Command::Send { id: 3, files, .. } if files.is_empty())
        );
        sync.forget(1);
        assert!(!sync.send(4, "c".into(), "".into(), None, vec![file.clone()]));
        assert!(commands.try_recv().is_err());
        sync.handle(phone::Event::Sent {
            id: 3,
            conversation: "c".into(),
            outcome: phone::SendOutcome::Unavailable("offline".into()),
        });
        assert!(!sync.send(5, "c".into(), "".into(), None, vec![file.clone()]));
        assert!(commands.try_recv().is_err());
        let out = sync.handle(phone::Event::Sent {
            id: 1,
            conversation: "c".into(),
            outcome: phone::SendOutcome::Attachment("photo.png: empty".into()),
        });
        assert!(
            matches!(&out[0], proto::Out::Reply { rid: 1, result: Err(error) }
            if error.code == proto::ErrorCode::Attachment && error.message == "photo.png: empty")
        );
        assert!(sync.send(6, "c".into(), "".into(), None, vec![file]));
        assert!(matches!(
            commands.try_recv().unwrap(),
            phone::Command::Send { id: 6, .. }
        ));
    }

    #[test]
    fn accepted_send_event_keeps_uploaded_attachment_name() {
        let (mut sync, mut commands) = sync();
        let mut m = message("temporary", 500, true, false);
        m.media = vec![medium("photo.png", "image/png")];
        let out = sync.handle(phone::Event::Sent {
            id: 1,
            conversation: "c".into(),
            outcome: phone::SendOutcome::Accepted(m),
        });
        assert_eq!(
            data(&out, "message")[0]["message"]["attachments"][0]["name"],
            "photo.png"
        );
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
