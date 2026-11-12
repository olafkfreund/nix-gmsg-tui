---
status: draft
issue: none (issues are disabled on the fork)
spec: spec/2026-11-11-omarchy-messages.md
stage: b2 (media and reactions; the rest of stage b after b1's contact pictures)
coders:
  1: coder   # protocol DTOs and params, one aggregation, unit tests: plain and well bounded
  2: codex   # Sync and serve: pending requests by kind, cache, file writes with no overwrite: small, tricky logic
  3: coder   # QML: attachment buttons, reaction chips and picker in Thread.qml, one Client deadline
reviewers:
  - codex   # independent read of the whole diff against this plan
  - agy     # second independent read; disagreement is the point
---

# Plan: Messages plugin, stage b2: media and reactions

## What is decided (copied from the approved spec)

- **Methods** (spec section 2, stage b row):
  - `media.open {message, index}` → `{path}`;
  - `media.save {message, index}` → `{path}`;
  - `react {conversation, message, emoji, op}` → `{}`.
- **Deadlines:** `media.*` 60 s, `react` 15 s. A client waits the deadline plus 5 s.
- **Error codes:** the existing set, nothing new: `not_found`, `phone_offline`,
  `bad_request`, `timeout`, `internal`.
- **Secrets never reach QML:** media keys and ids stay in the store. Only the name, type
  and size of an attachment leave it, as today.
- **Where decrypted media goes:** `runtime/media/<sha256(message,index)>.<ext>` (0600),
  opened with `file://`. `runtime/media` is already cleared at start and at stop
  (`src/serve.rs:118-131`).
- **Saving:** `media.save` copies atomically into the XDG Downloads directory and never
  overwrites a file there.
- **Check** (spec verification 8): an image opens and saves, and a reaction reaches the
  phone.

## Decided here (new in this plan, for approval)

**Opening media:**
- "Open" hands the file to the desktop's default app: `Qt.openUrlExternally("file://…")`,
  which runs `xdg-open`. Images aren't shown inline in the thread in b2.
- Downloaded bytes are kept in the encrypted store's existing `attachments` table
  (`store.rs:413-450`), so a second open needs no phone. The decrypted copy lives in
  `runtime/media` only.
- One medium larger than 100 MiB is refused with `internal` "too large". The phone's data
  is untrusted, and `runtime` is RAM.

**Saving:**
- The file name is the attachment's own name, cut to its last path component. A name
  that is empty, `.`, `..` or starts with `.` becomes `message-<ms>.<ext>`.
- If that name is taken, ` (2)`, ` (3)`, and so on up to ` (99)`, go before the
  extension.
- The copy is written to a temporary file in Downloads, then `hard_link`ed to the final
  name, which fails rather than overwrites when the name exists. The temporary file is
  then removed.
- With no Downloads directory (`dirs::download_dir()` is `None`), the reply is
  `internal` "no Downloads folder".

**Extensions,** from the type: `image/jpeg`→`jpg`, `image/png`→`png`, `image/gif`→`gif`,
`image/webp`→`webp`, `image/heic`→`heic`, `video/mp4`→`mp4`, `video/3gpp`→`3gp`,
`audio/mpeg`→`mp3`, `audio/ogg`→`ogg`, `application/pdf`→`pdf`, `text/x-vcard` and
`text/vcard`→`vcf`. Anything else gets `bin`.

**Reactions, what you see:**
- Under each bubble that has reactions, there's a row of chips: the emoji and how many
  people sent it, highlighted when one of them is you.
- Hovering a bubble shows a small bar with 👍 ❤️ 😂 😮 😢 🙏.
  - Picking the emoji you already sent removes it (`remove`).
  - Picking another one while you have a reaction replaces yours (`replace`).
  - Otherwise it adds one (`add`).
- Clicking one of your own chips removes it.
- The window doesn't show a reaction until the phone has it: the phone pushes a message
  update, which comes in as a `message` event like any other update, already stored and
  quiet (`sync.rs:296-342`). The reply to `react` only says the phone accepted it.

**Out of b2** (named, not built):
- a keyboard path for reacting;
- inline image previews;
- custom emoji beyond the six;
- sending attachments.

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

## Steps

1. **Protocol** (`src/proto.rs`). *coder: coder*
   - `Message` gains `reactions: Vec<Reaction>`. Each `Reaction` is
     `{emoji: String, count: u32, mine: bool}`. They're aggregated in `from_phone` from
     `phone::Message.reactions`, one per emoji, in first-seen order. `mine` is true when
     any of that emoji's entries is `mine`.
   - `ReactParams {conversation, message, emoji, op}`, with `op` an enum
     `add|remove|replace` (serde lowercase) that maps to `gm::ReactionOp`.
   - `MediaParams {message, index: usize}` and `MediaData {path: String}`.
   - Tests:
     - aggregation: two 👍 (one of them mine) and one ❤️ gives `[👍 2 mine, ❤️ 1]`;
     - the secrets test still passes, with reactions filled.
   → verify by `cargo test -p gmessages-tui proto`

2. **Sync and serve** (`src/sync.rs`, `src/serve.rs`). *coder: codex*
   - **serve** (next to `"send"`, `src/serve.rs:727`):
     - add `"react"`, `"media.open"` and `"media.save"`. Each needs state `ready` (else
       `self.unready`), registers a `Pending` with its deadline (`react` 15 s, `media.*`
       60 s), and calls into `Sync` with the `rid`.
     - Empty `conversation`, `message` or `emoji` is `bad_request`.
   - **sync:**
     - new `requests: HashMap<u64, Request>`, with
       `enum Request { React, Media { message: String, index: usize, save: bool } }`.
     - `react(rid, p)`: send `Command::React`. On `Event::Done { id, error }` for a
       `React` rid, reply `{}` or `phone_offline` with the error text.
     - `media(rid, message, index, save)`:
       1. Look up `store.message(message)`. No such message, or `index` ≥
          `media.len()`, replies `not_found`.
       2. If `store.attachments(message)` has the index, finish at once.
       3. Otherwise, record the request and send `Command::Download { id: rid, message,
          media }`.
     - On `Event::Downloaded`:
       - with an error, reply `phone_offline`;
       - otherwise `store.save_attachments` (a failure is logged and the bytes are still
         used), then finish.
     - **finish:** check the size cap. Write `runtime/media/<hex sha256(message ‖ 0x00 ‖
       index)>.<ext>` with the 0600 temp-then-rename used by `save_avatar`, skipping the
       write when the file already exists.
       - For open, reply `{path}`.
       - For save, copy into Downloads as decided above and reply with that path.
     - The `Done`/`Downloaded` arm at `sync.rs:631` keeps ignoring ids that aren't in
       `requests`.
   - `Sync::new` takes the media dir (`serve.rs:118`) the way it takes `avatar_dir`.
   - Tests in `sync.rs`, with `Commands::detached()` as in b1:
     - a cached attachment answers at once and sends no command;
     - a missing one sends `Download`, and `Downloaded` writes a 0600 file and replies
       with its path;
     - a bad index is `not_found`;
     - a save twice gives `name.png` and `name (2).png` in a temp Downloads dir, and never
       touches an existing file;
     - `../x.png` saves as `x.png`, and `.hidden` saves as `message-<ms>.<ext>`;
     - `react` maps `Done` to the reply, with and without an error.
   - Pass the Downloads dir into `Sync` (from `dirs::download_dir()`), so tests use a
     temp dir.
   → verify by `cargo test --workspace && cargo clippy --all-targets -- -D warnings`

3. **Plugin** (`plugin/Thread.qml`, `plugin/Client.qml`). *coder: coder*
   - **Client** (`Client.qml:69`): the inline deadline becomes 125 s for `send`, 65 s for
     `media.*`, and 20 s for everything else, `react` included.
   - **Attachments:** each attachment row (`Thread.qml:359-372`) gets two small buttons,
     "Open" and "Save", with icons and an `I18n` label each. While a request runs, that
     row shows a busy state.
     - Open: `media.open`, then `Qt.openUrlExternally("file://" + path)`.
     - Save: `media.save`, then a short line under the row, "Saved to <file name>", for
       4 s.
     - An error shows its `Errors.js` string on the row.
   - **Chips:** a `Flow` of reaction chips under the bubble text: the emoji and its count,
     with an accent border when `mine`. Clicking a mine chip sends `remove`.
   - **Picker:** a `HoverHandler` on the bubble shows the six-emoji bar beside it, with
     the op chosen as decided above.
   - **Strings:** new `I18n` keys `gmessages.open`, `gmessages.save`, `gmessages.saved_to`
     and `gmessages.react`, with English fallbacks. `Errors.js` already maps
     `not_found`, `phone_offline` and `internal` (`Errors.js:7-16`), so it needs nothing.
   → verify by `nix build .#plugin` (qmllint parse)

4. **Build, deploy, check** *(Claude)*
   - `nix flake check` and `nix build .#gmessages-tui .#plugin`.
   - Lock the NixOS worktree, post on the bus, then switch p620 (the owner OKs).
   - Checks on p620, with the owner:
     - an image opens in the default viewer;
     - Save puts it in `~/Downloads`, and a second save gives ` (2)`;
     - a 👍 from the window shows on the phone, and its chip appears in the window;
     - removing it clears it on both;
     - a reaction sent from the phone shows up as a chip;
     - `runtime/media` is 0700, its files are 0600, and it's empty after the unit stops;
     - no message text or file names in `phone.log`;
     - no QML errors.
   - Then razer, with the b1 build, after the owner says p620 is right.

## Deviations

None yet.

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
- There's no schema change, so the store needs nothing.
- Files saved to Downloads stay; they're the owner's.
