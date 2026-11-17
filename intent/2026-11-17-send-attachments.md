---
status: draft
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
   above the input as a row of chips (name, size), each one removable.
2. Send works with files and text, or files alone. The text is the caption.
3. While uploading, the composer shows that it is sending, and the input and chips stay
   as they were until the phone has taken the message, so a failure loses nothing.
4. A file that can't be read, is not a regular file or is too large is refused before
   anything is uploaded, with a message naming the file. An upload failure names the file
   too.
5. The sent message appears in the thread with its attachments, the same way received
   ones do (it arrives as an upsert from the phone, as text messages do today).
6. Images carry their width and height, so the phone and the other side can lay them out.

## Affected users and systems

- This repository: the JSON protocol (`src/proto.rs`), the daemon (`src/serve.rs`,
  `src/sync.rs`) and the plugin (`plugin/Composer.qml`, and maybe `plugin/Errors.js`).
- p620, the only host, by the usual flake update. No new service or permission.
- The owner, as the only user.

## Constraints

- The plugin passes local file paths, not file bytes. The plugin and the daemon run as
  the same user on the same machine, and the JSON socket should not carry megabytes of
  base64.
- The daemon is the trust boundary for these paths. It reads only regular files (no
  directories, devices or FIFOs), checks the size before reading, and never logs file
  contents. Paths and names are not logged at info level.
- Media keys stay in the daemon, as today.
- The socket protocol stays compatible: the new field is optional, and an old plugin
  without it works unchanged.
- Smallest change: no new crate unless the spec shows the standard library and the
  current dependencies (`imagesize` is already one) can't do it.
- `cargo test --workspace` and `cargo clippy --all-targets -- -D warnings` pass. Builds and
  deploys on p620 are the owner's.

## Open questions

1. Size limit per file and per message. Suggested: 100 MB per message, about what RCS
   accepts. The phone handles anything the carrier can't take (MMS may refuse or shrink
   it).
2. How many files per message. Suggested: up to 10.
3. The file picker. Qt's `QtQuick.Dialogs` FileDialog, if it loads inside Quickshell, or
   the desktop's own picker (xdg-desktop-portal) started as a process. The spec will test
   which works on p620 and pick.
4. First version scope. Suggested: the attach control only. Paste an image from the
   clipboard, drag-and-drop and inline previews of the chosen files come later.
