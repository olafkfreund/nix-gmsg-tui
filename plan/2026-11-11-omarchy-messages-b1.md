---
status: draft
issue: none (issues are disabled on the fork)
spec: spec/2026-11-11-omarchy-messages.md
stage: b1 (contact pictures; spec section 6). Media and reactions get their own stage b plan.
coders:
  1: codex   # phone layer: new command and event, colour and id choice, timeout and pacing
  2: claude  # live probe on the paired phone: needs the session and the owner's phone
  3: coder   # store migration v6: two nullable columns, plain SQL and a test
  4: codex   # Sync queue, image checks, file lifecycle, session guard: small, tricky logic
  5: coder   # QML: Avatar.qml and wiring in four files, following the existing patterns
reviewers:
  - codex   # independent read of the whole diff against this plan
  - agy     # second independent read; disagreement is the point
---

# Plan: Messages plugin, stage b1: contact pictures

## What is decided (copied from the approved spec, section 6)

**What the user sees:**
- Each list row has a round picture at its start, 32 px, and the thread header has one
  at 40 px, both at the theme scale.
- A conversation with no picture gets a circle in the phone's colour with the initials of
  its name:
  - The initials are the first letter of the first two words, split on code points.
  - A name that is a number, or has no letters, shows a person icon instead.
- Groups always get the initials circle. Group pictures are out of scope.

**Which picture:**
- Only 1:1 conversations get one:
  - the conversation is not a group;
  - the participant is the one `gm::Conversation::other_participant()` returns
    (`gmessages-proto/src/types.rs:795`);
  - its id is not empty.
- Two phone calls take the same request shape:
  - action 37, `participant_thumbnails`, keyed by `Participant.id`;
  - action 29, `contact_thumbnails`, keyed by `Participant.contact_id`.
- Step 2 probes both against the paired phone. If both answer, action 37 wins, because it
  also covers numbers that aren't in the address book. If neither answers, the feature
  ships as initials only, and the owner is told.

**Data:**
- `phone::Conversation` gains two fields:
  - `colour`: `#rrggbb` (lower case), or `""` when the phone's value isn't six hex digits,
    with or without `#`;
  - `avatar_id`: `Option<String>`, the id chosen above.
- The store's `conversations` table gains the nullable `colour` and `avatar_id` (migration
  v6). Image bytes never go into SQLite.
- `Sync` keeps, in memory:
  - `avatars`: a map from avatar_id to path;
  - `queue`: a deduplicated `VecDeque` of ids;
  - `tried`: a set of ids;
  - `failed_once`: a set of ids;
  - `in_flight`: `Option<u64>`, the batch token;
  - `ready`: a bool.
- Every conversation the daemon sends carries `avatar` from that map: `from_phone`,
  `from_store`, and every `conversations` reply.

**Fetching:**
- **What gets queued:**
  - the avatar_id of each item in a `conversations` reply;
  - the avatar_id of each live `conversation` event while `ready`.
  - `Event::Ready`'s bulk conversations don't queue anything.
  - Ids already in `tried` or `avatars`, or already queued, are skipped.
- **Sending a batch:** up to 50 ids, when `ready`, nothing is in flight, and the queue
  isn't empty. This is checked when entering `ready`, after queueing, and when a batch
  finishes.
  - The phone layer runs the call with `spawn_local`, under a 30 s timeout.
  - It emits the result no sooner than 2 s after the call started. That gap between
    batches is the pacing.
- **Results:**
  - An entry whose id wasn't requested is ignored.
  - A missing entry, or an empty `image`, goes into `tried`.
  - On a failed batch, ids go back into the queue once (`failed_once`). On a second
    failure they go into `tried`.
  - The log line has counts only, never ids.
  - A result whose token isn't `in_flight` is dropped. The token changes on `Unpaired`,
    `Expired`, `Fatal`, `Login` and every new pairing.
- **Image checks:** a result is kept only when:
  - its magic bytes are JPEG (`FF D8 FF`), PNG (`89 50 4E 47 0D 0A 1A 0A`) or WebP
    (`RIFF....WEBP`);
  - it is at most 256 KiB;
  - `imagesize::blob_size` gives at most 1024 px per side.
- **Files:**
  - Path: `runtime/avatars/<hex sha256(image)>.<jpg|png|webp>`. The directory is mode
    0700.
  - Each file is written as a temp file in the same directory, opened with
    `OpenOptionsExt::mode(0o600)` and `create_new`, then renamed.
  - The daemon builds each path itself.
- **Lifecycle:** `runtime/avatars` is cleared along with `runtime/media`:
  - at start and at stop;
  - on `Event::Unpaired`, which also empties `avatars`, `queue`, `tried`, `failed_once`
    and `in_flight`;
  - on `unpair` and `uninstall`, which are stage c.

**Protocol:**
- Conversation items gain:
  - `color`: `#rrggbb` or `""`;
  - `avatar`: an absolute path, or `null`.
- New event: `avatars {items: {<conversation id>: <path>}}`, one per batch. It lists every
  conversation whose avatar_id got a picture.
- The protocol stays at version 1. Stage a clients ignore unknown events and fields.
- The no-secrets test covers the new fields.

**Plugin:**
- **`Avatar.qml`** takes the properties `path`, `color`, `name`, `group` and `size`:
  - An `Image` with `source: path ? "file://" + path : ""`, `asynchronous: true` and
    `sourceSize` set to the size, inside a `ClippingRectangle` (`Quickshell.Widgets`)
    with radius size/2.
  - On `Image.Error`, or when there's no path, it shows the initials circle instead. Its
    text colour is chosen for contrast with `color`, which falls back to the theme's
    surface colour.
- **`ConversationList`:**
  - It keeps `property var avatars: ({})`, from conversation id to path, separate from the
    rows.
  - Items and the `avatars` event write to it. The delegate reads
    `root.avatars[modelData.id]`.
  - One event changes the map once: it builds a new object and assigns it once.
- **`App.qml`:** passes the selected conversation's `avatar`, `color`, `name` and `group`
  to `Thread`'s header, including a thread opened from a notification.

## Rules for every step

- **Coders (codex, agy, coder):**
  - Edit only the files their step lists.
  - May run, inside `nix develop -c …`: `cargo build`, `cargo test`, `cargo clippy`,
    `cargo fmt` and `qmllint`. In step 4 only, they may also run
    `cargo add imagesize --package gmessages-tui`.
  - Never run `nix build` or `nix flake check`. Never stage, commit, push, deploy or
    switch, never touch services, and never touch `~/.config/nixos`, `main` or another
    host.
- **Claude (session):** runs step 2 and every line marked *(Claude)*. It reviews each
  step's diff against this plan, and commits each step as its own commit naming the step.
  A coder's reported deviation is written into this plan in the same commit.
- **A step is done when:** its verify passes,
  `nix develop -c cargo clippy --workspace -- -D warnings` is clean, and
  `nix develop -c cargo fmt --check` passes.
- **Test-count guard:** every `cargo test <filter>` verify must report a non-zero number
  of tests run.
- **Repo traps:**
  - The runtime is single-threaded (`LocalSet`, `Rc`), so async code uses `spawn_local`
    and never needs `Send`.
  - `phone::Conversation` is `Serialize`/`Deserialize`, so new fields get
    `#[serde(default)]`.
  - Every exhaustive `phone::Conversation { … }` literal in tests must gain the new
    fields: `src/proto.rs:493` and `src/sync.rs:578`.
  - The plugin ships under `v-<hash>/` (`nix/plugin.nix`). Shell QML errors show only
    in the journal, so after a deploy check
    `journalctl --user --since -5min | grep -i gmessages`.
  - QuickShell's `FloatingWindow` has no `active` and no `appId`.
  - A shell hook blocks commands whose text mentions restarting services or rebooting.
    Use a script file for the unit stop and start.

## Steps

1. **Phone layer** (`src/phone.rs`). *coder: codex*
   - **Fields:**
     - `Conversation` (`:207`) gains `#[serde(default)] pub colour: String` and
       `#[serde(default)] pub avatar_id: Option<String>`.
     - `conversation()` (`:636`) fills them with two new private functions,
       `colour(&str) -> String` and `avatar_id(&gm::Conversation) -> Option<String>`.
     - For now `avatar_id` uses `other_participant()`'s `id`, the action 37 key. A
       constant `AVATAR_BY_CONTACT: bool = false` switches it to `contact_id`.
   - **Command:**
     - `Command` (`:128`) gains `Avatars { id: u64, ids: Vec<String> }`.
     - The handler (near `:1015`, the `Download` arm) runs
       `client.participant_thumbnails(&ids)`, or `contact_thumbnails` when
       `AVATAR_BY_CONTACT` is set, inside `tokio::time::timeout(30 s)`.
     - It then sleeps until 2 s after the start, and emits
       `Event::Avatars { id, images: Vec<(String, Vec<u8>)>, error: Option<String> }`.
   - **Event:** add `Avatars` to `Event` (near `:49`). `src/sync.rs:424` ignores it until
     step 4.
   - **Tests** in `phone.rs`:
     - `colour`: `"1A73E8"`, `"#1a73e8"`, `""`, `"blue"` and `"#12345"`;
     - `avatar_id`: a group, an empty id, `is_me` only, a hidden participant, and the
       normal case.
   - **Test literals:** `src/proto.rs:493` and `src/sync.rs:578` get the new fields.
     Those are the only edits outside `phone.rs`.
   - Verify: `nix develop -c cargo test phone::` (count > 0), clippy and fmt.

2. **Probe: which action works** *(Claude, with the owner)*
   - Temporary, never committed: in `Sync`'s `Event::Ready` arm, on the first ready only,
     send `Command::Avatars` for up to 10 avatar_ids from `conversations`. Then do the
     same with `AVATAR_BY_CONTACT` flipped, as a second build. Log at info level the
     number of ids asked, of non-empty images, and of errors.
   - Run it with a script file (`scratchpad/probe-avatars.sh`):
     1. stop the `gmessages-tui` user unit;
     2. run the debug binary in the foreground with `serve` until `ready`, plus 10 s;
     3. stop it;
     4. start the unit again.
   - Read only the count lines from `phone.log` and stdout.
   - Revert the probe with `git checkout -- src/`. Set `AVATAR_BY_CONTACT` from the
     result, and record the result in this plan under "Probe result", in the step 3
     commit.
   - If both counts are zero, stop and ask the owner before step 3.
   - Verify: the counts are recorded, and `git status` shows only the plan edit.

3. **Store** (`src/store.rs`). *coder: coder*
   - **Migration:** set `SCHEMA` to 6. Add a `version < 6` block in a transaction, the
     same way as v5 (`:120`): `ALTER TABLE conversations ADD COLUMN colour TEXT;` and
     `ALTER TABLE conversations ADD COLUMN avatar_id TEXT;`.
   - **Saving:** `save_conversations` (the `INSERT … ON CONFLICT` at `:184`) writes both
     columns. On conflict, it keeps the stored value when the new one is `NULL`.
   - **Reading:** `store::Conversation` (`:33`) gains `colour: String` and
     `avatar_id: Option<String>`. `conversations_with` (`:142`) reads them, with
     `colour` defaulting to `""`.
   - **Tests:**
     - migrating v5 to v6 keeps the conversations;
     - a save and load round-trips both fields;
     - a later save with `avatar_id: None` keeps the stored id.
     - The test at `:487`, which sets user_version 6 as a "newer" version, must use 7.
   - Verify: `nix develop -c cargo test store::` (count > 0), clippy and fmt.

4. **Sync, protocol and serve** (`src/sync.rs`, `src/proto.rs`, `src/serve.rs`,
   `Cargo.toml`, `Cargo.lock`). *coder: codex*
   - **`proto.rs`:**
     - `Conversation` (`:137`) gains `color: String` and `avatar: Option<String>`.
     - `from_phone` and `from_store` take a third argument,
       `avatar: Option<String>`, and set `color` from `colour`.
     - Add `AvatarsEvent { items: BTreeMap<String, String> }`.
     - The no-secrets test fills the new fields.
   - **`sync.rs`:**
     - Add the state listed in "Data" above, plus a `u64` token counter and
       `avatar_dir: PathBuf`. `Sync::new` takes `avatar_dir`.
     - `fn enqueue(&mut self, id)`, and `fn pump(&mut self)`, which sends one batch when
       the conditions in "Fetching" hold.
     - `conversations()` becomes `&mut self`: it enqueues each item's avatar_id, calls
       `pump`, and fills `avatar`.
     - `conversation()` fills `avatar`. It enqueues only when `ready`.
     - `Event::Ready` sets `ready` and calls `pump` after its outputs.
     - `Unpaired`, `Expired`, `Fatal` and `Login`:
       - clear `ready`;
       - bump the token, so `in_flight` becomes `None`;
       - only on `Unpaired`, empty `avatars`, `queue`, `tried` and `failed_once`, and
         clear `avatar_dir`.
     - `Event::Avatars`:
       - drop it if the token doesn't match;
       - apply the image checks and write the files;
       - update the maps;
       - emit one `avatars` event for every stored conversation (`store.conversation_list()`)
         whose `avatar_id` got a path;
       - call `pump`.
     - Logging has counts only.
   - **`serve.rs`:**
     - Create `runtime/avatars` (0700) next to `media` (`:115`), and clear it at start and
       at stop (`:125`, `clear_media` at `:877`, reused).
     - Pass it to `Sync::new`.
     - The `"conversations"` arm (`:661`) uses `as_mut()`.
   - **`Cargo.toml`:** `imagesize` (latest 0.x), added with `cargo add`.
   - **Tests** in `sync.rs`, using the existing fixtures:
     - colour reaches `color`;
     - queueing skips duplicates, `tried` ids and `avatars` ids;
     - batches hold at most 50 ids, with one in flight;
     - a bad magic, a 300 KiB file, and a 40-byte PNG whose header says 20000×20000 are
       all rejected;
     - an unrequested id is ignored;
     - a failed batch requeues once, then the ids go to `tried`;
     - a late result after `Unpaired` is dropped;
     - files are 0600 and named by their content;
     - a `conversations` reply after the `avatars` event carries the path;
     - `Unpaired` empties the directory.
   - Verify: `nix develop -c cargo test` (all), clippy and fmt.

5. **Plugin** (`plugin/Avatar.qml` new, `plugin/Client.qml`,
   `plugin/ConversationList.qml`, `plugin/Thread.qml`, `plugin/App.qml`).
   *coder: coder*
   - **`Client.qml`:** `signal avatarsArrived(var items)`, and a `case "avatars":` in the
     event switch (`:101`).
   - **`Avatar.qml`:** as in "Plugin" above. The person icon is the same glyph font the
     bar widget uses.
   - **`ConversationList.qml`:**
     - add the `avatars` map;
     - `upsertConversation` (`:85`) and the page merge (`:64`–`:83`) copy `item.avatar`
       into the map when it isn't null;
     - `onAvatarsArrived` merges in one assignment;
     - the delegate (`:340`) puts an `Avatar` before the unread dot, at `size: Style.space(32)`;
     - `conversationSelected` gains `avatar`, `color` and `group` arguments.
   - **`App.qml`:** forwards them to `Thread`.
   - **`Thread.qml`:** `property string conversationAvatar`, `conversationColor` and
     `conversationGroup`, and an `Avatar` at `size: Style.space(40)` in the header
     (`:180`).
   - Verify:
     - qmllint parse-only on the five files, with the flags `nix/plugin.nix` uses;
     - *(Claude)* `nix build .#plugin`.

6. **Build, deploy, check** *(Claude)*
   - `nix flake check`, then push.
   - In `~/.config/nixos-gmessages`: run `nix flake lock --update-input gmessages`, build
     p620 and razer, and check that both closure diffs touch only the gmessages paths.
   - Post on the agent bus, then switch p620 and razer. The owner asked for both hosts
     on 2026-11-12.
   - Checks:
     1. The list shows pictures for contacts that have one on the phone, and initials
        circles for the others and for groups.
     2. The thread header shows the same picture.
     3. `ls -l $XDG_RUNTIME_DIR/gmessages-tui/avatars`: 0700, with files at 0600.
     4. After the unit stops and starts again (script file), the pictures come back.
     5. `phone.log` has no participant ids.
     6. There are no QML errors in the journal.

## Tests

| When | Command | Expected |
|------|---------|----------|
| Steps 1, 3, 4 | that step's verify, plus clippy and fmt | green; filtered counts > 0 |
| Step 5 *(Claude)* | `nix build .#plugin` | builds, qmllint parse passes |
| Step 6 *(Claude)* | `nix flake check` | all pass |
| Step 6 | checks 1–6 | confirmed with the owner |
| Review | Opus review with only this plan path and `git diff 9ed1289..HEAD`; then Codex and agy, read-only | findings reported; none approves anything |

## Rollback

- Each step is its own commit on `feat/nixos-flake`: revert it.
- Migration v6 only adds nullable columns. A v5 binary refuses a v6 store ("store from a
  newer version"), so rolling the binary back also needs the two columns dropped, or the
  store restored. The plan doesn't automate that: ask the owner.
- On the hosts, switch to the previous generation. `runtime/avatars` is tmpfs and goes
  away at logout.
