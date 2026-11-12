---
status: approved
issue: none (issues are disabled on the fork)
spec: spec/2026-11-11-omarchy-messages.md
stage: b1 (contact pictures; spec section 6). Media and reactions get their own stage b plan.
coders:
  1: claude  # live probe on the paired phone: needs the session, the unit and the owner's phone
  2: codex   # phone layer: command, event, colour and id choice, timeout and pacing
  3: coder   # store migration v6: two nullable columns, merge rule, tests
  4: codex   # Sync queue, batch tokens, image checks, file lifecycle: small, tricky logic
  5: coder   # QML: Avatar.qml and reactive bindings in four files
reviewers:
  - codex   # independent read of the whole diff against this plan
  - agy     # second independent read; disagreement is the point
---

# Plan: Messages plugin, stage b1: contact pictures

Revised after Codex and agy reviewed the first draft (3da5fb8). The probe now comes
first, before any production code, as the spec requires.

## What is decided (copied from the approved spec, section 6)

Section 6 governs. The stage table in spec section 2 still says `avatar {conversation,
path}`, which section 6 replaced with one `avatars {items}` event per batch.

**What the user sees:**
- A round picture starts each list row (32 px) and the thread header (40 px), both at
  the theme scale.
- With no picture, a circle in the conversation's colour shows the initials of its name:
  the first letter of the first two words, split on code points.
  - A word counts only if it starts with a letter (`/\p{L}/u`).
  - With no such word (a number, a short code), it shows a person icon.
- Groups always get the initials circle. Group pictures are out of scope.

**Which picture:**
- Only 1:1 conversations get one: not a group, the participant that
  `gm::Conversation::other_participant()` (`gmessages-proto/src/types.rs:795`) returns,
  and a non-empty key.
- The key (decided in step 1, owner's choice on 2026-11-12):
  - `c:<contact_id>` when `Participant.contact_id` is set, fetched with action 29
    (`contact_thumbnails`);
  - otherwise `p:<id>`, from `Participant.id`, fetched with action 37
    (`participant_thumbnails`).
  - The prefix says which call fetches it. The phone layer splits each batch by prefix,
    makes the two calls one after the other, and puts the prefix back on the returned
    ids.
- **Colour:** the conversation's `colour`, else the participant's. It is always `#rrggbb`
  on this phone, and is lowercased; anything else becomes `""`.

**Data:**
- **`phone::Conversation`** gains:
  - `colour: String`: `#rrggbb` in lower case, or `""`;
  - `avatar_id: Option<String>`: the chosen key, or `None` for a group or when it's
    unknown.
- **The store's merge rule:** keep the stored value unless the new one is known.
  ```sql
  colour    = COALESCE(NULLIF(?colour, ''), colour)
  avatar_id = CASE WHEN ?is_group THEN NULL ELSE COALESCE(?avatar_id, avatar_id) END
  ```
  - The other person of a 1:1 conversation never changes. A partial phone update with
    no participants must not erase what's known.
  - A group never has an avatar_id.
- **`Sync` keeps, in memory:**
  - `meta`: a map from conversation id to `(colour, Option<avatar_id>)`, loaded from the
    store at start and updated with the same merge rule on every phone conversation;
  - `avatars`: a map from avatar_id to path;
  - `queue`: a `VecDeque` of avatar_ids;
  - `tried`: a set;
  - `failed_once`: a set;
  - `in_flight`: `Option<(u64 token, Vec<String> ids)>`;
  - `next_token`: a `u64`;
  - `ready`: a bool;
  - `avatar_dir`: a `PathBuf`.
- **Outgoing conversations:** every conversation the daemon sends gets `color` and
  `avatar` from `meta` and `avatars`. That covers `conversations` replies, `conversation`
  events from the phone or the store, and the `Gone` path at `src/sync.rs:382`. A
  partial update never blanks a known colour or picture.

**Fetching:**
- **Queueing:** an avatar_id is queued when it isn't already queued, in flight, in
  `tried` or in `avatars`. It comes from:
  - every item of a `conversations` reply;
  - the `phone::Event::Conversation` arm (`src/sync.rs:344`), when `ready`.
- **Never queued:** the shared helper `Sync::conversation()`. `Event::Ready` calls it in
  bulk, twice when the first page is `partial`.
- **`pump()`:** when `ready`, nothing is in flight and the queue isn't empty, it takes up
  to 50 ids and sends `Command::Avatars { id: token, ids }` with a fresh token per batch.
  It runs after queueing, on `Event::Ready`, and after each result.
- **The phone layer:**
  - runs the call with `spawn_local` under `tokio::time::timeout(30 s)`;
  - emits `Event::Avatars { id, images: Vec<(String, Vec<u8>)>, error: Option<String> }`
    no sooner than 2 s after the call started, using `tokio::time::Instant`. That gap is
    the pacing.
- **Results:** a result whose token isn't the one in flight is dropped. Otherwise, for
  the ids in flight:
  - an image that passes the checks gets written, and its path recorded;
  - an id that's missing from the result, has an empty image, or fails the checks goes
    into `tried`;
  - an entry for an id that wasn't requested is ignored;
  - on `error`, every id in flight goes back into the queue once (`failed_once`), then
    into `tried` on a second failure.
  - The log has counts only, never ids.
- **Losing `ready`:** on any phone event other than `Ready` that sets a non-ready state
  (`Login`, `Paired`, `Connected`, `Expired`, `Unpaired`, `Fatal`):
  - clear `ready`;
  - put the ids in flight back at the front of the queue;
  - set `in_flight` to `None`, so the late result's token no longer matches.
- **`Paired` and `Unpaired`** (a new or different account) also empty `avatars`,
  `queue`, `tried` and `failed_once`, and clear `avatar_dir`.
- **Image checks:** an image is kept only when:
  - its magic bytes are JPEG (`FF D8 FF`), PNG (`89 50 4E 47 0D 0A 1A 0A`) or WebP
    (`RIFF` at 0 and `WEBP` at 8);
  - it is at most 256 KiB;
  - `imagesize::blob_size` gives at most 1024 px per side.
- **Files:**
  - Path: `avatar_dir/<sha256 hex of the image>.<jpg|png|webp>`. The directory is 0700.
  - If that file already exists, it is reused.
  - Otherwise it's written to `.<name>.tmp`, opened with `create_new` and
    `OpenOptionsExt::mode(0o600)`, after removing any leftover. Then it's renamed.
  - The daemon builds every path itself.
- **Lifecycle:** `runtime/avatars` is cleared:
  - at start and stop, along with `runtime/media`;
  - on `Paired` and `Unpaired`;
  - on `unpair` and `uninstall`, which are stage c.

**Protocol:**
- Conversation items gain `color` (`#rrggbb` or `""`) and `avatar` (an absolute path or
  `null`).
- New event `avatars {items: {<conversation id>: <path>}}`, at most one per batch:
  - It lists the conversations whose avatar_id got a path in that batch, found through
    `meta`.
  - It's skipped when empty.
- The protocol stays at version 1. The no-secrets test covers the new fields.

**Plugin:**
- **`Avatar.qml`:**
  - Properties:
    - `required property var theme`;
    - `property string path`, `color` and `name`;
    - `property bool group`;
    - `property real size`.
  - When `group` is false and `path` isn't empty: an `Image` with
    `source: "file://" + path`, `asynchronous: true` and `sourceSize` set to the size,
    inside a `ClippingRectangle` (`Quickshell.Widgets`) with radius size/2.
  - Otherwise, or on `Image.Error`: the initials circle. Its fill is `color`, or
    `Commons.Color.surface` when that's empty, and its text colour is chosen for contrast.
- **`ConversationList.qml`:**
  - `property var avatars: ({})`, from conversation id to path, separate from the rows.
    Item paths, from pages and upserts, and the `avatars` event write to it, each with
    one assignment of a new object.
  - The map is emptied when the client state becomes `login`.
  - `readonly property var selectedItem`, the `allItems` entry for `selectedId`, or
    `null`.
  - `upsertConversation` keeps `color` when an update's is empty, as it already does
    for `snippet`.
- **`App.qml`:** binds the thread header reactively:
  - `conversationAvatar: convList.avatars[root.activeConversation] || ""`;
  - `conversationColor` and `conversationGroup` from `convList.selectedItem`.
  - This way a picture that arrives late, or a thread opened from a notification, updates
    on its own. `conversationSelected` keeps its signature.

## Rules for every step

- **Coders (codex, coder):**
  - Edit only the files their step lists.
  - May run, inside `nix develop -c …`: `cargo build`, `cargo test`, `cargo clippy`,
    `cargo fmt` and `qmllint`. In step 4 only, they may also run
    `cargo add imagesize sha2@0.10 --package gmessages-tui`. `sha2` 0.10.9 is already
    in `Cargo.lock`.
  - Never run `nix build` or `nix flake check`. Never stage, commit, push, deploy or
    switch, never touch services, and never touch `~/.config/nixos`, `main` or another
    host.
- **Claude (session):** runs step 1 and every line marked *(Claude)*. It reviews each
  step's diff against this plan, and commits each step as its own commit naming the step.
  A coder's reported deviation is written into this plan in the same commit.
- **A step is done when:** its verify passes,
  `nix develop -c cargo clippy --workspace -- -D warnings` is clean, and
  `nix develop -c cargo fmt --check` passes.
- **Test-count guard:** every `cargo test <filter>` verify must report a non-zero number
  of tests run.
- **Repo traps:**
  - The runtime is single-threaded (`LocalSet`, `Rc`), so async code uses `spawn_local`
    and never needs `Send`. `phone.rs:10` imports `std::time::Instant`; the pacing uses
    `tokio::time::Instant`.
  - `phone` and `store` carry `#[allow(dead_code)]` at module level (`main.rs:9`,
    `main.rs:17`). `sync` and `proto` don't, so anything new there must be used by the
    end of step 4.
  - `phone::Conversation` is `Serialize`/`Deserialize`, so new fields get
    `#[serde(default)]`. The `src/sync.rs:578` fixture builds it from JSON and needs no
    edit.
  - Exhaustive literals:
    - `phone::Conversation` at `src/proto.rs:493`;
    - `store::Conversation` at `src/proto.rs:511`, and the two constructors at
      `src/store.rs:150` and `src/store.rs:275` (`conversations_hidden`).
  - The plugin ships under `v-<hash>/` (`nix/plugin.nix`). Shell QML errors show only in
    the journal, and parse-only qmllint catches neither bindings nor types, so check
    after a deploy:
    ```sh
    journalctl --user --since -5min | grep -i gmessages
    ```
  - Quickshell's `FloatingWindow` has no `active` and no `appId`.
  - A shell hook blocks commands whose text mentions restarting services or rebooting.
    The unit stop and start go in a script file.

## Steps

1. **Probe: which action, and what `colour` looks like** *(Claude, with the owner)*
   - **Before the probe:**
     - The working tree is clean.
     - The probe lives in one patch file, `scratchpad/probe-avatars.patch`, touching only
       `src/phone.rs`.
     - In `list_conversations`, after the final `Event::Ready` (`src/phone.rs:905`), it
       spawns a task that takes up to 50 1:1 conversations from `listed`.
   - **What the task does:**
     1. Calls `participant_thumbnails` with their `Participant.id`s.
     2. Waits 2 s.
     3. Calls `contact_thumbnails` with their non-empty `contact_id`s, each under a 30 s
        timeout.
     4. Logs one info line per call with: ids asked, entries returned, non-empty images,
        returned ids that weren't requested, image formats by magic bytes, the largest
        byte size, and whether the call errored.
     5. Logs one line counting `Conversation.colour` and `Participant.colour` values by
        shape: empty, `#`+6, 6, `#`+8, 8, or other.
     6. Writes the line `probe done` to stderr.
   - **The run** is a script file, `scratchpad/probe-avatars.sh`, under a `trap` that
     always kills the probe with SIGTERM and starts the unit again:
     1. stop the `gmessages-tui` user unit;
     2. `nix develop -c cargo run -- serve` in the foreground;
     3. wait for `probe done`, or 120 s;
     4. SIGTERM, so `control.stop()` saves the session;
     5. start the unit;
     6. print only the probe lines.
   - **After the run:**
     - Revert with `git apply -R scratchpad/probe-avatars.patch`, then check that
       `git status --porcelain` is empty.
     - Record the counts and the decision under "Probe result" below, in a plan-only
       commit: which action and key, and the `colour` rule. For 8-digit values, drop the
       ARGB alpha. When the conversation colour is empty, use the participant's.
   - **If both actions return zero images:**
     - Before deciding, ask the owner whether any of the sampled contacts has a picture
       on the phone.
     - If they do, stop and ask.
     - If not, the plan shrinks to colour and initials: step 2 drops the command and
       event, step 4 drops fetching, and step 5 is unchanged.
   - Verify: the probe result is committed, and the tree is otherwise clean.

2. **Phone layer** (`src/phone.rs`, plus the literal at `src/proto.rs:493`).
   *coder: codex*
   - `Conversation` (`:207`) gains `#[serde(default)] colour: String` and
     `#[serde(default)] avatar_id: Option<String>`.
   - `conversation()` (`:636`) fills them through two private functions:
     - `colour(&gm::Conversation) -> String`, with the rule under "Colour";
     - `avatar_id(&gm::Conversation) -> Option<String>`, the prefixed key. It returns
       `None` for a group, when there's no participant, or for an empty key.
   - `Command` (`:128`) gains `Avatars { id: u64, ids: Vec<String> }`. Its handler, next
     to `Download` (`:1015`), does what "Fetching" describes. It splits `ids` by prefix:
     `c:` goes to `contact_thumbnails` and `p:` to `participant_thumbnails`, one call after
     the other, inside one 30 s timeout. The returned ids get their prefix back. If either
     call fails, `error` is set, and the images from the other call are still returned.
   - `Event` (near `:49`) gains `Avatars { id, images, error }`. `src/sync.rs:424` lists
     it with the ignored stage-b events until step 4. That is the one allowed edit in
     `sync.rs`.
   - **Tests:**
     - `colour`: `"1A73E8"`, `"#1a73e8"`, 
       `""`, `"blue"` and `"#12345"`; and the participant fallback;
     - `avatar_id`: a group, no participants, an empty key, `is_me` only, a hidden
       participant, and the normal case.
   - Verify: `nix develop -c cargo test phone::` (count > 0), clippy and fmt.

3. **Store** (`src/store.rs`, plus the literal at `src/proto.rs:511`). *coder: coder*
   - **Migration:** set `SCHEMA` to 6. Add a `version < 6` block in a transaction, the
     same way as v5 (`:120`), adding the nullable columns `colour TEXT` and
     `avatar_id TEXT`.
   - **Saving:** `save_conversations` (`:184`) writes both columns, with the merge rule
     from "Data".
   - **Reading:** `store::Conversation` (`:33`) gains `colour: String` and
     `avatar_id: Option<String>`. Both readers, `conversations_with` (`:142`) and
     `conversations_hidden` (`:268`), select them, with `colour` defaulting to `""`.
   - **Fix existing tests:**
     - the assertion at `:479` expects 6;
     - the "newer version" test at `:487` sets 7.
   - **New tests:**
     - v5 to v6 keeps the conversations;
     - both fields round-trip;
     - a later 1:1 save with `avatar_id: None` and `colour: ""` keeps both;
     - a group save clears `avatar_id`.
   - Verify: `nix develop -c cargo test store::` (count > 0), clippy and fmt.

4. **Sync, protocol and serve** (`src/sync.rs`, `src/proto.rs`, `src/serve.rs`,
   `Cargo.toml`, `Cargo.lock`). *coder: codex*
   - **`proto.rs`:**
     - `Conversation` (`:137`) gains `color: String` and `avatar: Option<String>`.
     - `from_phone` and `from_store` take a second argument, `(color: String,
       avatar: Option<String>)`.
     - Add `AvatarsEvent { items: BTreeMap<String, String> }`.
     - The no-secrets test fills the new fields.
   - **`sync.rs`:**
     - Add the state from "Data", and `Sync::new(store, commands, avatar_dir)`, which
       loads `meta` from the store.
     - Implement "Fetching" exactly as written: `enqueue`, `pump`, the
       `Event::Conversation` queueing, losing `ready`, `Paired`/`Unpaired`, results,
       image checks and files.
     - `conversations()` becomes `&mut self`.
     - Every `from_phone`/`from_store` call site passes `color` and `avatar`. That
       includes `conversation()`, the `Gone` path (`:382`) and the tests.
   - **`serve.rs`:**
     - Create `runtime/avatars` (0700) next to `media` (`:115`).
     - Clear it at start and stop with `clear_media` (`:877`, reused at `:116` and
       `:125`).
     - Thread the path through `serve_bound` and `Daemon` to the delayed `Sync::new`
       (`:270`).
     - The `"conversations"` arm (`:661`) uses `as_mut()`.
   - **Tests** in `sync.rs`:
     - color and avatar reach every outgoing conversation:
       - a `conversations` reply;
       - a live `conversation` event;
       - a bulk `Ready`;
       - `Gone`;
       - a partial update that keeps a known colour and picture.
     - queueing skips duplicates, ids in flight, `tried` and `avatars`, and a second
       `partial: false` Ready queues nothing;
     - one batch in flight, at most 50 ids, a fresh token per batch;
     - a bad magic, a 300 KiB file, and a 40-byte PNG whose header says 20000×20000 all
       go to `tried`;
     - a missing id goes to `tried`; an unrequested id is ignored;
     - a failed batch requeues once, then the ids go to `tried`;
     - after `Connected`, the ids in flight are requeued and the late result is dropped;
     - after `Paired`, `Unpaired` and a second pairing, everything is reset and the
       directory is empty;
     - files are 0600 and named by their content, and the same image twice gives one
       file;
     - at most one `avatars` event per batch, and none when empty.
   - Verify: `nix develop -c cargo test` (all), clippy and fmt.

5. **Plugin** (`plugin/Avatar.qml` new, `plugin/Client.qml`,
   `plugin/ConversationList.qml`, `plugin/Thread.qml`, `plugin/App.qml`).
   *coder: coder*
   - **`Client.qml`:** `signal avatarsArrived(var items)`, and `case "avatars":` in the
     event switch (`:101`).
   - **`Avatar.qml`:** as in "Plugin". The person icon uses the glyph font
     `BarWidget.qml` uses.
   - **`ConversationList.qml`:**
     - the `avatars` map and `selectedItem`;
     - the page merge (`:64`–`:83`) and `upsertConversation` (`:85`) write item paths
       into the map and keep a known `color`;
     - `onAvatarsArrived`;
     - the map is reset on the `login` state;
     - an `Avatar` in the delegate (`:340`) before the unread dot, at
       `size: Style.space(32)`.
   - **`Thread.qml`:**
     - `property string conversationAvatar`, `property string conversationColor` and
       `property bool conversationGroup: false`;
     - an `Avatar` at `size: Style.space(40)` in the header (`:180`);
     - the header's height becomes
       `Math.max(root.theme.ctl + root.theme.padY, Style.space(40) + root.theme.padY)`.
   - **`App.qml`:** the reactive bindings from "Plugin", on the `Thread` at `:238`.
   - Verify:
     - qmllint parse-only on the five files, with the flags `nix/plugin.nix` uses;
     - *(Claude)* `nix build .#plugin`;
     - *(Claude)* qmllint with `-I` on Quickshell's qml dir, on copies with the
       `qmllint disable` lines stripped. Every new warning is read; the known `qs.*`
       noise is ignored.

6. **Build, deploy, check** *(Claude)*
   - `nix flake check`, then push.
   - **Backup:** before the first v6 binary runs on a host, with the unit stopped (script
     file), copy `~/.local/share/gmessages-tui/messages.db` to
     `messages.db.v5-<date>` (0600).
   - In `~/.config/nixos-gmessages`:
     - `nix flake lock --update-input gmessages`;
     - build p620 and razer;
     - check that both closure diffs touch only the gmessages paths;
     - post on the agent bus;
     - switch p620, then razer. The owner asked for both on 2026-11-12.
   - **Checks, with the owner:**
     1. The list shows pictures for contacts that have one on the phone, and initials
        circles for the others and for groups.
     2. The thread header shows the same picture, including one opened before its
        picture arrived, and one opened from a notification.
     3. Scrolling the full list after a start of the unit leaves no row without its
        picture. This covers a picture arriving between pages.
     4. `ls -l $XDG_RUNTIME_DIR/gmessages-tui/avatars`: 0700, with files at 0600.
     5. After the unit stops and starts again (script file), the pictures come back.
     6. `phone.log` has counts only, with no ids.
     7. The journal shows no QML errors.

## Probe result

Run on p620 on 2026-11-12 against the owner's paired phone, with the unit stopped. The
probe was reverted, and the tree was clean afterwards.

| Call | Ids asked | Returned | With an image | Not requested | Largest | Formats |
|------|-----------|----------|---------------|---------------|---------|---------|
| Action 37, `Participant.id` | 50 (1:1) | 1 | 1 | 0 | 16755 B | png |
| Action 29, `Participant.contact_id` | 16 (of those 50, the ones with a contact id) | 8 | 8 | 0 | 24003 B | png |

Colour shapes, across 1:1 conversations:
- conversation colour: 771 `#rrggbb`, 99 empty;
- participant colour: 870 `#rrggbb`, none in any other shape.

**Decision:**
- **Key:** action 29 by `contact_id` first, then action 37 by `Participant.id`. This
  departs from the spec's "action 37 wins": action 37 found 1 picture in 50, action 29
  found 8 in 16. The owner chose it when asked, on 2026-11-12, and the spec's line was
  updated in the same commit.
- **Colour:** the conversation's, falling back to the participant's.

## Tests

| When | Command | Expected |
|------|---------|----------|
| Step 1 *(Claude)* | `scratchpad/probe-avatars.sh` | counts recorded; tree clean |
| Steps 2, 3, 4 | that step's verify, plus clippy and fmt | green; filtered counts > 0 |
| Step 5 *(Claude)* | `nix build .#plugin` and qmllint with `-I` | builds; no new warnings |
| Step 6 *(Claude)* | `nix flake check` | all pass |
| Step 6 | checks 1–7 | confirmed with the owner |
| Review | Opus review with only this plan path and `git diff <plan approval>..HEAD`; then Codex and agy, read-only | findings reported; none approves anything |

## Rollback

- In this repo, each step is its own commit on `feat/nixos-flake`: revert it.
- On a host, v6 can't be undone in place. A v5 binary refuses a v6 store ("store from a
  newer version", `src/store.rs:70`), and dropping the columns still leaves
  `user_version` at 6. To roll back:
  1. stop the unit;
  2. restore `messages.db.v5-<date>` from step 6, which loses messages that arrived
     since, though the phone sends recent history again;
  3. switch the host to the previous generation;
  4. start the unit.
- `runtime/avatars` is on tmpfs and goes away at logout.
