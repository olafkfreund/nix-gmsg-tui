---
status: approved
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

What exists, and why it can't simply be reused:
- `media.open` (`src/serve.rs:1127`, `src/sync.rs:567-600`) writes an attachment to the
  private cache directory `media_dir` and returns its path; media keys never leave the
  daemon. But `download` (`src/phone.rs:1653`) always asks the phone for the **full**
  file first. A full download is also cached in the encrypted store
  (`src/sync.rs:633`). Run on every visible picture, that would turn scrolling into full
  downloads and grow the store.
- The cache file name tells "full" from "-preview" only, and `write_media` never
  replaces an existing file (`src/sync.rs:576`, `:1384`). Two kinds of preview would
  collide.
- Requests are de-duplicated per message, with no global limit (`src/sync.rs:549`), and
  a download is buffered whole before its size is checked (`src/phone.rs:1662`, b2 plan
  line 91).
- Messages reach QML as `attachments: [{name, mime, size}]` (`src/proto.rs:131`), with
  no dimensions. `Medium` in the daemon has `width`/`height` (`src/phone.rs:271`) when
  the phone reports them.
- `mergeMessages` scrolls to the end on every update (`plugin/Thread.qml:112`).
- `media_dir` is emptied at stop and on unpair, and has no size budget (b2, out of
  scope).

## Proposed outcome

1. An image attachment shows as a picture inside its bubble, for received images and
   for sent ones (from the phone or from the panel). It is scaled to fit, keeps its
   aspect ratio, and its height is capped.
2. Clicking the picture opens it in the external viewer, as Open does today. Name, Open
   and Save stay available for every attachment, and they behave exactly as they do now.
3. Previews use a **separate preview path** that never asks the phone for the full file
   and never writes to the store's attachment cache. Each preview variant has its own
   cache name, and a preview is never stored or served as the original.
4. Before a picture loads, its bubble already has its final size whenever the dimensions
   are known, or a fixed placeholder size when they aren't, so loading doesn't move the
   list. If a preview fails, the bubble falls back to today's name row, and the reading
   position is kept.
5. Reading older messages is not interrupted. The list keeps its position while
   previews load or fail, while messages update and when the window is resized. It
   follows new messages to the end only when the user was already at the end.
6. Resources stay bounded:
   - a small global number of preview downloads at a time;
   - a bounded queue;
   - requests for messages scrolled out of view or for a closed chat are dropped;
   - a byte cap on the preview cache in `media_dir`, with the oldest files evicted first,
     without endless reload loops for visible pictures;
   - decoded images kept only for delegates in view.
7. Each preview is de-duplicated, a failure is retried a bounded number of times, a
   reply that arrives after the chat changed is ignored, and previews are cleared on
   unpair and when the daemon disconnects.
8. A picture still being sent shows a placeholder until the phone confirms the message,
   then loads under the confirmed id. Failed or deleted messages show no picture.
9. Other attachments (video, audio, PDF and the rest) look exactly as they do today.
10. Mixed versions work: the daemon advertises a new feature (e.g. `"previews"` in
    `hello.features`, next to `"attachments"`), and the plugin requests previews only
    when the current connection offers it, including after a reconnect to an older
    daemon.

## Affected users and systems

- This repository: the daemon (the preview path, its limits and the cache budget), the
  protocol (the feature flag, maybe dimensions in the attachment data) and the plugin
  (`plugin/Thread.qml`, `plugin/Client.qml`).
- p620, the only host, by the usual flake update and a shell restart.
- The owner, as the only user.

## Constraints

- **Untrusted image data, decoded in the shell process.** The bytes come from whoever
  sent the message. Decoding them with Qt's image plugins inside the Omarchy shell is a
  deliberate choice, and a decoder bug there could crash or compromise the shell. To
  lower that risk, the daemon hands over a preview only if:
  - its **content** (magic bytes, not the MIME type or file name) is JPEG, PNG, GIF or
    WebP;
  - its byte size is under a cap;
  - its pixel dimensions, read from the header without decoding (`imagesize`, already a
    dependency), are under a cap.

  Anything else stays a name row. `sourceSize` limits the decoded result in QML, but it
  isn't relied on as a security bound. The residual risk (Qt decoder flaws) is recorded
  in the spec.
- Media keys stay in the daemon. QML only gets file paths in `media_dir` (0700, emptied
  at stop and on unpair).
- No message text, names or paths in logs above `debug`.
- Smallest change: no new crate unless the spec shows the current ones can't do it.
- `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings` and
  `nix build .#plugin` pass. On top of those, targeted tests cover:
  - mislabelled and malformed images, and images over the byte or pixel caps;
  - the cache budget and eviction, and the concurrency and queue limits;
  - the offline fallback;
  - mixed versions and reconnects;
  - pending sends;
  - scroll position during updates.
- p620 deploys follow the usual bus announcement.

## Open questions

1. Load automatically when in view, or tap to load? Suggested: automatically, given the
   limits above. That the bytes come through Google's relay doesn't make them safe;
   the format and size checks are what limit the risk.
2. Which version to show. Suggested: the uploaded preview if there is one, then the
   inline preview, and never the full file on the preview path. When neither preview
   exists, show the name row. Open still works.
3. Animated formats (GIF, animated WebP): suggested first frame only, and click opens
   them.
4. Video thumbnails: suggested out of scope for now.
5. Sizes. Suggested starting values for the spec to confirm:
   - a displayed height of about 240 logical px, with the width inside the bubble's 78%
     cap minus padding;
   - a preview byte cap of 2 MiB;
   - a pixel cap of 40 MP;
   - a cache budget of 64 MiB;
   - 2 concurrent preview downloads.
6. Alternative to decoding in the shell: the daemon re-encodes every preview into a safe
   thumbnail. That needs an image-decoding crate in the daemon, so the parser risk moves
   to the daemon. Suggested: not now; checks on the format and the caps, as above.
