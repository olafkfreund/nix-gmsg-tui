---
status: approved
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui)
author: olafkfreund
---

# Intent: Send pictures and files from the panel

## Problem

The panel can only send text. Attachments on received messages can be opened and saved
(plan b2), but there is no way to send a picture or a file. Plan b2 listed "sending
attachments" as out of scope.

The lower layers already do the hard part:
- `gmessages-proto/src/client.rs:617`: `upload_media(data, name, mime)` encrypts and
  uploads a file and returns a `Media`. `send_message` sends each one as a message part,
  with the text as the caption.
- `src/phone.rs:1388`: `send()` already takes `attachments: Vec<Attachment>`, uploads each
  one and reports an upload failure as `SendOutcome::Unavailable`.
- `plugin/Client.qml:73`: a `send` request already waits up to 125 s.

The gaps sit above that:
- `src/proto.rs:342`: `SendParams` has no field for files.
- `src/serve.rs:1179`: a send without text is refused, so a picture alone can't go out.
- `src/sync.rs:1192`: `send()` always passes `attachments: Vec::new()`.
- `plugin/Composer.qml`: no way to choose a file, and nothing shows what will be sent.

## Proposed outcome

1. The composer has an attach control. It opens a file picker, and the chosen files show
   above the input as a row of chips (name, size), each one removable. Cancelling the
   picker changes nothing.
2. Send works with files and text, or files alone. The text is sent with the files (as a
   separate text part, the way `send_message` builds it; how the other side shows it is
   checked on the phone).
3. Chosen files belong to their conversation, like the text draft does
   (`plugin/Composer.qml:25`). Switching chats keeps each one's files. A finished send
   clears only what was sent, not anything chosen or typed meanwhile.
4. While uploading, the composer shows it is sending. On failure, the text and chips stay.
   On a timeout or a lost connection, the composer says the message may have been sent
   (as `send_uncertain` does today) and keeps the draft, and nothing is resent on its own.
   The deadlines on both sides are long enough for the largest allowed message.
5. Every file is checked before the first upload. One that can't be read, is not a regular
   file, is empty or is too large refuses the whole send, with a message naming that file
   (its base name, never the full path). An upload failure names the file too, and is
   not reported as "phone offline".
6. The sent message appears in the thread with its attachments, the same way received
   ones do. When the phone accepts it without an echo (the fallback at
   `src/phone.rs:1465`), the pending message shows its attachments until the conversation
   fetch replaces it.
7. Images whose size `imagesize` can read carry their width and height to the phone. Any
   other file, or an image it can't read, goes as a plain file.
8. A new plugin talking to an old daemon does not offer attachments. It can't send files
   that the daemon would silently drop while sending the text: `SendParams` ignores unknown
   fields today (`src/proto.rs:342`).

## Affected users and systems

- This repository: the proto crate, the daemon and the plugin (files listed under
  Constraints).
- p620, the only host, by the usual flake update. No new service or permission.
- The owner, as the only user.

## Constraints

- The plugin passes local file paths, not file bytes. The plugin and the daemon run as
  the same user on the same machine, and the JSON socket should not carry megabytes of
  base64. File URLs from the picker are turned into paths in the plugin.
- The daemon is the trust boundary for these paths. Any path the user can read may be
  sent, as with any desktop app sending a file the user chose. The daemon:
  - opens the file first and checks the open handle (a regular file, its size), so the
    path can't be swapped between check and read;
  - follows symlinks, since the handle check covers where they lead;
  - reads at most the limit, so a file that grows after the check is refused, not read
    whole;
  - never logs file contents, and logs paths and names only at debug level.
- Memory: the limits bound one send. Sends run one at a time (the composer allows one in
  flight), so concurrent sends can't add up.
- Media keys stay in the daemon, as today.
- The socket protocol stays compatible both ways: the new field is optional, so an old
  plugin works unchanged, and outcome 8 covers the other direction.
- Smallest change: no new crate unless the spec shows the standard library and the
  current dependencies (`imagesize` is already one) can't do it.
- Files touched include `gmessages-proto/src/media.rs` or `client.rs` (image size),
  `src/phone.rs` (error names, the fallback message), `src/proto.rs`, `src/serve.rs`,
  `src/sync.rs`, `plugin/Composer.qml`, `plugin/Client.qml`, `plugin/Errors.js`.
- `cargo test --workspace` and `cargo clippy --all-targets -- -D warnings` pass. Builds and
  deploys on p620 are the owner's.

## Open questions

1. Limits. Suggested: 100 MiB per message in total, no separate per-file cap, empty files
   refused. That is our own cap, not a known carrier limit: RCS and MMS limits vary, and
   what the phone does with a file too big for MMS is checked on the phone, not assumed.
2. How many files per message. Suggested: up to 10.
3. File type. Suggested: the daemon picks the MIME type from the file extension through a
   small built-in table, falling back to `application/octet-stream`. File contents are not
   sniffed.
4. The file picker. Qt's `QtQuick.Dialogs` FileDialog, if it loads inside Quickshell, or
   the desktop's own picker through xdg-desktop-portal. The spec will try both on p620 and
   pick one.
5. First version scope. Suggested: the attach control only. Paste an image from the
   clipboard, drag-and-drop and inline previews of the chosen files come later.
