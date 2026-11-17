---
status: draft
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui)
author: olafkfreund
---

# Intent: Show pictures inline in the conversation

## Problem

A picture in a conversation shows only as a line with its file name, plus Open and Save
buttons (`plugin/Thread.qml:508-590`, plan b2). To see it, the user has to open it in an
external viewer. A picture sent with no caption is hard to recognise in the thread. The
owner reported such a bubble as "can not see what was sent". Plan b2 listed "inline image
previews" as out of scope, and so did intent `2026-11-17-send-attachments` (open question
5).

What exists to build on:
- `media.open` (`src/serve.rs:1127`, `src/sync.rs:567-600`) downloads an attachment and
  writes it to the private cache directory `media_dir`. It returns the path; media keys
  never leave the daemon. The best available version is used: the full file, the
  uploaded preview (`thumb_id`), or the small inline preview (`inline`,
  `src/phone.rs:652-654`). The file name ends in `-preview` when it isn't the full file.
- Messages reach QML with `attachments: [{name, mime, size}]` (`src/proto.rs:131`).
- `media_dir` is emptied when the daemon stops and on unpair. It has no size budget
  (plan b2, out of scope).

## Proposed outcome

1. An image attachment (`image/*`) shows as a picture inside its bubble, received or
   sent. The picture is scaled to fit the bubble, keeps its aspect ratio and has a capped
   height.
2. Clicking the picture opens it in the external viewer, as Open does today. The name,
   Open and Save stay available for every attachment.
3. Previews load only for messages in view and use the smallest version that looks
   right in the bubble. A small picture never triggers a full 100 MiB download just to
   draw a thumbnail.
4. While a preview loads, the bubble keeps its final size or a placeholder, so the list
   doesn't jump. If a preview can't be loaded or decoded, the bubble falls back to the
   current name row, with no error spam.
5. Other attachments (video, audio, PDF and the rest) look exactly as they do today.
6. Scrolling a long, picture-heavy chat stays smooth, and memory stays bounded: images
   are decoded at display size, not at full resolution.

## Affected users and systems

- This repository: the daemon (a way to ask for a preview), maybe the protocol, and
  the plugin (`plugin/Thread.qml`).
- p620, the only host, by the usual flake update and a shell restart.
- The owner, as the only user.

## Constraints

- Media keys stay in the daemon. QML gets file paths in `media_dir` only, as today.
- Image bytes come from the other side of the conversation and are untrusted. Decoding
  happens in the shell process through Qt's image plugins. Only common formats are
  decoded inline (JPEG, PNG, GIF, WebP); anything else stays a name row. The decode size
  is capped (`sourceSize`), so one image can't exhaust memory.
- Preview files live in `media_dir` (mode 0700, emptied at stop and on unpair), and
  nothing else is written to disk. A budget for `media_dir` is in scope if inline
  previews make it grow without bound during a long session.
- The protocol stays compatible both ways, as in the send-attachments work: an old
  plugin ignores anything new, and a new plugin works with an old daemon by not showing
  pictures.
- No message text, names or paths in logs above `debug`.
- Smallest change: no new crate unless the spec shows the current ones can't do it.
- `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings` and
  `nix build .#plugin` pass. Builds and deploys on p620 follow the usual bus
  announcement.

## Open questions

1. Load previews automatically when in view, or only on click ("tap to load")?
   Suggested: automatically. They come from the user's own phone through Google's relay,
   not from a third-party server, and that is the point of the feature.
2. Which version to show. Suggested: the inline preview when it is big enough for the
   bubble, otherwise the uploaded preview, and the full file only when there is no
   preview and it is at most a few MiB.
3. Animated GIFs: animate, or show the first frame? Suggested: the first frame; click
   opens it.
4. Video thumbnails: in scope? Suggested: no, a later step. The phone's preview for a
   video may be a still image, which would make this cheap, but that is unverified.
5. The maximum displayed height. Suggested: about 240 px, with the width following the
   bubble's 78% cap.
