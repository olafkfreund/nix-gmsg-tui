---
status: approved
issue: none (issues are disabled on the fork)
spec: spec/2026-11-11-omarchy-messages.md
stage: b2 (media and reactions; the rest of stage b after b1's contact pictures)
coders:
  1: coder   # protocol DTOs, phone download of one medium, store per-position cache: plain and bounded
  2: codex   # Sync and serve: per-message download queue, expiry, Downloads writes with no overwrite: small, tricky logic
  3: coder   # QML: attachment buttons, reaction chips and picker in Thread.qml, one Client deadline, Errors.js
reviewers:
  - codex   # independent read of the whole diff against this plan
  - agy     # second independent read; disagreement is the point
---

# Plan: Messages plugin, stage b2: media and reactions

Revised after Codex and agy reviewed the first draft (2f6a99a). Both found the same three
problems:
- concurrent downloads of one message break each other (`phone.rs:1527`, one `full`
  listener per message);
- the size cap came after the bytes were in RAM and in the store;
- a save that timed out could still write into Downloads.

agy also found:
- previews cached as if they were the full medium;
- index drift when a stored medium is malformed;
- media gated on `ready` although cached media needs no phone;
- the `runtime/media` clear on unpair, required by spec section 6;
- raw phone errors reaching QML;
- several QML layout traps.

All are fixed below.

## What is decided (copied from the approved spec)

- **Methods** (spec section 2, stage b row):
  - `media.open {message, index}` → `{path}`;
  - `media.save {message, index}` → `{path}`;
  - `react {conversation, message, emoji, op}` → `{}`.
- **Deadlines:** `media.*` 60 s, `react` 15 s. A client waits the deadline plus 5 s.
- **Error codes:** the existing set, nothing new: `not_found`, `phone_offline`,
  `not_paired`, `bad_request`, `timeout`, `internal`.
- **Secrets never reach QML:** media keys and ids stay in the store. Only the name, type
  and size of an attachment leave it, as today. Phone error strings don't leave either:
  replies carry fixed messages.
- **Where decrypted media goes:** `runtime/media/<name>` (0600), opened with `file://`.
  `runtime/media` is cleared at start (`serve.rs:118-121`), at stop, and on
  `Event::Unpaired` (spec section 6). systemd removes the whole runtime dir when the unit
  stops.
- **Saving:** `media.save` copies atomically into the XDG Downloads directory and never
  overwrites a file there.
- **Check** (spec verification 8, the whole item):
  - an image opens and saves;
  - a reaction reaches the phone;
  - contacts with a picture show it in the list and the thread header, and the others
    show initials;
  - after the unit starts again, the pictures come back without pairing.

  The picture parts are b1's checks (b1 step 6, still open). They're done in the same
  sitting as step 4 here.

## Decided here (new in this plan, for approval)

**The index:**
- `index` counts only the stored media entries that parse as `phone::Medium`, in order.
  That's exactly the list `proto::Message::from_phone` turns into `attachments`
  (`proto.rs:221-231`).
- Sync uses the same filter, so the index the window shows is the one the backend
  downloads and caches.

**One medium at a time:**
- `Command::Download` carries the single medium asked for, not the whole message. RAM per
  request is one medium.
- The store caches it by position. A new `Store::attachment(message, position)` reads one
  row, and `save_attachment(message, position, a)` does an `INSERT OR REPLACE`; the
  primary key is `(message, position)` (`store.rs:92-101`). The existing whole-message
  `save_attachments` stays for its callers.
- **One download per message at a time,** because the phone layer has one full-size
  listener per message (`phone.rs:1527-1538`):
  - Sync keeps a FIFO queue per message.
  - Requests for the same `(message, index)` share one download, and every waiting rid
    gets the result.

**Previews are not cached:**
- `phone::Attachment` gains `full: bool`: false when `download` fell back to the preview
  or the inline thumbnail (`phone.rs:1494-1505`).
- A preview still answers the request: the file opens and saves. But it isn't written to
  the store, and its runtime file name has `-preview`, so the next open asks the phone
  again.

**Size cap: 100 MiB per medium.**
- Before downloading, a declared `size` over the cap fails as "too large".
- After downloading, bytes over the cap fail the same way, and nothing is cached or
  written.
- *shortcut:* `gm::Client::download_media` still buffers the whole body before the
  after-check. Stream with a limit if a phone ever sends media over 100 MiB without
  declaring it.

**Expiry and late results:**
- serve tells Sync when a rid expires or its connection closes (`Daemon::expire`,
  `serve.rs:444`; `Daemon::close`, `serve.rs:373`), and Sync drops it from the waiters.
- A download that finishes with no waiters left still fills the cache, if it's full and
  under the cap, but writes no file in `runtime/media` or Downloads.

**Gating:**
- `media.*` needs only the store (`self.sync.is_some()`, like `history`), so a cached
  medium opens while the phone is offline.
- On a cache miss when the phone isn't `ready`, the reply is `phone_offline`.
- `react` needs `ready`; otherwise `self.unready`.

**Opening:** "Open" hands the file to the desktop's default app:
`Qt.openUrlExternally("file://…")`, which runs `xdg-open`. Images aren't shown inline in
b2.

**Runtime file name:** `hex(sha256(message_id bytes ‖ 0x00 ‖ index as u32
big-endian))`, then `-preview` for a preview, then `.<ext>`. An existing file isn't
written again.

**Saving to Downloads:**
- **Folder:** `dirs::download_dir()`, created with `create_dir_all` if it's missing. If
  it's `None`, the reply is `internal` "no Downloads folder".
- **Name:**
  - the attachment's name after the last `/` or `\`, with control characters removed,
    cut to 200 bytes on a char boundary;
  - an empty name, `.`, `..`, or one starting with `.` becomes `message-<ms>.<ext>`.
    `<ms>` is the message time, held in the request.
  - a name with no extension gets `.<ext>`.
- **Write:**
  1. Write `.gmessages-<16 random hex>.part` in Downloads with `create_new(true)`.
  2. `fs::hard_link` it to the name, then ` (2)`, ` (3)` … ` (99)` before the extension,
     moving on only on `AlreadyExists`.
  3. Remove the temporary file whatever happens.

  Running out of names gives `internal` "no free file name".

**Extensions:** from the type lowercased and cut at `;`. `image/jpeg`→`jpg`,
`image/png`→`png`, `image/gif`→`gif`, `image/webp`→`webp`, `image/heic`→`heic`,
`video/mp4`→`mp4`, `video/3gpp`→`3gp`, `audio/mpeg`→`mp3`, `audio/ogg`→`ogg`,
`application/pdf`→`pdf`, `text/x-vcard` and `text/vcard`→`vcf`. Anything else gets
`bin`.

**Replies on failure:** fixed texts only, with the phone's own error logged at `debug`
and no ids:
- `not_found` "message not found" or "no such attachment";
- `phone_offline` "phone did not send it";
- `internal` "too large", "no Downloads folder", "no free file name" or "cannot write
  the file".

`Errors.js` shows `err.message` for `internal`, since these texts are ours.

**Reactions, what you see:**
- Under the bubble, in the same delegate, there's a row of chips: the emoji and how many
  people sent it.
  - A chip that's mine has a thicker border in `Commons.Color.foreground` (the accent is
    already my bubble's fill).
  - The delegate's height includes the chips row.
- Hovering the whole message row (bubble and chips) shows a bar with 👍 ❤️ 😂 😮 😢 🙏.
  - The bar sits above the bubble, inside the row: on the right for my messages, on the
    left for others.
  - It never sits beside the bubble, which would leave the hover area or be clipped.
- **Which op:**
  - picking the emoji I already sent removes it (`remove`);
  - picking another while I have any reaction on the message, custom ones from the phone
    included, replaces it (`replace`);
  - otherwise it adds one (`add`).
  - Clicking one of my chips removes it.
- **No bar and no clickable chips** on deleted, pending or failed messages. A pending
  message has a temporary id the phone doesn't know.
- While a `react` is in flight for a message, its bar and chips don't take clicks.
- The window shows a reaction only once the phone has it: the phone pushes a message
  update, which arrives as a `message` event like any other update, stored and quiet
  (`sync.rs:294-347`).

**State that outlives delegates:** `mergeMessages` (`Thread.qml:73`) replaces the message array on every
event, so the busy, error and "Saved to" state lives in maps on the
Thread root, keyed by `message + ":" + index` or by message id, not in delegates.

**Out of b2** (named, not built):
- a keyboard path for reacting;
- inline image previews;
- custom emoji beyond the six;
- sending attachments;
- a total budget for `runtime/media` (it's emptied at stop and on unpair).

## Rules for every step

- Work on branch `feat/nixos-flake`. Never on `main`, never in `~/.config/nixos`.
- Implementers never stage, commit, push, deploy, switch, restart, garbage-collect or
  reboot. Claude commits after each step.
- `cargo test --workspace` and `cargo clippy --all-targets -- -D warnings` pass after every
  Rust step.
- No message text, media bytes, media ids, keys or file names in `phone.log` or
  `tracing`. Paths in logs are fine only as the `runtime/media` hash.
- **Trap:** a git flake ignores untracked files, so a new file is committed before
  `nix build`.
- **Trap:** `qs.Commons` `Color` has `foreground`, `background`, `accent`, `urgent` and
  `muted`, and no `surface`.
- **Trap:** `Thread.qml` has `pragma ComponentBehavior: Bound`, so every `Repeater`
  delegate that uses `index` or `modelData` declares it as a `required property`.

## Steps

1. **Protocol, phone, store** (`src/proto.rs`, `src/phone.rs`, `src/store.rs`). *coder: coder*
   - **proto:**
     - `Message` gains `reactions: Vec<Reaction>`. Each `Reaction` is
       `{emoji: String, count: u32, mine: bool}`.
       - They're aggregated in `from_phone` from `phone::Message.reactions`, one per
         emoji, in first-seen order.
       - `mine` is true when any of that emoji's entries is `mine`.
       - `sender_id` never leaves.
     - `ReactParams {conversation, message, emoji, op}`, with `op` an enum
       `add|remove|replace` (serde lowercase) that maps to `gm::ReactionOp`
       (`gmessages-proto/src/types.rs:1368`).
     - `MediaParams {message, index: u32}` and `MediaData {path: String}`.
   - **phone:**
     - `download()` (`phone.rs:1481`) keeps its signature. Callers send one medium.
     - It checks the declared `size` and then the downloaded length against
       `MEDIA_MAX = 100 MiB`, and fails with "too large".
     - It sets `Attachment.full` to false on the preview and inline fallbacks.
     - Its errors stay in `Event::Downloaded.error`. Sync logs them at `debug` and never
       forwards them.
   - **store:** `attachment(message, position) -> Option<Attachment>` and
     `save_attachment(message, position, &Attachment)` (`INSERT OR REPLACE`).
   - **Tests:**
     - aggregation: two 👍 (one of them mine) and one ❤️ gives `[👍 2 mine, ❤️ 1]`;
     - the secrets test, with a `SECRET-…` marker in `Reaction.sender_id`
       (`proto.rs:491`), still finds no marker;
     - store: save position 1 and read it back; position 0 stays `None`;
     - phone: a declared size over the cap fails before any download. It's unit-tested
       on a helper `check_size(declared, actual)`.

   → verify by `cargo test --workspace && cargo clippy --all-targets -- -D warnings`

2. **Sync and serve** (`src/sync.rs`, `src/serve.rs`). *coder: codex*
   - **`Sync::new`** takes `media_dir: PathBuf` and `downloads: Option<PathBuf>`, after
     `avatar_dir`. Its callers:
     - `Daemon::got_key` (`serve.rs:282-286`), with `serve_bound`'s `_media` (now used)
       and `dirs::download_dir()`, stored on `Daemon` the way `avatars` is;
     - the test helper `tests::sync()` (`sync.rs:879`), with temp dirs.
   - **Sync state:**
     - `waiters: HashMap<u64, Waiter>`, where `Waiter { message, index, save, ms }`;
     - `queues: HashMap<String, VecDeque<u32>>`, the indexes waiting per message (the
       first one is the download in flight);
     - `in_flight: HashMap<u64, (String, u32)>`, from the download's id to its
       `(message, index)`.

       The download id comes from the `next_token` counter b1 added. It's never a rid,
       so a rid and an avatar token can't collide.
   - **`media(rid, message, index, save) -> Vec<Out>`:**
     1. `store.message(message)`. If it's missing, reply `not_found`. Filter its media as
        decided above. An index past the end replies `not_found`.
     2. If `store.attachment(message, index)` is cached, finish at once: write the
        runtime file, save if asked, and reply.
     3. If the phone isn't `ready`, reply `phone_offline`.
     4. Otherwise, add a waiter. If `(message, index)` is already queued, wait on it.
        Else push it onto the message's queue, and if the queue was empty, send
        `Command::Download { id: token, message, media: vec![that medium] }`.
   - **`Event::Downloaded { id, attachments, error }`:**
     - Pop the queue head.
     - If it's full and under the cap, save it with `save_attachment`. A store failure
       is logged, and the bytes are still used.
     - Finish every waiter on that `(message, index)`, or reply `phone_offline` to them
       on an error.
     - Start the next index in that message's queue.
     - An id that isn't in `in_flight` is ignored.
   - **`forget(rid)`**, called by `Daemon::expire` and by `Daemon::close` for each rid
     they drop, removes the waiter. Downloads keep running and fill the cache.
   - **`react(rid, p)`:** send `Command::React`, recording the rid in a `reacts` set. On
     `Event::Done { id }` for a rid in that set, reply `{}`, or `phone_offline` "phone
     refused the reaction". Ids that aren't in the set keep being ignored
     (`sync.rs:631`).
   - **On `Event::Unpaired` and `Event::Paired`:**
     - clear `media_dir` with `clear_media`, next to `reset_avatars`;
     - drop every waiter with `not_paired`;
     - drop the queues.
   - **serve** (next to `"send"`, `serve.rs:727-760`):
     - add `"media.open"` and `"media.save"`. Each needs `self.sync.is_some()` (else
       `self.unready`). Empty `message` is `bad_request`.
     - Register `Pending` with 60 s, then pass `sync.media(..)` to `self.outputs(..)` and
       `return`, so an immediate reply is written once, and not again by the generic
       reply at `serve.rs:763-770`.
     - `"react"` is the same with 15 s, needs `ready`, and empty `conversation`,
       `message` or `emoji` is `bad_request`.
   - **Tests** in `sync.rs`, with `Commands::detached()` as in b1:
     - a cached attachment answers at once and sends no command;
     - a miss sends one `Download` with one medium. `Downloaded` writes a 0600 file and
       replies with its path.
     - two requests for the same `(message, index)` send one `Download` and both get the
       path;
     - two indexes of one message send the second `Download` only after the first
       `Downloaded`;
     - `forget` and then `Downloaded` caches it and writes nothing: no runtime file, and
       Downloads stays empty;
     - a preview (`full: false`) answers but isn't cached, and its name ends in
       `-preview`;
     - a malformed medium before a valid one: index 0 is the valid one;
     - a bad index is `not_found`;
     - offline with no cache is `phone_offline`, and offline with a cache answers;
     - save twice gives `name.png`, then `name (2).png`, in a temp Downloads dir that
       already holds a `name.png` of other bytes, which stays untouched;
     - `../x.png` saves as `x.png`; `.hidden` and `` give `message-<ms>.<ext>`;
       `IMAGE/JPEG; q=1` gives `.jpg`;
     - `react`: `Done` maps to the reply, with and without an error.
     - `Unpaired` empties `media_dir` and replies `not_paired` to the waiters.

   → verify by `cargo test --workspace && cargo clippy --all-targets -- -D warnings`

3. **Plugin** (`plugin/Thread.qml`, `plugin/Client.qml`, `plugin/Errors.js`). *coder: coder*
   - **Client** (`Client.qml:69`): the inline deadline becomes 125 s for `send`, 65 s for
     `media.*`, and 20 s for everything else, `react` included.
   - **Errors.js** (`format`, line 36): for `internal`, show `err.message` when it's
     present.
   - **Attachments:** each attachment row (`Thread.qml:359-372`; the delegate declares
     `required property int index` and `required property var modelData`) gets two small
     buttons, "Open" and "Save", with icons and an `I18n` label each.
     - Open: `media.open`, then `Qt.openUrlExternally("file://" + path)`.
     - Save: `media.save`, then "Saved to <file name>" under the row for 4 s.
     - While a request runs the row is busy. An error shows its `Errors.js` string on the
       row.
     - That state lives in Thread-root maps keyed by `message + ":" + index`.
   - **Chips and picker:** as decided above.
     - The `HoverHandler` sits on the whole row item.
     - `bubbleRow` height becomes the column's `implicitHeight` (bar, bubble and chips),
       not `bubbleRect.implicitHeight` (`Thread.qml:289`).
     - The in-flight flag lives in a Thread-root map keyed by message id. It's cleared on
       the reply, or when that message's next `message` event arrives.
   - **Strings:** new `I18n` keys `gmessages.open`, `gmessages.save`,
     `gmessages.saved_to` and `gmessages.react`, with English fallbacks.
     `gmessages.saved_to` is joined with the name the way `Thread.qml:251` builds its
     strings.

   → verify by `nix build .#plugin` (qmllint parse)

4. **Build, deploy, check** *(Claude)*
   - `nix flake check` and `nix build .#gmessages-tui .#plugin`.
   - Lock the NixOS worktree, post on the bus, then switch p620 (the owner OKs).
   - Checks on p620, with the owner (the spec's whole item 8):
     - an image opens in the default viewer;
     - Save puts it in `~/Downloads`, and a second save gives ` (2)`;
     - a cached image opens with the phone's network off;
     - a 👍 from the window shows on the phone, and its chip appears in the window;
     - removing it clears it on both;
     - a reaction sent from the phone shows up as a chip;
     - contact pictures, with initials for the others, in the list and the thread
       header. They come back after the unit starts again (b1 step 6).
     - `runtime/media` is 0700 and its files 0600; the runtime dir is gone after the
       unit stops;
     - no message text or file names in `phone.log`;
     - no QML errors.
   - Then razer, with this b2 build, after the owner says p620 is right.

## Deviations

- **Step 3, the emoji bar:** it floats over the top edge of the bubble (z above, outside
  the layout) instead of sitting above it. A bar that takes layout space on hover shifts
  the list under the pointer and can flicker. While it shows, it covers the sender name
  or the first line.
- **Step 3, tooltip:** `gmessages.react` is defined but not shown. The picker tooltip was
  dropped, because `ToolTip` isn't imported in `Thread.qml`.
- **Step 3, reaction errors:** a failed `react` shows its error in the composer's status
  line.
- **Review fixes** (from the Codex, agy and Opus reviews of the diff):
  - `Thread.qml`:
    - An in-flight react is now `{busy: true}`. `setUi` had turned `true` into `{}`, so
      the guard never blocked a second react.
    - The emoji bar hides while the pointer is over an attachment row, so it no longer
      covers Open and Save.
  - `sync.rs`, the runtime file: written through a random temp file and renamed into
    place.
  - `sync.rs`, `media.save`: saves straight from the bytes and skips the runtime file.
  - `sync.rs`, the cache: a result for a message that is gone isn't cached.
  - `sync.rs`, `reset_media`: answers pending reacts with `not_paired`.
  - `sync.rs`, saved file names:
    - drop bidi and format characters;
    - the 200-byte cut keeps the extension (one test covers both).
  - `serve.rs`: expired requests are dropped before each phone event.
  - `phone.rs`: the declared size is checked before `full()` as well as after.
  - Not fixed:
    - Saved files are 0600.
    - `hard_link` fails on exFAT.
    - A body with no declared size is still buffered whole before the cap applies.
    - A race on the `full` listener after re-pairing.

## Tests

```bash
cargo test --workspace
cargo clippy --all-targets -- -D warnings
nix flake check
nix build .#gmessages-tui .#plugin
```

All green, plus the p620 checks in step 4.

## Rollback

- Revert this stage's commits on `feat/nixos-flake`, relock the NixOS worktree, and
  switch.
- There's no schema change (the `attachments` table exists), so the store needs nothing.
- Files saved to Downloads stay; they're the owner's.
