---
status: approved
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui)
intent: intent/2026-11-17-send-attachments.md
---

# Spec: Send pictures and files from the panel

## Design

Answers to the intent's open questions, as approved: 100 MiB per message in total,
empty files refused, at most 10 files, MIME type from the extension, attach control only.

### Flow

```
Composer (FileDialog → paths) ──send {conversation, text, attachments:[paths]}──▶ serve
serve: shape checks ──▶ sync.send(…, paths) ──▶ phone::Command::Send { files }
phone send(): read + check all files (spawn_blocking) ──▶ upload each ──▶ send_message
```

### 1. Protocol (`src/proto.rs`)

- `SendParams` gains `#[serde(default)] pub attachments: Vec<String>`: absolute local
  paths. Old plugins don't send it, so nothing changes for them.
- `HelloEvent` gains `#[serde(default)] pub features: Vec<String>`. The daemon sends
  `["attachments"]`. This is how a new plugin knows not to offer files to an old daemon
  (intent outcome 8). `PROTOCOL` stays 1: bumping it would lock an old plugin out
  entirely, which is more than this needs.
- `ErrorCode` gains `Attachment` (`"attachment"`). Its `message` is shown to the user as
  is, and names the file by its base name, e.g. `photo.jpg: too large`.

### 2. Daemon request checks (`src/serve.rs:1178`)

- Refused with `bad_request` before anything starts: an empty `conversation`; neither
  text nor attachments; more than 10 attachments; a path that is empty or not absolute.
- With attachments, the pending deadline is 900 s instead of 120 s (`serve.rs:1191`).
  That covers 100 MiB at about 1 Mbit/s plus the reads, the send retries (3, 8 and 20 s,
  `gmessages-proto/src/client.rs:403`) and the echo wait. The HTTP client sets only a 30 s
  connect timeout (`gmessages-proto/src/conn.rs:127`), so no lower layer cuts an upload
  short. Below about 1 Mbit/s, the composer reports "may not have been sent", and the
  message may still arrive.
- **One upload at a time, enforced by the daemon:** `Sync` gets
  `uploading: Option<u64>` (the rid). A send with attachments while it is `Some` is
  refused with `busy`. It is set when the send is dispatched and cleared only in the
  `phone::Event::Sent` handler for that rid (`src/sync.rs:1018`), not in `forget`
  (`src/sync.rs:675`). So a timed-out or disconnected send still holds it until the
  phone task really ends, and that bounds memory whatever the clients do. Text-only
  sends are not affected.
- `sync.send` (`src/sync.rs:1192`) takes `attachments: Vec<PathBuf>` and passes it on.

### 3. Reading files (`src/phone.rs`)

- `Command::Send { attachments: Vec<Attachment> }` becomes `files: Vec<PathBuf>`.
  `phone::Attachment` stays as it is: the download path uses it (`src/sync.rs:600`).
- A new plain function, `fn read_files(paths: &[PathBuf]) -> Result<Vec<Attachment>, String>`,
  runs in `tokio::task::spawn_blocking`. Reading 100 MiB must not stall the event loop,
  and the `rt` feature already provides it. For each path, in order:
  1. Name first: the path's base name with control and bidi characters removed (the same
     character set as `save_download`, `src/sync.rs:1386`), falling back to `file`. Every
     error below uses this cleaned name.
  2. Open with `OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK)`. `libc` is
     already a dependency. A FIFO or device swapped in at any moment can't block the
     open, and `O_NONBLOCK` doesn't change reads of a regular file. Symlinks are followed
     on purpose.
  3. `file.metadata()` on the open handle: it must be a regular file with a length above
     0. Because only the handle is checked, there is no gap between a check and the read.
  4. Read through `file.take(remaining + 1)`, where `remaining` is 100 MiB minus the bytes
     already read for this message. Reading more than `remaining` means "too large", and
     reading 0 bytes means "empty" (a file truncated after step 3). A file that grows
     can't push the read past the limit.
  5. MIME: a `const` table keyed on the lowercase extension (jpg/jpeg, png, gif, webp,
     heic, heif, bmp, mp4, 3gp, mov, webm, mkv, mp3, m4a, aac, ogg, opus, amr, wav, pdf, txt,
     vcf, zip), falling back to `application/octet-stream`. No new crate.
  6. For `image/*`, width and height come from `imagesize::blob_size(&data)`. If it
     fails, they stay `None` and the file is sent without a size.
- An error stops the send before any upload. Errors read `<name>: cannot read`,
  `<name>: not a file`, `<name>: empty` or `<name>: too large (over 100 MiB in all)`.
  Full paths, names and OS error texts go only to a `debug` log line.
- `MEDIA_MAX` (`src/phone.rs:1481`, also 100 MiB) is the limit, so downloads and uploads
  share one constant.

### 4. Upload and send (`src/phone.rs:1388`)

- `SendOutcome` gains `Attachment(String)`. File errors map to it, and so do upload
  failures, as `<name>: upload failed`. The `warn` log line says only "attachment upload
  failed", and the name and the error text go to `debug`, as the intent requires. Both used to
  be `Unavailable`, which shows as "phone offline". `sync.rs:409` maps it to
  `ErrorCode::Attachment`.
- After `upload_media`, the returned `Media` gets
  `dimensions: Some(Dimensions { width, height })` when they are known. The wire
  conversion already carries them (`gmessages-proto/src/types.rs:958`), so the proto
  crate does not change.
- The upload loop keeps `stored_medium(&media, None)` for each file. The no-echo fallback
  message (`src/phone.rs:1465`) uses those instead of `Vec::new()`, so a pending
  picture-only message shows its attachments until the fetch replaces it
  (intent outcome 6). `proto::Message::from_phone` already strips the keys.

### 5. Plugin

- **`plugin/Client.qml`:** stores `features` from `hello` (`root.features`, an empty
  array when absent). It resets `features` to `[]` on disconnect, next to
  `clearPending("disconnected")` (`Client.qml:181`). That way a reconnect to an old
  daemon can't inherit the new daemon's features. A `send` with attachments gets a
  905 s deadline; plain sends keep 125 s (`Client.qml:73`).
- **`plugin/Composer.qml`:**
  - `property var files: ({})` maps a conversation id to an array of
    `{ key, path, name }`, beside the existing `drafts` (`Composer.qml:26`). `key` comes
    from a counter (`nextKey++`), so the same file picked twice gets two entries. Every
    change assigns a new object (`root.files = Object.assign({}, root.files, { [conv]: list })`)
    so the bindings for the chips, the count and the Send button update. Mutating it in
    place would not notify them. `currentFiles` is a binding on
    `root.files[root.conversationId] || []`.
  - An attach button left of the input, shown only while
    `client.features.indexOf("attachments") >= 0`, and disabled while sending or at 10
    files. It opens a `QtQuick.Dialogs` `FileDialog` with
    `fileMode: FileDialog.OpenFiles`. The conversation id is captured when the dialog
    opens, and the picked files go to that conversation even if the user switched away.
    Each result is cut to the slots that conversation has left at the moment it is
    accepted, so one dialog can't add an 11th file. If any were dropped, a note says so.
    Cancelling does nothing.
  - URL to path: only `file:///…` and `file://localhost/…` are accepted. The prefix is
    removed up to the third `/`, keeping the leading `/`, then `decodeURIComponent` runs
    inside `try`. A URL that fails to decode, or any other scheme or host, is dropped
    with a note.
  - Chips above the input show the name and a remove button (✕).
  - `submit()`:
    - It needs text or at least one file.
    - With files, it also needs `attachments` in the current `client.features`. If that
      is missing, it keeps everything and shows "the background service can't send files;
      update it". This covers a reconnect to an old daemon with files already picked.
    - It snapshots `conv`, the raw text, the trimmed text it sends, the reply id and the
      files' `key`s.
    - On success, it removes only those `key`s from `files[conv]`. It clears the text
      only if it still equals the raw snapshot: the input when `conv` is still open, else
      `drafts[conv]`. This replaces the unconditional `delete root.drafts[conv]` at
      `Composer.qml:66`. It clears `replyToId` only if it still equals the snapshot.
    - Status and error notes from a send are shown only if `conv` is still the open
      conversation, so a reply about chat A never shows under chat B.
  - On `timeout` or `disconnected`, the error note shows the `send_uncertain` text and
    the draft stays. Every error keeps the draft, as today. Nothing is resent
    automatically.
  - The Send button is enabled when there is text or a file.
- **`plugin/Errors.js`:** `attachment` is added to the message list, and `formatError`
  shows `err.message` for it, the same way it does for `internal`.

### Deviation from the intent, for approval

Intent outcome 1 puts the size on each chip. QML has no API to read a file's size: `FileDialog`
returns URLs only, and `Quickshell.Io` `FileView` reads contents. So the chips show the
name only, and the size is checked when sending (outcome 5). Showing the size would take
a new daemon method that returns a file's size and refuses a bad file when it is picked.
Not proposed, unless you want it.

## Alternatives rejected

- **File bytes over the socket (base64):** about 133 MiB of JSON per message through the
  NDJSON line protocol and its per-connection queue cap (`MAX_QUEUED`,
  `src/serve.rs:661`). Paths are enough, since both ends run as the same user.
- **zenity or yad as the picker**, started through `Quickshell.Io` `Process`: both are
  installed, but the plugin would need a new runtime dependency substituted into
  `nix/plugin.nix`, as `notify-send` is. Tested here: `QtQuick.Dialogs` comes with
  Quickshell 0.3.2 (`qtdeclarative-6.12.0`) and a `FileDialog` loads in a test shell. This
  stays the fallback if the dialog won't show from the panel window (see Risks).
- **Bumping `PROTOCOL` to 2:** an old plugin with a new daemon (or the reverse) would be
  locked out until both update. `features` degrades gracefully instead.
- **Reading the files in `serve` or `sync`:** that runs on the event loop that serves
  every client, so a 100 MiB read would freeze the bar and the panel.
- **`mime_guess` or sniffing file contents:** a new crate for a table of about 25
  entries, and sniffing adds disagreement cases the phone resolves anyway.
- **Relying on the composer's `root.sending` guard alone:** the daemon doesn't check
  which role sends, and a timed-out send keeps running after `forget`. Only a daemon-side
  guard that is released when the send really finishes bounds memory (section 2).
- **Checking the path, then opening it** (metadata before `open`): it leaves a gap in
  which a FIFO can be swapped in and block `open`. The `O_NONBLOCK` open plus a handle
  check has no such gap.

## Risks

- **The picker might not show from the panel.** The `qtengine` platform theme decides
  whether `FileDialog` uses the desktop portal or Qt's QML fallback. The fallback needs a
  parent window and may sit badly with a layer-shell panel. Proven only by hand on p620.
  If it fails, switch to zenity as above (one small plan change).
- **Memory:** one send holds up to 100 MiB of plain bytes, plus the encrypted copy of the
  file being uploaded (`gmessages-proto/src/media.rs:41`), so about 200 MiB at the peak.
  The daemon guard keeps it to one such send at a time. Accepted for a single-user
  desktop daemon.
- **A slow uplink:** below about 1 Mbit/s, 100 MiB can outlast the 900 s deadline. The
  user sees "may not have been sent", and the guard refuses another file send with
  `busy` until the first one ends.
- **Timed-out sends keep going:** `forget` drops only the bookkeeping
  (`src/sync.rs:675`). The user sees "may not have been sent", and the message may still
  arrive. That is the existing behaviour for text, now more likely with large files.
- **`qmllint` proves syntax only:** the install check disables import and type
  resolution (`nix/plugin.nix:37`). `FileDialog` working at runtime is proven only by
  hand.
- **Carrier limits:** an MMS conversation may refuse or shrink a large file. The phone
  decides; we report its `Rejected` result as today.
- **`hello` consumers:** the new field is additive, and the `tests/serve.rs` checks read
  single keys, not the whole object.

## Verification

- `cargo test --workspace` and `cargo clippy --all-targets -- -D warnings` pass.
- New unit tests in `src/phone.rs` for `read_files`, against temp files:
  - a regular file, read with the right name, MIME type and image size;
  - a directory, refused as "not a file";
  - an empty file, refused;
  - a missing file, refused as "cannot read";
  - two files over 100 MiB together, refused as "too large" with nothing uploaded
    (`read_files` returns an error);
  - a name with control characters, cleaned, in a "cannot read" error too;
  - an unknown extension, sent as `application/octet-stream`;
  - a FIFO (`libc::mkfifo` in a temp dir), refused as "not a file" without blocking: the
    test runs under a 5 s timeout.
- New tests in `tests/serve.rs` over the real socket:
  - `hello` carries `features: ["attachments"]`;
  - a send with no text and no attachments is `bad_request`;
  - 11 attachments are `bad_request`;
  - a relative path is `bad_request`;
  - a send with attachments only, before pairing, gets the same not-ready reply as text.
- Unit tests in `src/sync.rs`:
  - `SendOutcome::Attachment` maps to `ErrorCode::Attachment`;
  - a second send with attachments while one is in flight is `busy`; after `forget` it is
    still `busy`; after that rid's `Event::Sent` it is allowed;
  - an `Accepted` fallback with media gives a `message` event whose `attachments` list
    them.
- `nix build .#plugin`: its `qmllint` install check passes.
- By hand on p620 (the owner): the picker opens from the panel; a photo, a PDF and a
  photo with a caption arrive on the phone; a directory or a file over 100 MiB is refused
  with its name; the composer keeps the draft on failure; an old daemon shows no attach
  button, and files picked before a reconnect to an old daemon are not sent; picking the
  same file twice, then sending while removing one chip, leaves the right chips.

## Amendment (2026-11-17, after deploy)

Pictures sent with `dimensions` set on the media part reached the phone as text only: the
phone dropped the media part (owner-confirmed on the phone, two sends). mautrix-gmessages,
whose sends work, sets no dimensions; everything else on the wire matches it (message
layout, upload, `MediaContent` fields, chunked AES-GCM). So `send()` no longer sets
`Media::dimensions`. Intent outcome 6 ("images carry width and height") is dropped by the
owner's decision. `read_files` still reads the size; it is harmless and kept for the tests.
