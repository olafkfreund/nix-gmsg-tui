---
status: draft
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
- With attachments, the pending deadline is 600 s instead of 120 s (`serve.rs:1191`).
  That allows 100 MiB over a slow uplink (about 1.4 Mbit/s).
- `sync.send` (`src/sync.rs:1192`) takes `attachments: Vec<PathBuf>` and passes it on.

### 3. Reading files (`src/phone.rs`)

- `Command::Send { attachments: Vec<Attachment> }` becomes `files: Vec<PathBuf>`.
  `phone::Attachment` stays as it is: the download path uses it (`src/sync.rs:600`).
- A new plain function, `fn read_files(paths: &[PathBuf]) -> Result<Vec<Attachment>, String>`,
  runs in `tokio::task::spawn_blocking`. Reading 100 MiB must not stall the event loop,
  and the `rt` feature already provides it. For each path, in order:
  1. `std::fs::metadata(path)` must be a regular file. This runs before `open`, because
     opening a FIFO for reading blocks until something writes to it. Following symlinks is
     intended.
  2. `File::open`, then `file.metadata()` on the open handle: still a regular file, and
     the length is not 0. Checking the handle catches a path swapped since step 1.
  3. Read through `file.take(remaining + 1)`, where `remaining` is 100 MiB minus the bytes
     already read for this message. Reading more than `remaining` means "too large". A
     file that grew after step 2 can't push the read past the limit.
  4. Name: the path's base name with control and bidi characters removed (the same
     character set as `save_download`, `src/sync.rs:1386`), falling back to `file`.
  5. MIME: a `const` table keyed on the lowercase extension (jpg/jpeg, png, gif, webp,
     heic, heif, bmp, mp4, 3gp, mov, webm, mkv, mp3, m4a, aac, ogg, opus, amr, wav, pdf, txt,
     vcf, zip), falling back to `application/octet-stream`. No new crate.
  6. For `image/*`, width and height come from `imagesize::blob_size(&data)`. If it
     fails, they stay `None` and the file is sent without a size.
- An error stops the send before any upload. Errors read `<name>: cannot read`,
  `<name>: not a file`, `<name>: empty` or `<name>: too large (over 100 MiB in all)`.
  Full paths and OS error texts stay in a `debug` log line only.
- `MEDIA_MAX` (`src/phone.rs:1475`, also 100 MiB) is the limit, so downloads and uploads
  share one constant.

### 4. Upload and send (`src/phone.rs:1388`)

- `SendOutcome` gains `Attachment(String)`. File errors map to it, and so do upload
  failures, as `<name>: upload failed` with the error text in a `warn` log. Both used to
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
  array when absent). A `send` with attachments gets a 605 s deadline; plain sends keep
  125 s (`Client.qml:73`).
- **`plugin/Composer.qml`:**
  - `property var files: ({})` maps conversation id to an array of
    `{ path, name }`, beside the existing `drafts` (`Composer.qml:26`). Switching chats
    keeps each chat's files.
  - An attach button left of the input, shown only when
    `client.features.indexOf("attachments") >= 0`, disabled while sending or at 10
    files. It opens a `QtQuick.Dialogs` `FileDialog` with
    `fileMode: FileDialog.OpenFiles`. The conversation id is captured when the dialog
    opens, and the picked files go to that conversation even if the user switched away.
    Cancelling does nothing.
  - URL to path: `decodeURIComponent(String(url).replace(/^file:\/\//, ""))`. Anything
    that isn't a `file://` URL is dropped.
  - Chips above the input show the name and a remove button (✕).
  - `submit()` sends when there is text or at least one file. On success it removes the
    sent paths from that conversation's list, clears the text only if it still equals
    what was sent, and clears `replyToId` as today.
  - On `timeout` or `disconnected`, the error note shows the `send_uncertain` text and
    the draft stays. Every error keeps the draft, as today. Nothing is resent
    automatically.
  - The Send button is enabled when there is text or a file.
- **`plugin/Errors.js`:** `attachment` is added to the message list, and `formatError`
  shows `err.message` for it, the same way it does for `internal`.

### Deviation from the intent, for approval

Intent outcome 1 puts the size on each chip. QML can't read a file's size without a new
daemon method, so the chips show the name only, and the size is checked when sending
(outcome 5). Adding a `files.check` method that returns each file's size and refuses bad
files when they are picked would be about 40 more lines. Not proposed, unless you want it.

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
- **A daemon-side lock allowing one upload at a time:** the composer already allows one
  send at a time (`root.sending`), and only the app role sends.

## Risks

- **The picker might not show from the panel.** The `qtengine` platform theme decides
  whether `FileDialog` uses the desktop portal or Qt's QML fallback. The fallback needs a
  parent window and may sit badly with a layer-shell panel. Proven only by hand on p620.
  If it fails, switch to zenity as above (one small plan change).
- **Memory:** one send holds up to 100 MiB of plain bytes, plus the encrypted copy while
  each file uploads (`gmessages-proto/src/media.rs:41`), so about 200 MiB at the peak, and
  more if a timed-out send is still uploading when the next one starts. Accepted for a
  single-user desktop daemon.
- **Timed-out sends keep going:** `forget` drops only the bookkeeping
  (`src/sync.rs:675`). The user sees "may not have been sent", and the message may still
  arrive. That is the existing behaviour for text, now more likely with large files.
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
  - a name with control characters, cleaned;
  - an unknown extension, sent as `application/octet-stream`.
- New tests in `tests/serve.rs` over the real socket:
  - `hello` carries `features: ["attachments"]`;
  - a send with no text and no attachments is `bad_request`;
  - 11 attachments are `bad_request`;
  - a relative path is `bad_request`;
  - a send with attachments only, before pairing, gets the same not-ready reply as text.
- A unit test that `sync` maps `SendOutcome::Attachment` to `ErrorCode::Attachment`.
- `nix build .#plugin`: its `qmllint` install check passes.
- By hand on p620 (the owner): the picker opens from the panel; a photo, a PDF and a
  photo with a caption arrive on the phone; a directory or a file over 100 MiB is refused
  with its name; the composer keeps the draft on failure; an old daemon shows no attach
  button.
