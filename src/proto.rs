//! The wire protocol v1 between the daemon and its clients: NDJSON, one envelope per line.
//! These are the only serialized types of the wire; none of them carries a secret, and no
//! `phone::` type is ever serialized onto it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

use crate::{phone, store};

pub const PROTOCOL: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Starting,
    Locked,
    KeyringMissing,
    StoreUnreadable,
    Login,
    Pairing,
    Connecting,
    Ready,
    Expired,
    Fatal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotPaired,
    Locked,
    PhoneOffline,
    SendRejected,
    SendUncertain,
    NotFound,
    BadRequest,
    UnknownMethod,
    ProtocolMismatch,
    Busy,
    Timeout,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub id: Option<u64>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: Option<u64>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Reply {
    pub fn ok(id: Option<u64>, data: Value) -> Self {
        Self {
            id,
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn err(id: Option<u64>, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: false,
            data: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
            }),
        }
    }

    /// The reply to a line that is not a request.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::err(None, ErrorCode::BadRequest, message)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventLine {
    pub event: String,
    pub data: Value,
}

impl EventLine {
    /// An event named `event`, carrying `data`.
    pub fn new(event: &str, data: &impl Serialize) -> Self {
        Self {
            event: event.to_owned(),
            // Plain structs of strings and numbers: serializing cannot fail.
            data: serde_json::to_value(data).unwrap_or(Value::Null),
        }
    }
}

/// What `Sync` hands the daemon.
pub enum Out {
    Broadcast(EventLine),
    Reply {
        rid: u64,
        result: Result<Value, ErrorBody>,
    },
}

/// Parses one line of a client.
pub fn parse_request(line: &str) -> Result<Request, Reply> {
    serde_json::from_str(line).map_err(|e| Reply::bad_request(format!("not a request: {e}")))
}

// ---- DTOs ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub size: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Conversation {
    pub id: String,
    pub name: String,
    pub group: bool,
    pub folder: String,
    pub hidden: bool,
    pub unread: bool,
    pub last_ms: i64,
    pub snippet: String,
    pub color: String,
    pub avatar: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub conversation: String,
    pub from_me: bool,
    pub sender: String,
    pub text: String,
    pub timestamp_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quote: Option<String>,
    pub deleted: bool,
    pub failed: bool,
    pub pending: bool,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
}

/// One emoji on a message: how many people sent it, and whether I did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub count: u32,
    pub mine: bool,
}

/// `text` cut to at most 80 bytes, on a char boundary.
fn snippet(text: &str) -> String {
    let mut end = text.len().min(80);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn folder(folder: &Option<String>) -> String {
    folder.clone().unwrap_or_else(|| "inbox".to_owned())
}

impl Conversation {
    pub fn from_phone(c: &phone::Conversation, color: String, avatar: Option<String>) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            group: c.group,
            folder: folder(&c.folder),
            hidden: c.deleted,
            unread: c.unread,
            last_ms: c.timestamp_ms,
            snippet: c
                .latest
                .as_ref()
                .map_or_else(String::new, |l| snippet(&l.text)),
            color,
            avatar,
        }
    }

    pub fn from_store(c: &store::Conversation, color: String, avatar: Option<String>) -> Self {
        Self {
            id: c.id.clone(),
            name: c.name.clone(),
            group: c.group,
            folder: folder(&c.folder),
            hidden: c.hidden,
            unread: c.unread,
            last_ms: c.timestamp_ms,
            snippet: c
                .messages
                .last()
                .map_or_else(String::new, |m| snippet(&m.text)),
            color,
            avatar,
        }
    }
}

impl Message {
    pub fn from_phone(conversation: &str, m: &phone::Message, pending: bool) -> Self {
        // Only name, type and size leave the media; their keys stay in the store.
        let attachments = m
            .media
            .iter()
            .filter_map(|v| serde_json::from_value::<phone::Medium>(v.clone()).ok())
            .map(|d| Attachment {
                name: d.name,
                mime: d.mime,
                size: d.size,
            })
            .collect();
        let mut reactions: Vec<Reaction> = Vec::new();
        for r in &m.reactions {
            match reactions.iter_mut().find(|x| x.emoji == r.emoji) {
                Some(x) => {
                    x.count += 1;
                    x.mine |= r.mine;
                }
                None => reactions.push(Reaction {
                    emoji: r.emoji.clone(),
                    count: 1,
                    mine: r.mine,
                }),
            }
        }
        Self {
            id: m.id.clone(),
            conversation: conversation.to_owned(),
            from_me: m.from_me,
            sender: m.sender.clone(),
            text: m.text.clone(),
            timestamp_ms: m.timestamp_ms,
            quote: m.quote.clone(),
            deleted: m.deleted,
            failed: m.failed,
            pending,
            attachments,
            reactions,
        }
    }
}

// ---- Method params ----

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HelloParams {
    pub protocol: u32,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    App,
    Bar,
    Service,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairMethod {
    Google,
    Qr,
    Manual,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PairStartParams {
    pub method: PairMethod,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Folder {
    Inbox,
    Archive,
    Spam,
    Trash,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConversationsParams {
    pub folder: Folder,
    #[serde(default)]
    pub offset: i64,
    /// At most 200.
    pub limit: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConversationParams {
    pub conversation: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ContactsParams {
    #[serde(default)]
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StartChatParams {
    pub numbers: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DeleteMessageParams {
    pub conversation: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SendParams {
    pub conversation: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReactParams {
    pub conversation: String,
    pub message: String,
    pub emoji: String,
    pub op: ReactOp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReactOp {
    Add,
    Remove,
    Replace,
}

impl From<ReactOp> for gmessages_proto::ReactionOp {
    fn from(op: ReactOp) -> Self {
        match op {
            ReactOp::Add => Self::Add,
            ReactOp::Remove => Self::Remove,
            ReactOp::Replace => Self::Replace,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaParams {
    pub message: String,
    pub index: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MediaData {
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FocusParams {
    pub conversation: Option<String>,
}

// ---- Reply data ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateData {
    pub state: State,
    pub detail: Option<String>,
    pub unread: i64,
}

/// The `hello` event the daemon sends first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloEvent {
    pub protocol: u32,
    pub version: String,
    pub state: State,
    pub detail: Option<String>,
    pub unread: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationsData {
    pub items: Vec<Conversation>,
    pub total: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContactDto {
    pub name: String,
    pub number: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContactsData {
    pub items: Vec<ContactDto>,
    pub total: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StartChatData {
    pub conversation: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GoneEvent {
    pub conversation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryData {
    pub messages: Vec<Message>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendStatus {
    Sent,
    Accepted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendData {
    pub status: SendStatus,
    pub message: Message,
}

// ---- Event payloads ----

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateEvent {
    pub state: State,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PairingEvent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emoji: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qr_svg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConversationEvent {
    pub conversation: Conversation,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AvatarsEvent {
    pub items: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEvent {
    pub conversation: String,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEvent {
    pub conversation: String,
    pub message: Message,
    pub notify: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissedEvent {
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnreadEvent {
    pub count: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypingEvent {
    pub conversation: String,
    pub name: String,
    pub started: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round<T: Serialize + for<'a> Deserialize<'a> + PartialEq + std::fmt::Debug>(v: T) {
        let line = serde_json::to_string(&v).unwrap();
        assert!(!line.contains('\n'));
        assert_eq!(serde_json::from_str::<T>(&line).unwrap(), v);
    }

    #[test]
    fn envelopes_round_trip() {
        round(Request {
            id: Some(3),
            method: "send".into(),
            params: json!({"conversation": "c", "text": "hi"}),
        });
        round(Reply::ok(Some(3), json!({})));
        round(Reply::err(Some(4), ErrorCode::SendUncertain, "maybe"));
        round(EventLine::new(
            "state",
            &StateEvent {
                state: State::KeyringMissing,
                detail: None,
            },
        ));
        let line = serde_json::to_string(&Reply::err(None, ErrorCode::NotPaired, "x")).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap(),
            json!({"id": null, "ok": false, "error": {"code": "not_paired", "message": "x"}})
        );
    }

    #[test]
    fn non_json_line_is_bad_request() {
        let reply = parse_request("this is not json").unwrap_err();
        assert!(!reply.ok);
        assert_eq!(reply.id, None);
        assert_eq!(reply.error.unwrap().code, ErrorCode::BadRequest);
    }

    #[test]
    fn snippet_cuts_on_a_char_boundary() {
        let text = "é".repeat(60); // 120 bytes
        let s = snippet(&text);
        assert_eq!(s.len(), 80);
        assert_eq!(snippet("a€"), "a€");
        assert_eq!(snippet(&format!("{}€", "a".repeat(79))).len(), 79);
    }

    fn phone_message() -> phone::Message {
        let medium = |n: u8| {
            serde_json::to_value(phone::Medium {
                mime: "image/png".into(),
                name: format!("p{n}.png"),
                size: 10,
                part: format!("SECRET-PART-{n}"),
                media_id: format!("SECRET-ID-{n}"),
                key: format!("SECRET-KEY-{n}").into_bytes(),
                thumb_id: format!("SECRET-THUMB-ID-{n}"),
                thumb_key: format!("SECRET-THUMB-KEY-{n}").into_bytes(),
                ..Default::default()
            })
            .unwrap()
        };
        phone::Message {
            id: "m1".into(),
            timestamp_ms: 5,
            from_me: false,
            sender_id: "SECRET-SENDER-ID".into(),
            sender: "Ann".into(),
            text: "hello".into(),
            quote: Some("m0".into()),
            media: vec![medium(1), medium(2)],
            reactions: vec![
                phone::Reaction {
                    emoji: "👍".into(),
                    sender_id: "SECRET-REACTOR-1".into(),
                    mine: false,
                },
                phone::Reaction {
                    emoji: "❤️".into(),
                    sender_id: "SECRET-REACTOR-2".into(),
                    mine: false,
                },
                phone::Reaction {
                    emoji: "👍".into(),
                    sender_id: "SECRET-REACTOR-3".into(),
                    mine: true,
                },
            ],
            deleted: false,
            failed: false,
        }
    }

    #[test]
    fn reactions_are_aggregated_per_emoji() {
        let dto = Message::from_phone("c1", &phone_message(), false);
        let want = |emoji: &str, count, mine| Reaction {
            emoji: emoji.into(),
            count,
            mine,
        };
        assert_eq!(dto.reactions, [want("👍", 2, true), want("❤️", 1, false)]);
    }

    #[test]
    fn dtos_carry_no_secrets() {
        let m = phone_message();
        let dto = Message::from_phone("c1", &m, true);
        assert_eq!(dto.attachments.len(), 2);
        assert_eq!(dto.attachments[0].name, "p1.png");
        assert!(dto.pending);

        let pc = phone::Conversation {
            id: "c1".into(),
            name: "Ann".into(),
            group: false,
            timestamp_ms: 9,
            latest: Some(phone::Latest {
                id: "m1".into(),
                text: "hello".into(),
                from_me: false,
                sender: "Ann".into(),
            }),
            folder: None,
            deleted: false,
            unread: true,
            colour: "#1a73e8".into(),
            avatar_id: Some("c:contact".into()),
        };
        let conv = Conversation::from_phone(&pc, "#1a73e8".into(), Some("/avatar.png".into()));
        assert_eq!(conv.folder, "inbox");
        assert_eq!(conv.snippet, "hello");
        let sc = store::Conversation {
            id: "c1".into(),
            name: "Ann".into(),
            group: false,
            timestamp_ms: 9,
            unread: true,
            hidden: false,
            folder: Some("archive".into()),
            colour: String::new(),
            avatar_id: None,
            messages: vec![m.clone()],
        };
        let from_store = Conversation::from_store(&sc, String::new(), None);
        assert_eq!(from_store.folder, "archive");
        assert_eq!(from_store.snippet, "hello");

        let wire = [
            serde_json::to_string(&dto).unwrap(),
            serde_json::to_string(&conv).unwrap(),
            serde_json::to_string(&from_store).unwrap(),
            serde_json::to_string(&SendData {
                status: SendStatus::Accepted,
                message: dto.clone(),
            })
            .unwrap(),
            serde_json::to_string(&HistoryData {
                messages: vec![dto.clone()],
                truncated: false,
            })
            .unwrap(),
            serde_json::to_string(&HistoryEvent {
                conversation: "c1".into(),
                messages: vec![dto.clone()],
            })
            .unwrap(),
            serde_json::to_string(&MessageEvent {
                conversation: "c1".into(),
                message: dto,
                notify: true,
            })
            .unwrap(),
            serde_json::to_string(&ConversationsData {
                items: vec![conv.clone(), from_store],
                total: 2,
            })
            .unwrap(),
            serde_json::to_string(&ConversationEvent { conversation: conv }).unwrap(),
            serde_json::to_string(&ContactsParams {
                offset: 0,
                limit: 200,
            })
            .unwrap(),
            serde_json::to_string(&StartChatParams {
                numbers: vec!["123".into()],
            })
            .unwrap(),
            serde_json::to_string(&DeleteMessageParams {
                conversation: "c1".into(),
                message: "m1".into(),
            })
            .unwrap(),
            serde_json::to_string(&ContactsData {
                items: vec![ContactDto {
                    name: "Ann".into(),
                    number: "123".into(),
                }],
                total: 1,
            })
            .unwrap(),
            serde_json::to_string(&StartChatData {
                conversation: "c1".into(),
            })
            .unwrap(),
            serde_json::to_string(&GoneEvent {
                conversation: "c1".into(),
                message: Some("m1".into()),
            })
            .unwrap(),
            serde_json::to_string(&AvatarsEvent {
                items: BTreeMap::from([("c1".into(), "/avatar.png".into())]),
            })
            .unwrap(),
        ];
        for line in wire {
            assert!(!line.contains("SECRET"), "secret on the wire: {line}");
        }
    }
}
