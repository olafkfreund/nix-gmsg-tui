---
status: approved
issue: none (issues are disabled on the fork)
spec: spec/2026-11-11-omarchy-messages.md
stage: a (packaging, backend, pairing, conversation list, history, send, badge, notifications)
coders:
  1: codex   # Nix packaging of the current tree: small, needs exact hashes
  2: codex   # two small, tricky bug fixes with tests
  3: codex   # store migration in a transaction, typed send outcome, cancel, logging
  4: coder   # wire types, conversions and the secrets test: plain serde work
  5: codex   # TUI removal coupled to the Sync port and its notify/unread rules
  6: codex   # daemon core: lock, socket, handshake, dispatch, pairing, signals
  7: agy     # QML plugin: many files, follows meetingai's shape and the interfaces below
  8: coder   # plugin.nix, hm-module.nix, flake outputs, README
reviewers:
  - codex   # independent read of the whole diff against this plan
  - agy     # second independent read; disagreement is the point
---

# Plan: Messages plugin, stage a

## What is decided (copied from the approved spec)

**Product.** Google Messages becomes an Omarchy shell plugin, `olafkfreund.gmessages`
("Messages", AGPL-3.0-only). It has three parts:
- a background service (notifications);
- a bar widget (unread badge);
- a `panel` entry `App.qml`, which is a Quickshell `FloatingWindow` (app id
  `olafkfreund.gmessages`).

The TUI is no longer built. Stage a delivers packaging, the backend, pairing, the
conversation list, history, send, the badge and notifications. Stages b (media,
reactions) and c (contacts, new chats, archive, restore, delete, unpair, uninstall) get
their own plans.

**Backend.** `gmessages-tui serve` runs as a systemd user unit and is the only owner of
the store.
- The CLI is `serve`, `--version` and the hidden `--internal-google-signin`.
- Paths (new `src/paths.rs`):
  - data `dirs::data_dir()/gmessages-tui`: `session.json`, `messages.db`, `phone.log`,
    `lock`;
  - cache `dirs::cache_dir()/gmessages-tui`;
  - runtime `$XDG_RUNTIME_DIR/gmessages-tui`: `daemon.sock`, `media/`. Mode 0700; systemd
    creates it (`RuntimeDirectory=`), and `serve` creates it too and clears `media/` at
    startup.
- Lifecycle:
  - `flock` with O_CLOEXEC on `<data>/lock`, with the pid written into it. A second
    instance exits 1 with "already running (pid N)".
  - With the lock held: remove a stale socket, bind `daemon.sock` (0600), accept only
    peers with the same uid (`SO_PEERCRED`).
  - Startup order: lock, socket, key, store, phone.
  - SIGTERM or SIGINT: `phone::Control::stop`, drop the sign-in window/page, clear
    `media/`, remove the socket, exit within 5 s.
- Store key:
  - `GMESSAGES_TUI_PASSPHRASE` if set; empty gives state `locked` with detail
    `empty_passphrase`. Else the keyring, through `spawn_blocking`.
  - Locked, missing or refusing to store gives state `locked` or `keyring_missing`, and a
    retry on `key.retry` and every 30 s. Never fatal.
  - A store that won't open gives `store_unreadable` and is never recreated.
- Logging: `phone.log` filter defaults to `gmessages_proto=info,gmessages_tui=info`, cut to
  its last 1 MiB at startup. It never logs message text, cookies, tokens or the pairing
  URL.

**Protocol v1** (`src/proto.rs`, the only serialized types). NDJSON, at most 1 MiB per
line. A longer line, or more than 1000 queued outgoing lines, closes that client.

- Handshake:
  - daemon → `{"event":"hello","data":{"protocol":1,"version":"…","state":"…","detail":null,"unread":0}}`;
  - client → `{"id":0,"method":"hello","params":{"protocol":1,"role":"app|bar|service"}}`.
  - A mismatch gives `protocol_mismatch` and refusal of every other method from that
    client.
- Requests and replies:
  - `{"id":N,"method":"…","params":{…}}` → `{"id":N,"ok":true,"data":…}` or
    `{"id":N,"ok":false,"error":{"code":"…","message":"…"}}`.
  - Ids are per connection, mapped to global `rid`s. A malformed line gets
    `{"id":null,"ok":false,"error":{"code":"bad_request","message":"…"}}`.
  - When a client drops, its pending replies are discarded, but their effects (store,
    broadcast) still happen.
- Deadlines: `send` 120 s, everything else in stage a 15 s; then the reply is `timeout`.
  Clients wait the deadline plus 5 s.
- States (snake_case): `starting`, `locked`, `keyring_missing`, `store_unreadable`,
  `login`, `pairing`, `connecting`, `ready`, `expired`, `fatal`. The `state` event is
  `{state, detail}`.
- Errors: `not_paired`, `locked`, `phone_offline`, `send_rejected`, `send_uncertain`,
  `not_found`, `bad_request`, `unknown_method`, `protocol_mismatch`, `busy`, `timeout`,
  `internal`.

**Stage-a methods** (params → data):
- `hello`
- `state` → `{state, detail, unread}`
- `key.retry` → `{}`
- `pair.start {method: "google"|"qr"|"manual"}` → `{}`
- `pair.cancel` → `{}`
- `conversations {folder: "inbox"|"archive"|"spam"|"trash", offset, limit≤200}` →
  `{items: [Conversation], total}`
- `history {conversation}` → `{messages: [Message], truncated}`: the last 200 from the
  store. The phone fetch of the newest messages lands later as a `history` event.
- `send {conversation, text, reply_to?}` → `{status: "sent"|"accepted", message}`
- `read {conversation}` → `{}`
- `focus {conversation|null}` → `{}`

**Stage-a events** (`{"event":"<name>","data":…}`):
- `state {state, detail}`
- `pairing {emoji?, qr_svg?, manual_url?, message?}`
- `conversation {conversation: Conversation}`
- `history {conversation, messages}`
- `message {conversation, message: Message, notify}`
- `missed {count}`
- `unread {count}`
- `typing {conversation, name, started}`

**DTOs** (no secrets):
- `Conversation {id, name, group, folder, unread, last_ms, snippet}`. `snippet` is the
  last message text, cut to 80 bytes on a char boundary.
- `Message {id, conversation, from_me, sender, text, timestamp_ms, quote?, deleted,
  failed, pending, attachments: [{name, mime, size}]}`. `pending` is true only for an
  accepted send whose real id isn't known yet.

**Notify rules.** `notify` is true only when all of these hold:
- the message is live (`old: false`) and new to the store (`known: false`);
- it isn't deleted and isn't from me;
- no connection's `focus` names its conversation.

Focus belongs to its connection and is cleared on disconnect. A reconnect's missed
messages give one `missed {count}`. `unread` counts inbox, non-hidden conversations with
the stored `unread` flag set; it is sent on every change and is correct offline.

**Accepted sends.** An `Accepted` outcome:
- replies `accepted`;
- broadcasts the message with `pending: true` and its temporary id;
- asks the phone to fetch that conversation.

When a `history` event for the conversation arrives, the clients drop its pending
messages. The real message is in that history.

**Secrets.** No DTO carries cookies, keys, tokens or media keys. Sign-in uses the
backend's window. The manual path is the localhost page of `src/web.rs` (it accepts
pasted cookies or a pasted cURL command): its URL is sent only as `pairing.manual_url`,
never logged. `web::Page` aborts its server on drop, except after `done()`, when it
lingers as it does today and then ends.

**Plugin.** `Client.qml`, `Service.qml` (notify-send via `@notifySend@`, at most 3 at a
time, Open action, IPC `olafkfreund.gmessages`), `BarWidget.qml`, `App.qml` and its
components, `Theme.qml`, `Errors.js`. Details are in step 7.

**Nix.**
- `nix/package.nix`, `nix/plugin.nix` (copy, substitute, parse-check);
- `nix/hm-module.nix` with `services.gmessages.{enable,package}`: `home.packages` plus a
  user unit (`serve`; `PartOf`/`After`/`WantedBy=graphical-session.target`;
  `RuntimeDirectory=gmessages-tui`; `Restart=on-failure`; no sandboxing options);
- `flake.nix` on nixos-unstable: `packages.x86_64-linux.{default,gmessages-tui,plugin}`,
  `overlays.default`, `homeManagerModules.default`, `devShells.default`, `checks` (package
  with tests, plugin parse).
- p620's host module in `~/.config/nixos` sets the nixarchy plugin option. The flake's
  module never does.

## Rules for every step

- **Coders (codex, agy, coder):**
  - Edit only the files their step lists.
  - May run, inside `nix develop -c …`: `cargo build`, `cargo test`, `cargo clippy`,
    `cargo fmt`, `cargo update -w` (step 5 only) and `qmllint`.
  - Never run `nix build` or `nix flake check`. Never stage, commit, push, deploy or
    switch, never touch services, and never touch `~/.config/nixos`, `main` or another
    host.
- **Claude (session):** runs every line marked *(Claude)*, reviews each step's diff
  against this plan, and commits each step as its own commit naming the step. A coder's
  reported deviation is written into this plan in the same commit.
- **A step is done when:** its verify passes,
  `nix develop -c cargo clippy --workspace -- -D warnings` is clean, and
  `nix develop -c cargo fmt --check` passes. Between steps 5 and 6 only, the binary
  crate root carries `#![allow(dead_code)]`, with a comment naming step 6; step 6
  removes it.
- **Test-count guard:** every `cargo test <filter>` verify must report a non-zero number
  of tests run.
- **Repo traps:**
  - The runtime is single-threaded (`tokio::task::LocalSet`, `Rc` state; see
    `main.rs:83`), so new async code uses `spawn_local` and never needs `Send`.
  - `build.rs` shells out to `git`. Without `.git` (the Nix sandbox) it falls back to
    `v<CARGO_PKG_VERSION>`.
  - The sign-in window re-execs `current_exe()` with `--internal-google-signin`.
  - Tests in `src/signin/native.rs` spawn `sh` and read `/proc`.
  - A shell hook blocks commands whose text mentions restarting services or rebooting:
    keep that wording out of commands and heredocs.

## Steps

1. **Package the current tree first, as a baseline.** Coder: codex.
   - Files: new `flake.nix`, `nix/package.nix`.
   - `nix/package.nix` builds the current tree unchanged (TUI included).
     - `nativeBuildInputs`: `pkg-config`, `wrapGAppsHook3`, `perl`.
     - `buildInputs`: `openssl`, `dbus`, `gtk3`, `webkitgtk_4_1`, `libsoup_3`,
       `glib-networking`.
     - `cargoLock.lockFile = ../Cargo.lock`, with `outputHashes` for
       `chat-tui-core-1.12.1` and `emoji-expander-1.2.2` (`Cargo.lock:543,1099`). Start
       with `lib.fakeHash`; Claude fills in the real hash from the first build.
     - `cargoTestFlags = [ "--workspace" ]`.
     - Deviation found in implementation: `preBuild` exports
       `LIBSQLITE3_SYS_USE_PKG_CONFIG=0`. nixpkgs' `cargoUseSystemLibs` hook sets it to
       1, which makes rusqlite link a system SQLCipher (first a link failure, then
       `PRAGMA key` test failures with system `sqlcipher`) instead of building
       `bundled-sqlcipher`.
   - `flake.nix`: input nixpkgs `github:NixOS/nixpkgs/nixos-unstable`.
     - `packages.x86_64-linux.{default,gmessages-tui}`.
     - `devShells.default` with `rustc`, `cargo`, `clippy`, `rustfmt`, `rust-analyzer`,
       `qt6.qtdeclarative` (for `qmllint`) and the package's inputs (`inputsFrom`).
   - Verify:
     ```bash
     nix develop -c cargo test --workspace       # green
     ```
     Then *(Claude)*:
     ```bash
     nix build .#gmessages-tui
     ./result/bin/gmessages-tui --version        # prints v1.3.6
     git add flake.lock
     ```
   - Traps: the git deps are pinned by tag in `Cargo.toml:19` and by commit in the lock.

2. **Bug fixes.** Coder: codex.
   - Files: `src/key.rs`, `gmessages-proto/src/codec.rs:57-70`, `src/main.rs:175-180`
     (call site only).
   - `key.rs`:
     - `store_key` returns `Result<String, KeyError>`, with `pub enum KeyError { Empty,
       Locked(String), Missing(String), Refused(String) }` implementing `Display` and
       `Error`;
     - an empty `GMESSAGES_TUI_PASSPHRASE` is `Empty`;
     - keyring `NoStorageAccess` is `Locked`, any other entry or get error is `Missing`,
       and a failed `set_password` is `Refused`;
     - `ask_passphrase` and the rpassword use are deleted. The `rpassword` dependency
       stays until step 5.
   - `main.rs`: the call site maps `KeyError` with `?` (it implements `Error`). A TUI
     user without a keyring now gets an error instead of a prompt; that is acceptable,
     because the TUI goes away in step 5.
   - `codec.rs` `PbLiteError::ty`: when `s.len() > 40` bytes, truncate at
     `s.floor_char_boundary(40)` and push '…'. If that function is not stable on the
     pinned rustc, walk `char_indices`.
   - Tests:
     - `key.rs`: an empty env is `Empty`; a non-empty env wins without touching the
       keyring.
     - `codec.rs`: a `Json::String` of 21 "é" (42 bytes, 44 with quotes) doesn't panic,
       ends with '…', and its prefix is valid UTF-8 of at most 40 bytes.
   - Verify:
     ```bash
     nix develop -c cargo test -p gmessages-proto codec
     nix develop -c cargo test --bin gmessages-tui key
     ```

3. **Store and phone changes.** Coder: codex.
   - Files: `src/store.rs`, `src/phone.rs`, `src/main.rs` (consumers of `Event::Sent`
     and `Event::Fatal` only, so the TUI still compiles).
   - `store.rs`:
     - `SCHEMA = 5`. At the top of `migrate`, a `user_version > SCHEMA` is an error
       ("store from a newer version"), never downgraded.
     - The existing v1–v4 blocks are unchanged.
     - A new v5 block runs in `unchecked_transaction()`: `ALTER TABLE conversations ADD
       COLUMN unread INTEGER NOT NULL DEFAULT 0` and `PRAGMA user_version = 5`, then
       commit.
     - The trailing `pragma_update(user_version, SCHEMA)` (`store.rs:107`) stays: it
       makes 1–4 reach 5 and is a no-op after v5.
     - `store::Conversation` gains `unread: bool` and `timestamp_ms: i64` (the existing
       column), filled by every reader.
     - `save_conversations` writes `unread`. Add `set_unread(&self, id, bool)` and
       `unread_count(&self) -> i64` (inbox = `folder IS NULL`, `hidden = 0`).
   - `phone.rs`:
     - `pub enum SendOutcome { Sent(Message), Accepted(Message), Unavailable(String),
       Rejected(String), Uncertain(String) }`:
       - `Unavailable` covers failures before anything left (offline, unknown
         conversation, empty text: the early returns around `phone.rs:1275-1288`);
       - `Uncertain` is an error from `client.send_message(…)` itself;
       - `Rejected` is a non-`Accepted` result;
       - `Accepted` is the echo timeout (temporary id);
       - `Sent` is the echoed message.

       `Event::Sent` carries `outcome: SendOutcome` instead of `message`/`error`. The
       `Forget` guard (`phone.rs:1311`) still runs on every path.
     - Pairing cancel: `State` holds `pairing_cancel: RefCell<Option<gm::CancelToken>>`.
       Both pairing paths (around `phone.rs:1478` and `1492`) store their token there and
       clear it at the end. A new `Command::CancelPairing` cancels it and sets
       `inner.pairing = false`, and the pairing ends with `Event::PairFailed { message:
       "cancelled" }`.
     - The phone-side logout at `phone.rs:833-838` emits a new `Event::Unpaired { reason:
       String }` instead of `Fatal`.
     - `log::init` (`phone.rs:1720`) defaults to `gmessages_proto=info,gmessages_tui=info`.
     - Tracing audit: list every `tracing::`/`log::` call in `src/` and
       `gmessages-proto/src/` whose fields or format include message text, cookies,
       tokens, keys or URLs with tokens. Remove those fields, and report the list.
     - Outcome (deviation): no log call carries such a field directly. The one leak path
       was `reqwest::Error`'s URL inside `gm::Error::Network`. `gmessages-proto/src/error.rs`
       now builds it with `without_url()` (with a test), so error fields stay in the logs.
       Narrow `#[allow(dead_code)]`s mark the APIs that steps 4–5 consume, and two
       `store::Conversation` literals in `main.rs` gained the new fields.
   - `main.rs`: map the new `Event::Sent` and `Event::Unpaired` into the old TUI events.
     The behaviour can be approximate; it is deleted in step 5.
   - Tests:
     - store: a v4 database migrates to v5 and keeps its rows;
     - store: a v6 database is refused;
     - store: `unread_count` ignores archived and hidden conversations, and `set_unread`
       changes it;
     - phone: `SendOutcome` classification, with a fake for the client call if the code
       allows; otherwise report why not.
   - Verify:
     ```bash
     nix develop -c cargo test --bin gmessages-tui store
     nix develop -c cargo test --bin gmessages-tui phone
     ```

4. **Wire types.** Coder: coder.
   - Files: new `src/proto.rs`, `src/main.rs` (`mod proto;` only).
   - `pub const PROTOCOL: u32 = 1`.
   - Envelopes: `Request {id: Option<u64>, method: String, params: serde_json::Value}`,
     `Reply {id: Option<u64>, ok: bool, data?, error?: ErrorBody {code: ErrorCode,
     message}}`, and `EventLine {event: &'static str or String, data: Value}`.
   - `State` and `ErrorCode` enums are snake_case. Every method params type and reply
     data type is typed exactly as listed under "Stage-a methods", and the events as
     listed under "Stage-a events", with `Option` fields skipped when `None`.
   - The `Out` type is what `Sync` hands the daemon:
     ```rust
     pub enum Out { Broadcast(EventLine), Reply { rid: u64, result: Result<Value, ErrorBody> } }
     ```
   - Conversions:
     - `Conversation::from_phone(&phone::Conversation)` and
       `Conversation::from_store(&store::Conversation)`;
     - `Message::from_phone(conversation: &str, &phone::Message, pending: bool)`;
     - `folder` `None` maps to `"inbox"`.
   - Tests:
     - round trip of each envelope;
     - `bad_request` for a non-JSON line;
     - the secrets test: build a `phone::Message` whose `media` entries carry marker
       keys (`"SECRET-KEY-1"`…), and a `phone::Conversation`, serialize every DTO built
       from them, and assert that no marker appears.
   - Verify:
     ```bash
     nix develop -c cargo test --bin gmessages-tui proto   # > 0 tests
     ```
   - Traps: never `#[derive(Serialize)]` on, or `#[serde(flatten)]` of, any `phone::`
     type.

5. **Remove the TUI and port `Sync`.** Coder: codex.
   - Files: `Cargo.toml`, `Cargo.lock`, `src/main.rs`, `src/cli.rs`, `src/pair.rs`,
     `src/web.rs`, `src/store.rs` (Attachment only), `src/signin/native.rs` (its
     `pair::say` call only), new `src/paths.rs`, new `src/sync.rs`, `nix/package.nix`
     (outputHashes).
   - `Cargo.toml`:
     - drop `chat-tui-core`, `crossterm` and `rpassword`;
     - add `dirs` (at the version in the lock) and `qrcode = { version = "0.14",
       default-features = false, features = ["svg"] }`;
     - then `nix develop -c cargo update -w`. Check that
       `git diff Cargo.lock | grep -E '^\+(name|version)'` prints nothing (removals
       only).
   - `src/paths.rs`: `data()`, `cache()` and `runtime()` as decided, plus
     `create_private_dir` (moved from `main.rs:872`).
   - `src/main.rs`:
     - delete `run`, `events`, `GMessages`, `Waiting`, `answer`, `discussion`, `message`,
       `attachment`, `stored_discussion`, `folder`, `unpair`, `uninstall`,
       `unpair_session`, `cache_dir`, `start_pairing` and every `chat_tui` import;
     - add `mod paths; mod sync;`;
     - `main()` dispatches `Serve` (prints "serve: not yet" and exits 2 until step 6),
       `Version` and `InternalSignin`;
     - its tests (`main.rs:1218-1321`) move to `sync.rs`, ported.
   - `src/cli.rs`: `Command::{Serve, Version, InternalSignin}`. Its tests are updated.
   - `src/sync.rs`: `Sync` ported from `main.rs:370-573`, with `ChatId`/`MessageId` as
     `String`. It keeps the `known`, `deleted` and `old` checks of `live`
     (`main.rs:460-496`), the `fetching`, `looked_up` and `read` sets, and the missed
     logic. API:
     ```rust
     pub fn new(store: Rc<Store>, commands: phone::Commands) -> Self
     pub fn handle(&mut self, event: phone::Event) -> Vec<proto::Out>
     pub fn send(&mut self, rid: u64, conversation: String, text: String, reply_to: Option<String>)   // phone::Command::Send with id = rid
     pub fn read(&mut self, conversation: &str) -> Vec<proto::Out>       // Command::Read{up_to: last stored id}, store.set_unread(false), emits unread if changed
     pub fn history(&self, conversation: &str) -> proto::HistoryData     // store, last 200, truncated
     pub fn conversations(&self, folder, offset, limit) -> proto::ConversationsData
     pub fn set_focus(&mut self, conn: u64, conversation: Option<String>)
     pub fn drop_conn(&mut self, conn: u64)
     pub fn unread(&self) -> i64
     ```
     `handle` maps:

     | `phone::Event` | Output |
     |----------------|--------|
     | `Login` | state `login` |
     | `Qr{url}` | `pairing{qr_svg}` (SVG rendered with `qrcode`) |
     | `Emoji` | `pairing{emoji}` |
     | `PairFailed` | `pairing{message}` and state `login` |
     | `Paired` | state `connecting` |
     | `Connected` | state `connecting` |
     | `Ready` | store, then `conversation`s, then state `ready` and `unread` |
     | `History` | `history`, then `missed` once fetching is empty |
     | `Message` | `message` with `notify` per the rules |
     | `Conversation` | `conversation` and `unread` if it changed |
     | `Gone` | `conversation` (hidden) |
     | `Typing` | `typing` |
     | `Sent{id, outcome}` | `Reply{rid: id}`:<br>`Sent` → `{status: "sent"}` and broadcast `message`<br>`Accepted` → `{status: "accepted"}`, broadcast `message{pending: true}` and `Command::Fetch([conv])`<br>`Unavailable` → `phone_offline`<br>`Rejected` → `send_rejected`<br>`Uncertain` → `send_uncertain` |
     | `Expired` | state `expired` |
     | `Unpaired` | state `login`, detail `unpaired_remotely` |
     | `Fatal` | state `fatal` |
     | `Status` | `tracing::info!` (no event in stage a) |

     The `Done`, `Downloaded`, `Contacts` and `Started` events are logged and ignored in
     stage a.
   - `src/pair.rs`:
     - delete `read_terminal`, `link`, `say`, the crossterm imports and the terminal
       thread and stop flag in `ManualPairing`;
     - keep `parse`, `curl_cookies` and `pairs` (the localhost page accepts a pasted cURL
       command);
     - `ManualPairing::start()` takes no intro and exposes `url()`;
     - `Pairing::start(manual)` no longer prints anything;
     - keep the existing cookie tests.
   - `src/signin/native.rs`: remove its `pair::say` instruction text; nothing else
     changes.
   - `src/web.rs`: `Page` stores the server `JoinHandle`. `Drop` aborts it unless
     `done()` was called (then the server lingers `LINGER` and ends by itself, as
     today). Test: drop without `done()`, yield, and the port refuses connections.
   - `src/store.rs:9`: a local `pub struct Attachment` with the same fields as the
     chat-tui one, deriving `Debug`, `Clone` and `PartialEq`.
   - `nix/package.nix`: remove the `outputHashes` entries for crates no longer in the
     lock.
   - Tests in `sync.rs` (with `Commands::detached()` and a temp store, plus the ported
     `main.rs` tests):
     - a live new message from someone else notifies;
     - an update of a known message doesn't;
     - a deleted message doesn't;
     - from-me doesn't;
     - `old` replay doesn't, and one `missed{count}` follows once fetches finish;
     - a focused conversation doesn't, and focus clears after `drop_conn`;
     - `read` emits `unread` with the lower count;
     - each `SendOutcome` maps to the reply above, and `Accepted` emits a `Fetch`
       command.
   - Verify:
     ```bash
     nix develop -c cargo build
     nix develop -c cargo test --workspace            # sync tests > 0
     grep -rn 'chat_tui\|crossterm\|rpassword' src Cargo.toml   # no output
     ```
     Then *(Claude)*:
     ```bash
     nix build .#gmessages-tui
     ```

6. **The daemon.** Coder: codex.
   - Files: new `src/serve.rs`, `src/main.rs` (the `Serve` arm, removing
     `#![allow(dead_code)]`), new `tests/serve.rs`.
   - `serve::run() -> ExitCode`, in the existing `runtime()` and a `LocalSet`.
     - Startup:
       - lock with pid;
       - socket: stale removal, bind 0600, `SO_PEERCRED` uid check through
         `UnixStream::peer_cred`;
       - the key through `spawn_blocking(key::store_key)`, mapping `KeyError` to the
         `locked`/`keyring_missing` states, with a 30 s retry timer and `key.retry`;
       - the store (an open failure is state `store_unreadable` with no retry);
       - `phone::Phone::start(session)`, then the event pump into `Sync::handle`.
     - Clients: a reader task (lines up to 1 MiB, else close) and a writer task fed by a
       bounded queue of 1000 (`try_send` failing closes that client). There is a
       handshake gate, a `(conn, id) → rid` map, a deadline timer per rid replying
       `timeout`, and every `Out::Broadcast` goes to every handshaken client.
     - Methods:
       - `state`, `key.retry`, `conversations`, `history`, `read`, `focus` go straight
         to `Sync`;
       - `send` goes to `Sync::send(rid, …)`, with the reply from `Out::Reply`;
       - `pair.start`:
         - `qr` sends `Command::PairQr`;
         - `google` runs `pair::Pairing::start(false)`, falling back to manual when the
           window is unavailable; its `Pasted::Cookies` goes to `Command::PairGoogle`;
         - `manual` runs `Pairing::start(true)` and broadcasts
           `pairing{manual_url}`;
       - `pair.cancel` sends `Command::CancelPairing` and drops the pairing.
       - Methods other than `hello`/`state` while not `ready` answer `not_paired` or
         `locked` as fitting.
     - Signals: `tokio::signal::unix` SIGTERM/SIGINT, then cleanup as decided, within
       5 s (`Control::stop` under a timeout).
     - `phone.log` is cut to its last 1 MiB before `log::init`.
   - `tests/serve.rs` spawns `env!("CARGO_BIN_EXE_gmessages-tui")` with `serve`, temp
     `XDG_DATA_HOME`, `XDG_CACHE_HOME` and `XDG_RUNTIME_DIR` (0700), and
     `GMESSAGES_TUI_PASSPHRASE=test`. Checks:
     - hello arrives with `protocol: 1`, and state reaches `login`;
     - the client's hello is accepted; with `protocol: 2`, `protocol_mismatch` comes
       back and a later `state` is refused;
     - a garbage line gets `bad_request`, and `state` still works after it;
     - a line over 1 MiB closes the connection, and a new one works;
     - `send` while `login` gets `not_paired`;
     - `pair.cancel` while `login` is `{}`;
     - `conversations {folder: "inbox", offset: 0, limit: 50}` gives
       `{items: [], total: 0}`;
     - a second `serve` exits 1 with "already running";
     - `kill -9`, then a new `serve` reaches hello (stale socket);
     - SIGTERM: exit within 5 s, the socket is gone and `media/` is empty.
   - Verify:
     ```bash
     nix develop -c cargo test --test serve       # > 0 tests
     nix develop -c cargo test --workspace
     ```
     Then *(Claude)*:
     ```bash
     nix build .#gmessages-tui
     ```
   - Traps:
     - Tests must never reach the real keyring: always set the env passphrase.
     - Never log request params.
     - `pair::Pairing` (the sign-in window) needs a display. Tests never call
       `pair.start google`.

7. **The QML plugin.** Coder: agy.
   - Files: new `plugin/manifest.json`, `plugin/Client.qml`, `plugin/Service.qml`,
     `plugin/BarWidget.qml`, `plugin/App.qml`, `plugin/ConversationList.qml`,
     `plugin/Thread.qml`, `plugin/Composer.qml`, `plugin/PairView.qml`,
     `plugin/HelpOverlay.qml`, `plugin/Theme.qml`, `plugin/Errors.js`,
     `plugin/README.md`.
   - Interfaces it may rely on (copied here from the shell on p620; this list is
     authoritative):
     - `import qs.Commons` gives these singletons:
       - `Style`: `Style.cornerRadius`, `Style.space(n)`,
         `Style.spacing.controlHeight`, `Style.spacing.controlGap`,
         `Style.font.{family, caption, bodySmall, body, subtitle, title}`,
         `Style.bar.sizeHorizontal`;
       - `Color`: `Color.{foreground, background, accent, urgent, muted}`,
         `Color.popups.{background, text, border}`, `Color.bar.{background, text,
         active}`;
       - `I18n`: `I18n.t(key, fallback)`, `I18n.tn(key, count, fallback)`;
       - `Util`: `Util.alpha(color, a)`.
     - `import qs.Ui` gives `BarWidget` (the base type for the bar widget; set
       `moduleName: "olafkfreund.gmessages"`; it has `bar`, `settings`, `vertical`),
       `TextField`, `ConfirmDialog`, `BarIconButton` and `PanelToolTip`.
     - `import Quickshell.Io`:
       - `Socket { path; connected; parser: SplitParser { splitMarker: "\n"; onRead:
         function(data) {…} }; write(str); flush() }`;
       - `Process { command; running; stdout: StdioCollector { onStreamFinished } }`;
       - `IpcHandler { target: "olafkfreund.gmessages"; function toggle() {}; function
         open(conversation: string) {} }`.
     - `import Quickshell` gives `FloatingWindow`, `Quickshell.env(name)` and
       `Quickshell.execDetached([…])`.
     - Entry points get `property var shell` and `property var manifest`, as in
       meetingai's `Service.qml`. The panel window is shown and hidden from `Service.qml`
       through the IPC handler, not through the shell.

     For style only, read meetingai's `Client.qml`, `Theme.qml`, `BarWidget.qml`,
     `Service.qml` and `UiText.qml` under `~/.config/omarchy/plugins/nixarchy.meetingai/`
     (a symlink). If they're missing, the interfaces above are enough.
   - `manifest.json`: `schemaVersion: 1`, id, name "Messages", version "0.1.0", license
     "AGPL-3.0-only", kinds `["service","bar-widget","panel"]`, `entryPoints {service:
     "Service.qml", barWidget: "BarWidget.qml", panel: "App.qml"}`, and `barWidget
     {displayName: "Messages", category: "Communication", allowMultiple: false,
     defaultSection: "right"}`.
   - `Client.qml` (each entry point makes its own):
     - the socket path is `Quickshell.env("XDG_RUNTIME_DIR") + "/gmessages-tui/daemon.sock"`;
     - on connect it sends hello with its `role`; `protocol !== 1` sets `mismatch`;
     - `send(method, params, cb)` keeps id → `{cb, timer}`, with a 20 s timeout (125 s for
       `send`) that calls `cb({ok:false, error:{code:"timeout"}})`;
     - reconnect backoff is 0.5 s doubling to 10 s;
     - it exposes `state`, `detail`, `unread`, `connected` and `mismatch`, plus the
       signals `pairing(data)`, `conversationChanged(c)`, `historyArrived(conv, msgs)`,
       `messageArrived(conv, msg, notify)`, `missed(count)` and `typing(…)`.
   - `Service.qml`:
     - one Client (role `service`);
     - on `messageArrived` with `notify`, it runs `["@notifySend@", "-a", "Messages",
       "-A", "open=Open", "--", sender, text]` as a `Process`, with at most 3 running
       (more are dropped and counted into the next `missed` text);
     - on `missed`, one notification "N new messages";
     - when stdout is `open`, the window shows on that conversation;
     - `IpcHandler` `toggle`/`open` owns one `App` instance (a `Loader`), and
       `App.visible` is toggled.
   - `BarWidget.qml`: extends `qs.Ui` `BarWidget`, with its own Client (role `bar`).
     - It shows a glyph plus `unread` when it is above 0.
     - It uses distinct glyphs and a tooltip for `login`, `locked`/`keyring_missing`,
       not connected, and `mismatch`.
     - A click runs `Quickshell.execDetached(["omarchy-shell",
       "olafkfreund.gmessages", "toggle"])`.
   - `App.qml`: a `FloatingWindow` with title "Messages" and app id
     `olafkfreund.gmessages`, and its own Client (role `app`).
     - It shows `PairView` unless the state is `ready`, else `ConversationList` plus
       `Thread`: two columns at 720 px wide or more, one otherwise.
     - It sends `focus {conversation}` when a thread is open and the window is active,
       and `focus {conversation: null}` otherwise.
     - Keys (`Shortcut`, application context): `j`/`k`/Down/Up move, `Enter` opens, `i`
       focuses the composer, `Ctrl+Return` sends, `Esc` goes back (thread → list) or
       hides, `/` filters the list, `?` toggles `HelpOverlay`, and `q` hides.
   - `ConversationList.qml`: pages through `conversations` (`limit` 200) and updates on
     `conversationChanged`. Each row shows name, snippet, a time from
     `Qt.locale(I18n.formatLocale)` and an unread dot.
   - `Thread.qml`:
     - on open, `history`; it merges `historyArrived` and `messageArrived` by `id`, and
       on `historyArrived` drops `pending` messages;
     - opening a thread sends `read`;
     - bubbles use `Style.cornerRadius`, and `deleted`/`failed` messages are styled
       distinctly.
   - `Composer.qml`: a `qs.Ui` `TextField`, multi-line; `Ctrl+Return` sends.
     - `accepted` shows "sending…" on the pending bubble;
     - `send_uncertain` shows "may not have been sent";
     - other errors go through `Errors.js`.
   - `PairView.qml`: state text, the `emoji` big, the `qr_svg` as an `Image` with
     `"data:image/svg+xml;utf8," + encodeURIComponent(svg)`, `manual_url` as a link
     (`Qt.openUrlExternally`), and buttons "Sign in with Google", "QR code" and
     "Manual". A `locked` state shows "Unlock your keyring" with a Retry that sends
     `key.retry`.
   - `Theme.qml`: a plain `QtObject` (as meetingai's) over `Style`/`Color`.
   - `Errors.js`: maps each error code and state detail to `I18n.t("gmessages.<code>",
     "<English>")`.
   - Every visible string goes through `I18n.t`/`I18n.tn` with an English fallback.
   - Verify:
     ```bash
     nix develop -c qmllint --import disable --unresolved-type disable \
       --missing-type disable --missing-property disable --unqualified disable \
       plugin/*.qml   # exit 0: parses
     grep -rn 'console.log' plugin/                # no output
     ```
   - Traps:
     - Never log message text.
     - No `localStorage` and no files: all state comes from the daemon.
     - `@notifySend@` stays a literal in `Service.qml`.
     - Don't name a file after a Qt Quick type.

8. **Nix outputs.** Coder: coder.
   - Files: new `nix/plugin.nix`, new `nix/hm-module.nix`, `flake.nix`, `README.md`.
   - `nix/plugin.nix`: a `stdenvNoCC.mkDerivation` that copies `plugin/` to `$out`, and
     `substituteInPlace $out/Service.qml --replace-fail @notifySend@
     ${libnotify}/bin/notify-send`. Its `doInstallCheck` runs the step 7 `qmllint` line
     on `$out/*.qml`, and fails if `grep -r @notifySend@ $out` matches.
   - `nix/hm-module.nix`: `services.gmessages.enable` (`mkEnableOption`) and `package`
     (`default` from this flake through the overlay). It sets `home.packages = [ cfg.package
     ]` and `systemd.user.services.gmessages-tui`:
     ```nix
     Unit = { Description = "Google Messages backend"; PartOf = [ "graphical-session.target" ]; After = [ "graphical-session.target" ]; };
     Service = { ExecStart = "${cfg.package}/bin/gmessages-tui serve"; RuntimeDirectory = "gmessages-tui"; RuntimeDirectoryMode = "0700"; Restart = "on-failure"; RestartSec = 5; };
     Install.WantedBy = [ "graphical-session.target" ];
     ```
   - `flake.nix` adds:
     - `packages.x86_64-linux.plugin`;
     - `overlays.default` (`gmessages-tui`, `gmessages-plugin`);
     - `homeManagerModules.default`;
     - `checks.x86_64-linux = { gmessages-tui = …; plugin = …; }`.

     No new inputs.
   - `README.md`: a new top section "Omarchy plugin (Nix)", which covers what it is, the
     flake outputs, the home-manager snippet, the p620-style host module snippet, and
     where state lives. The TUI usage sections are replaced by one line saying the TUI
     isn't built in this fork.
   - Verify *(Claude)*:
     ```bash
     nix flake check
     nix build .#plugin
     grep -c notify-send result/Service.qml        # >= 1
     ! grep -r @notifySend@ result/
     ```

9. **Install and test on p620.** Implementer: Claude, with the owner.
   - First read `~/.config/nixos/AGENTS.md` and follow that repo's rules.
   - Files:
     - `~/.config/nixos/flake.nix`: input `gmessages.url =
       "git+file:///mnt/code/Source-home/GitHub/nix-gmsg-tui?ref=feat/nixos-flake"`, with
       `inputs.nixpkgs.follows = "nixpkgs"`;
     - new `hosts/common/nixos/omarchy-gmessages.nix`, modelled on
       `omarchy-omateams.nix`, with `home-manager.users.olafkfreund` importing
       `inputs.gmessages.homeManagerModules.default` and setting
       `services.gmessages.enable = true`,
       `programs.nixarchy.plugins."olafkfreund.gmessages".src =
       inputs.gmessages.packages.x86_64-linux.plugin`, and
       `local.omarchy.enableOnce = [ "olafkfreund.gmessages" ]`;
     - imported from `hosts/p620/nixos/` only.
   - Before switching:
     - post on the agent bus;
     - show the owner the diff and the result of `nixos-rebuild build --flake
       .#p620`;
     - get an explicit OK.
   - After the switch, with the owner:
     1. `systemctl --user is-active gmessages-tui` prints `active`, and
        `systemctl --user show-environment` has `WAYLAND_DISPLAY`.
     2. `omarchy-shell shell listPlugins` shows `olafkfreund.gmessages` enabled, and the
        shell journal shows no QML errors from it.
     3. Pairing:
        - "Sign in with Google" opens the window, the emoji shows, and the phone
          accepts it;
        - if Google refuses the window, "Manual" gives a URL that pairs from the
          browser.
     4. Messages:
        - the list and history load;
        - a message sent from the window reaches the phone;
        - a reply from the phone notifies once, and not while that thread is open and
          focused;
        - the badge counts unread conversations and drops after reading.
     5. Starting the unit again, and booting the machine again, reopen without pairing.
     6. Locking the keyring before the unit starts shows "Unlock your keyring", and
        Retry recovers.
     7. A theme switch and a window-rounding change apply live.
     8. A 1 MiB line reaches `Client.qml` intact (a test history or a crafted event
        through a debug socket client).
     9. `phone.log` holds no message text, cookie or URL: grep for a sent message's
        text.
   - Commit in `~/.config/nixos` only when the owner says so.
   - Verify: every check passes, or each failure is written down with its cause before
     any fix.
   - Traps: never touch p510 or razer.

## Tests

| When | Command | Expected |
|------|---------|----------|
| After each of steps 1–8 | that step's verify, plus clippy and fmt | green; filtered test runs count > 0 |
| After step 8 *(Claude)* | `nix flake check` | all checks pass |
| After step 8 *(Claude)* | `nix run . -- --version` | `v1.3.6` |
| Step 9 | checks 1–9 above | each confirmed with the owner |
| Review | Opus review with only this plan path and `git diff main...feat/nixos-flake`; then Codex and agy, read-only | findings reported; none approves anything |

## Rollback

- In this repo, each step is its own commit on `feat/nixos-flake`: revert that commit.
  `main` is untouched until a PR merges.
- On p620: remove the import of `omarchy-gmessages.nix` and the `gmessages` input, then
  `nixos-rebuild switch` with the owner's OK, or boot the previous generation. The state
  in `~/.local/share/gmessages-tui` stays; deleting it is the owner's call.
