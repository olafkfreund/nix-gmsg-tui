---
status: draft
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui)
spec: spec/2026-11-17-send-attachments.md
coders:
  1: codex   # phone.rs: O_NONBLOCK open, handle checks, bounded read, error mapping: small, tricky logic at the trust boundary
  2: codex   # proto/serve/sync: features, request checks, the upload guard that outlives forget: small, tricky logic
  3: coder   # QML: attach button, FileDialog, chips, per-chat state, conditional clearing: plain and broad
reviewers:
  - codex   # independent read of the whole diff against this plan
---

# Plan: Send pictures and files from the panel

Self-contained: an implementer needs only this file and the code.

## Approved decisions (from the spec)

- The plugin sends local **absolute paths**, not file bytes: `send` gains
  `attachments: [paths]`.
- Limits:
  - 100 MiB per message in total (the existing `MEDIA_MAX`);
  - at most 10 files;
  - empty files are refused;
  - the MIME type comes from a built-in extension table, otherwise
    `application/octet-stream`;
  - no new crate.
- `hello` gains `features: ["attachments"]`. `PROTOCOL` stays 1. The plugin offers and
  sends files only while the current connection advertises the feature.
- New error code `attachment` (`ErrorCode::Attachment`). Its message is shown as is: the
  cleaned base name, then a reason (`cannot read`, `not a file`, `empty`,
  `too large (over 100 MiB in all)` or `upload failed`).
- Files are read in `spawn_blocking`, opened with `O_NONBLOCK`, and checked on the open
  handle (a regular file, length > 0). The read goes through `take(remaining + 1)`, and
  reading 0 bytes means `empty`. Every file is checked before the first upload.
- Images get width and height from `imagesize`, set on `Media::dimensions` after the
  upload. The no-echo fallback message carries the uploaded media.
- The daemon allows **one upload at a time**: `Sync::uploading: Option<u64>`. It is set
  on dispatch and cleared only on that rid's `phone::Event::Sent`, never in `forget`.
  A second send with files is refused with `busy`.
- Deadline with attachments: daemon 900 s, plugin 905 s. Plain sends keep 120/125 s.
- Logging: names, paths and OS error texts only at `debug`. A `warn` line carries a fixed
  text.
- The chips show the name only (the deviation from the intent, approved with the spec).

## Rules for every step

- Work on branch `feat/send-attachments`. Never on `main`, never in `~/.config/nixos`.
- Implementers never stage, commit, push, deploy, switch, restart, garbage-collect or
  reboot. Claude commits after each step.
- `cargo test --workspace` and `cargo clippy --all-targets -- -D warnings` pass after
  every Rust step. Run them with `nix develop -c sh -c '…'`, because the host lacks the
  openssl headers.
- Leave the `allow(dead_code)` attributes in `src/main.rs` alone.
- No message text, numbers, contact names, conversation ids, keys, file names or paths go
  into `phone.log`, into `tracing` above `debug`, or into the journal.
- **Trap:** `src/phone.rs` has its own `log` module (`src/phone.rs:1869`), with `warn`
  only. Use `tracing::debug!` directly for the detailed lines.
- **Trap:** there is no `tempfile` crate. Tests make a unique directory under
  `std::env::temp_dir()`, as `src/sync.rs:1546` does, and remove it at the end.
- **Trap:** a git flake ignores untracked files, so a new file is committed before
  `nix build`.
- **Trap:** `qs.Commons` `Color` has `foreground`, `background`, `accent`, `urgent` and
  `muted`, and no `surface`.
- **Trap:** files with `pragma ComponentBehavior: Bound` need `required property` for
  `index` and `modelData` in every delegate.

## Steps

1. **`src/phone.rs` (+ the call site and match arm in `src/sync.rs`): read, check and
   upload files.**
   - `Command::Send` (`src/phone.rs:159`): `attachments: Vec<Attachment>` becomes
     `files: Vec<PathBuf>`. Update the dispatch at `src/phone.rs:1134-1142` and the one
     constructor, `src/sync.rs:1194-1200`, which passes `files: Vec::new()` for now
     (step 2 passes the real list). `phone::Attachment` (`src/phone.rs:316`) stays as is.
   - Add `fn read_files(paths: &[PathBuf]) -> Result<Vec<Attachment>, String>`, plus a
     private `fn file_name(path: &Path) -> String`. `file_name` takes the base name, drops
     the characters `save_download` drops (`src/sync.rs:1386-1392`: `is_control()`,
     U+200B–U+200F, U+202A–U+202E, U+2060–U+2069, U+FEFF) and falls back to `"file"`.
     For each path, in order:
     1. `let name = file_name(path)`.
     2. `OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path)`
        (`use std::os::unix::fs::OpenOptionsExt`). An error gives
        `"{name}: cannot read"`.
     3. `file.metadata()`: not `is_file()` gives `"{name}: not a file"`; `len() == 0`
        gives `"{name}: empty"`.
     4. `remaining = MEDIA_MAX - total`; read with
        `(&file).take(remaining as u64 + 1).read_to_end(&mut data)`. Reading more than
        `remaining` gives `"{name}: too large (over 100 MiB in all)"`; reading 0 bytes
        gives `"{name}: empty"`; a read error gives `"{name}: cannot read"`.
     5. `content_type = mime_for(&name)`: a `const MIME: &[(&str, &str)]` keyed on the
        lowercase extension (jpg, jpeg → image/jpeg; png; gif; webp; heic; heif; bmp;
        mp4 → video/mp4; 3gp → video/3gpp; mov → video/quicktime; webm; mkv →
        video/x-matroska; mp3 → audio/mpeg; m4a → audio/mp4; aac; ogg; opus → audio/ogg;
        amr → audio/amr; wav → audio/wav; pdf; txt → text/plain; vcf → text/vcard; zip),
        otherwise `application/octet-stream`.
     6. For `image/*`: `imagesize::blob_size(&data)` fills `width`/`height` as
        `Some(u32)`; on error they stay `None`.
     7. Push `Attachment { content_type, name: Some(name), data, width, height, full: true }`.

     Every error also writes `tracing::debug!(path = %path.display(), %error, "attachment refused")`
     with the OS error where there is one.
   - `SendOutcome` gains `Attachment(String)`.
   - In `send()` (`src/phone.rs:1388`):
     - First, if `files` is not empty:
       `tokio::task::spawn_blocking(move || read_files(&files)).await`. An `Err` returns
       `SendOutcome::Attachment(msg)`. A join error returns
       `SendOutcome::Attachment("attachment: cannot read".into())`.
     - Then, in the upload loop (`:1404-1416`), an upload error does
       `log::warn("attachment upload failed")` plus a `tracing::debug!` with the name and
       the error, and returns `SendOutcome::Attachment(format!("{name}: upload failed"))`.
     - After a successful upload, if both sizes are known, set
       `media.dimensions = Some(gm::Dimensions { width: w as i64, height: h as i64 })`.
       Check the import path the proto crate exports for `Dimensions`
       (`gmessages-proto/src/types.rs:847`).
     - Push `serde_json::to_value(stored_medium(&media, None)).unwrap_or_default()` onto
       a `sent_media` vec.
     - In the `Accepted` fallback (`:1465`), `media: sent_media` replaces `Vec::new()`.
   - `src/sync.rs:399-440` `sent()`: add an arm that maps `SendOutcome::Attachment(message)`
     to a `Reply` error with `proto::ErrorCode::Attachment`. That needs the
     `ErrorCode::Attachment` variant in `src/proto.rs:30-43`
     (`#[serde(rename_all = "snake_case")]` is already on the enum; check).
   - Tests in `src/phone.rs`'s test module (`:1894`): a jpeg/png with the right name, MIME
     and size (write a tiny valid PNG: the 67-byte 1×1 PNG as a byte literal); a
     directory gives `not a file`; an empty file gives `empty`; a missing path gives
     `cannot read`; two files over 100 MiB together give `too large` (create with
     `File::set_len` so they're sparse, not written); a name with `\u{202E}` and `\n` is
     cleaned in the error; `x.unknown` gives `application/octet-stream`; a FIFO made with
     `libc::mkfifo` gives `not a file`, run in a thread joined with
     `recv_timeout(5 s)` so a blocking open fails the test instead of hanging it.
   → verify with
   `nix develop -c sh -c 'cargo test --workspace && cargo clippy --all-targets -- -D warnings'`.
   Traps: `MEDIA_MAX` is a `usize` at `src/phone.rs:1481`; read it, don't copy it.
   Sparse 100 MiB files are fine on tmpfs.

2. **`src/proto.rs`, `src/serve.rs`, `src/sync.rs`, `tests/serve.rs`: protocol, request
   checks, the upload guard.**
   - `proto.rs`:
     - `SendParams` (`:342`) gains `#[serde(default)] pub attachments: Vec<String>`.
     - `HelloEvent` (`:403`) gains `#[serde(default)] pub features: Vec<String>`.
     - Add `pub const FEATURES: &[&str] = &["attachments"];` beside `PROTOCOL` (`:11`).
   - `serve.rs`:
     - The hello at `:569-578` sets
       `features: proto::FEATURES.iter().map(|s| s.to_string()).collect()`. Grep for every
       other `HelloEvent {` constructor, tests included, and fill it there too.
   - `serve.rs` `"send"` arm (`:1178-1210`), in this order:
     - The `Ok(p)` guard becomes: refuse with
       `"conversation and text or attachments required"` when `p.conversation` is empty,
       or when both `p.text` and `p.attachments` are empty.
     - New guards: more than 10 attachments gives `"at most 10 attachments"`; any path
       that is empty or not `Path::new(a).is_absolute()` gives
       `"attachments must be absolute paths"`. These replies go out as `bad_request`
       through the existing `reply = Err(..)` path.
     - Ready with attachments, while `self.sync` reports `upload_busy()`: write
       `proto::Reply::err(Some(id), proto::ErrorCode::Busy, "another upload is in progress")`
       and return, as `:1052` does.
     - `until`: `Duration::from_secs(if p.attachments.is_empty() { 120 } else { 900 })`.
     - The call becomes
       `sync.send(rid, p.conversation, p.text, p.reply_to, p.attachments.into_iter().map(PathBuf::from).collect())`.
   - `sync.rs`:
     - The `Sync` struct gains `uploading: Option<u64>`, initialised to `None` wherever
       `Sync` is constructed.
     - `pub fn upload_busy(&self) -> bool { self.uploading.is_some() }`.
     - `send(..)` (`:1192`) takes `files: Vec<PathBuf>`. If it is not empty, it sets
       `self.uploading = Some(rid)`, then sends `phone::Command::Send { files, .. }`.
     - In the `phone::Event::Sent` handler (`:1018-1024`): if
       `self.uploading == Some(id)`, set it to `None`.
     - `forget` (`:675`) does **not** touch `uploading`. Add a one-line comment saying
       why: the task still holds the files in memory.
     - If `Sync` is replaced or dropped on unpair, the new one starts with `None`. Nothing
       to carry over.
   - Tests:
     - `src/sync.rs`: `Attachment` maps to an `attachment` error reply. `send` with one
       file sets `upload_busy()`; `forget(rid)` leaves it busy; `Event::Sent` for that
       rid clears it; `Event::Sent` for another rid doesn't. An `Accepted` outcome whose
       message has one `media` value (a `phone::Medium` JSON with mime and name) gives a
       `message` event whose `attachments` has that name.
     - `tests/serve.rs`:
       - `hello` has `features == ["attachments"]`;
       - `send` with `"text":""` and no attachments is `bad_request`;
       - 11 absolute paths are `bad_request`;
       - `["rel/a.png"]` is `bad_request`;
       - `{"conversation":"c","text":"","attachments":["/tmp/a.png"]}` before pairing gets
         the same code as the existing text send at `tests/serve.rs:185`.
   → verify with the same command as step 1.
   Traps:
   - the `"send"` arm returns early on success (`return;` at `:1196`), so the busy reply
     must also `self.write` and `return`;
   - `Path` and `PathBuf` may need importing in `serve.rs`.

3. **`plugin/Client.qml`, `plugin/Composer.qml`, `plugin/Errors.js`: the attach UI.**
   - `Client.qml`:
     - `property var features: []`. In `onEvent` `case "hello"` (`:106`), set
       `root.features = Array.isArray(d.features) ? d.features : []`. Where
       `clearPending("disconnected")` runs (`:181`), set `root.features = []`.
     - Timeout at `:73`: a `send` whose `params.attachments` has entries gets 905000 ms;
       otherwise the existing expression stands.
   - `Errors.js`:
     - Add `"attachment": "Could not send the attachment."` to `messages`.
     - In `formatError` (`:47-52`), show `String(err.message)` when the code is
       `attachment` and there is a message, as for `internal`.
   - `Composer.qml` (`plugin/Composer.qml`, 150 lines; read it all first):
     - `import QtQuick.Dialogs`.
     - New state beside `drafts` (`:26`):
       - `property var files: ({})`: conversation id → array of `{ key, path, name }`;
       - `property int nextKey: 1`;
       - `property string pickFor: ""`;
       - `readonly property var currentFiles: root.files[root.conversationId] || []`;
       - `readonly property bool canAttach: root.client.features.indexOf("attachments") >= 0`.
     - Helpers. Each assigns a new object to `root.files`; never mutate it in place:
       - `setFiles(conv, list)` does
         `var f = Object.assign({}, root.files); f[conv] = list; root.files = f;`.
       - `addPicked(conv, urls)`:
         - For each url `u = String(url)`: if it starts with `file:///`, the path is
           `u.slice(7)`; if it starts with `file://localhost/`, the path is
           `u.slice(16)`; otherwise drop it.
         - `try { path = decodeURIComponent(path) } catch (e) { drop }`.
         - `name = path.split("/").pop()`.
         - Append `{ key: root.nextKey++, path, name }` while the list for `conv` has
           fewer than 10.
         - If anything was dropped, set `errorNote` (only when `conv` is still open) to
           `I18n.t("gmessages.attach_dropped", "Some files were not added (10 at most, local files only).")`.
       - `removeFile(conv, key)`.
     - `FileDialog { id: picker; fileMode: FileDialog.OpenFiles; onAccepted: root.addPicked(root.pickFor, selectedFiles) }`.
     - Attach button: a `Rectangle`, like the Send button (`:124-150`), placed before
       `inputField` in the `RowLayout`:
       - label `"\u{F0066}"`, the paperclip glyph already used in `Thread.qml:531`;
       - `visible: root.canAttach`;
       - enabled when not sending, a conversation is open and `currentFiles.length < 10`;
       - `onClicked`: `root.pickFor = root.conversationId; picker.open()`.
     - Chips: a `Flow`, placed above the `RowLayout`, with `visible: currentFiles.length > 0`,
       and a `Repeater { model: root.currentFiles }`. Each delegate has
       `required property var modelData` and `required property int index`, and shows a
       rounded rectangle with the plain-text name (elided, max ~180 px) and a `✕`
       `MouseArea` that calls `root.removeFile(root.conversationId, modelData.key)`.
     - `submit()` replaces `:45-80`:
       - Snapshot `conv`, `raw = String(inputField.text || "")`, `body = raw.trim()`,
         `reply = root.replyToId`, `sent = currentFiles.slice()`.
       - Return if sending, no `conv`, or `body === ""` with `sent.length === 0`.
       - If `sent.length > 0 && !root.canAttach`: set
         `errorNote = I18n.t("gmessages.attach_unsupported", "The background service can't send files; update it.")`
         and return.
       - `params = { conversation: conv, text: body }`; add `reply_to` as today; add
         `attachments: sent.map(f => f.path)` when there are files.
       - Callback:
         - It always sets `sending = false`.
         - Success:
           - remove every `key` in `sent` from `files[conv]`;
           - text: if `conv === root.conversationId`, clear `inputField.text` only when
             it `=== raw`; otherwise `delete root.drafts[conv]` only when
             `root.drafts[conv] === raw`;
           - clear `root.replyToId` only when `conv` is open and it still equals `reply`;
           - set the `accepted` status note only when `conv` is open.
         - Error: only when `conv` is open. `timeout`, `disconnected` and `send_uncertain`
           show the `send_uncertain` text; others show `Errors.formatError`. Never clear
           anything on error.
     - The Send button's `opacity` and `enabled`: active when not sending, a conversation
       is open, and there is trimmed text or `currentFiles.length > 0`.
     - Keep `onConversationIdChanged` (`:29-35`) as it is. `files` needs no swap, since
       `currentFiles` is a binding.
   → verify with `nix build .#plugin` (runs `qmllint`). Commit first if a new file is
   added (none expected).
   Traps:
   - the arrow function and `Object.assign` work in Qt 6 QML JS;
   - `FileDialog.selectedFiles` is a list of `url`, so convert with `String()`;
   - the TextField must not be disabled while sending (`:111` comment): it drops focus.

## Tests

```sh
nix develop -c sh -c 'cargo test --workspace && cargo clippy --all-targets -- -D warnings'
nix build .#plugin
```

Expected: all tests pass, including the new `read_files`, `sync` and `tests/serve.rs`
cases; clippy is clean; the plugin build's `qmllint` passes.

By hand on p620 (the owner; Claude does not build or deploy there):
- the picker opens from the panel;
- a photo, a PDF and a photo with a caption arrive on the phone;
- a directory or a file over 100 MiB is refused with its name;
- a failed send keeps the draft;
- an older daemon shows no attach button, and files picked before a reconnect to it are
  not sent;
- the same file picked twice, then sent while one chip is removed, leaves the right chips.
If the picker doesn't open from the panel, the spec's fallback is zenity through
`Quickshell.Io` `Process`, which needs a plan change and re-approval.

## Rollback

Revert the step commits on `feat/send-attachments`; nothing else depends on them. The
protocol changes are additive (`features`, `attachments`, a new error code), so a
reverted daemon still works with a new plugin, which then hides the attach button, and
the reverse.
