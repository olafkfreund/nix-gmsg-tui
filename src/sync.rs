//! Keeps the encrypted store, phone events and wire events in step.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use qrcode::QrCode;
use qrcode::render::svg;
use serde_json::to_value;

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
}

impl Sync {
    pub fn new(store: Rc<store::Store>, commands: phone::Commands) -> Self {
        let stored = store.conversation_list().unwrap_or_default();
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
        if conversation.deleted
            && let Err(error) = self.store.forget(&conversation.id)
        {
            tracing::warn!(%error, "cannot forget conversation");
        }
        self.known.insert(conversation.id.clone());
        vec![Self::event(
            "conversation",
            &proto::ConversationEvent {
                conversation: proto::Conversation::from_phone(&conversation),
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
            phone::Event::Login => vec![Self::state(proto::State::Login, None)],
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
            phone::Event::PairFailed { message } => vec![
                Self::event(
                    "pairing",
                    &proto::PairingEvent {
                        message: Some(message),
                        ..Default::default()
                    },
                ),
                Self::state(proto::State::Login, None),
            ],
            phone::Event::Paired | phone::Event::Connected => {
                vec![Self::state(proto::State::Connecting, None)]
            }
            phone::Event::Ready {
                conversations,
                since,
                partial,
            } => {
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
                let mut out = self.conversation(conversation);
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
                                conversation: proto::Conversation::from_store(c),
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
                vec![Self::state(proto::State::Expired, Some(message))]
            }
            phone::Event::Unpaired { .. } => vec![Self::state(
                proto::State::Login,
                Some("unpaired_remotely".into()),
            )],
            phone::Event::Fatal { message } => {
                vec![Self::state(proto::State::Fatal, Some(message))]
            }
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
        &self,
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
        proto::ConversationsData {
            total: conversations.len() as i64,
            items: conversations
                .iter()
                .skip(offset.max(0) as usize)
                .take(limit.clamp(0, 200) as usize)
                .map(proto::Conversation::from_store)
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
        (Sync::new(store, commands), received)
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
        let (sync, _) = sync();
        assert_eq!(sync.history("c").messages.len(), 1);
        assert!(!sync.history("c").truncated);
        assert_eq!(sync.conversations(proto::Folder::Inbox, 0, 200).total, 1);
        assert!(
            sync.conversations(proto::Folder::Inbox, 1, 200)
                .items
                .is_empty()
        );
    }
}
