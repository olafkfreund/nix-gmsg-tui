---
status: draft
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui)
intent: intent/2026-11-17-inline-image-previews.md
---

# Spec: Show pictures inline in the conversation

## Design

Answers to the intent's open questions, as approved:
- previews load automatically when in view;
- the uploaded preview comes first, then the inline preview, and never the full file;
- animated images show their first frame;
- no video thumbnails;
- caps: about 240 logical px tall, 2 MiB, 40 MP, a 64 MiB cache and 2 downloads at a time;
- no re-encoding in the daemon.

The model is the avatar path that already exists (`src/phone.rs:1046` `avatars`,
`src/sync.rs:202` `avatar_result`, `src/sync.rs:1320` `save_avatar`). The daemon fetches
bytes, checks them by content and size, writes a private file, and QML shows it with
`Image`.

```
Thread delegate in view ─media.preview {message,index}─▶ serve ─▶ Sync::preview
  Sync: cache hit? reply path │ inline-only? check + write + reply │ else queue job
  queue (≤32, ≤2 running) ─▶ phone::Command::Preview{id, thumb_id, thumb_key}
  phone: stream download, stop at 2 MiB ─▶ Event::Previewed{id, bytes|error}
  Sync: check image ─▶ write media_dir/previews/<hash>.<ext> ─▶ evict to 64 MiB ─▶ reply path
Thread delegate destroyed ─media.preview.cancel─▶ drops the job if still queued
```

### 1. Protocol (`src/proto.rs`)

- `FEATURES` (`:12`) becomes `["attachments", "previews"]`.
- `Attachment` (`:131`) gains `width` and `height` (`i64`, with
  `#[serde(default, skip_serializing_if = "is_zero")]`), filled from `phone::Medium` in
  `Message::from_phone` (`:228-240`). The plugin uses them to size the bubble before the
  picture loads. Old plugins ignore them.
- New methods, both taking the existing `MediaParams { message, index }` (`:380`):
  - `media.preview` replies with `MediaData { path }`;
  - `media.preview.cancel` replies with `{}`.
- Errors use the existing codes:
  - `not_found` for "no such attachment" and "no preview" (not an image, no preview
    bytes, or bytes that fail the checks);
  - `phone_offline` when the phone isn't ready;
  - `busy` when the queue is full or the job was cancelled.

### 2. Daemon request handling (`src/serve.rs:1127`)

- `media.preview` follows the `media.open` arm: a pending entry with a 60 s deadline,
  then `self.sync…preview(rid, &message, index)`.
- `media.preview.cancel` calls `sync.cancel_preview(&message, index)` and replies `{}`
  straight away.

### 3. Sync: the preview path (`src/sync.rs`)

New state on `Sync`, initialised in `new`:
- `previews: HashMap<(String, u32), PathBuf>`: files written in this run;
- `preview_jobs: VecDeque<PreviewJob>`, where `PreviewJob { message, index, token,
  thumb_id, thumb_key }`;
- `preview_running: HashMap<u64, PreviewJob>`, keyed by token;
- `preview_waiters: HashMap<(String, u32), Vec<u64>>`: the rids waiting on one preview.

`pub fn preview(&mut self, rid, message, index) -> Vec<Out>`:
1. Find the stored message and its `index`-th valid `Medium`, filtered exactly as
   `media()` does (`:526-534`), so indexes match the plugin's list. If it is missing,
   reply `not_found` "no such attachment".
2. If `medium.mime` doesn't start with `image/`, reply `not_found` "no preview".
3. **Cache hit:** `previews[(message, index)]` exists and the file is still there. Reply
   with the path.
4. **Inline only:** if `thumb_id` is empty and `inline` isn't, check and write
   synchronously (step 6) and reply. If both are empty, reply `not_found` "no preview".
   The full file is never requested.
5. **Uploaded preview:** if not ready, reply `phone_offline`. Otherwise add `rid` to
   `preview_waiters[(message, index)]`, and queue a job if this key has no job queued or
   running yet (de-duplication). The queue holds at most 32 jobs; when it is full, reply
   `busy`. Then `pump_previews()`: while fewer than 2 are running, pop a job and send
   `phone::Command::Preview { id: token, thumb_id, thumb_key }`.
6. `fn accept_preview(&mut self, message, index, bytes) -> Result<PathBuf, &'static str>`:
   - `image_kind(&bytes)`, a magic-byte check split out of `save_avatar` (`:1324-1332`),
     plus GIF (`GIF87a` / `GIF89a`). It gives `jpg`, `png`, `webp` or `gif`, otherwise
     "no preview".
   - The length must be ≤ `PREVIEW_MAX` (2 MiB).
   - `imagesize::blob_size` must succeed, with `width * height ≤ 40_000_000` and each
     side ≤ 16384.
   - Write it with `write_media` to
     `media_dir/previews/<sha256(message ‖ 0 ‖ index ‖ "preview")>.<ext>`, creating the
     directory with `paths::create_private_dir` if needed. The name is distinct from
     `media.open`'s full/`-preview` files (`:576`), so the two never collide.
   - Record it in `previews`, then `evict_previews(&written)`.
   `save_avatar` keeps its own caps (256 KiB, 1024 px) and calls `image_kind` for the
   format.
7. `fn evict_previews(&mut self, keep: &Path)`: list `media_dir/previews`, and while the
   total is over 64 MiB, delete the oldest file by mtime other than `keep`, and drop it
   from `previews`. The plugin requests again only when a delegate is created, so an
   evicted visible picture stays drawn (Qt holds the decoded image) and there's no
   reload loop.
8. `phone::Event::Previewed { id, result: Result<Vec<u8>, String> }`:
   - take the job from `preview_running`;
   - on `Ok`, run `accept_preview`;
   - reply to every rid in `preview_waiters.remove(&key)`: the path, or `not_found` "no
     preview" (bad bytes), or `phone_offline` (download error);
   - then `pump_previews()`.

   A token not in `preview_running` (left from an old run) is ignored.
9. `pub fn cancel_preview(&mut self, message, index)`: if the key's job is still in
   `preview_jobs`, remove it, and reply `busy` "cancelled" to its waiters. A running
   job finishes, because cancelling a download mid-stream isn't worth the code, and only
   2 can run.
10. `forget(rid)` (`:686`) removes `rid` from every `preview_waiters` list. A job with no
    waiters left still finishes, and its file is reused.
11. On unpair or a store reset, the new `Sync` starts empty and `clear_media` already
    removes `media_dir` (`:804`). The `previews` map always matches the files on disk,
    because each lookup checks the file exists.

### 4. Phone (`src/phone.rs`)

- New `Command::Preview { id, thumb_id: String, thumb_key: Vec<u8> }`, dispatched in
  `run_commands` next to `Command::Download` (`:1118`):
  - `client.download_media_stream(&thumb_id, &thumb_key)` (`gmessages-proto/src/client.rs:630`);
  - then `next_chunk()` until the end, stopping with an error as soon as the total is
    over `PREVIEW_MAX`, so a lying server can't make us buffer more;
  - the whole thing runs under `tokio::time::timeout(30 s)`;
  - it emits `Event::Previewed { id, result }`.

  It never calls `full()` and never writes to the store.
- The error string is generic ("preview download failed"); the detail goes to
  `tracing::debug!`.

### 5. Plugin

- **`plugin/Client.qml`:** nothing new. `features` already comes from `hello` and is
  reset on disconnect.
- **`plugin/Thread.qml`:**
  - `readonly property bool canPreview: client.features.indexOf("previews") >= 0`.
  - Preview state lives outside delegates, in `root.ui`, keyed
    `"p:" + message + ":" + index`, as `{ path, busy, failed, tries }`. Delegates are
    rebuilt on every merge (`:120`).
  - **The image box,** placed above the name row inside each attachment's
    `ColumnLayout` (`:511`):
    - shown when `canPreview` and the attachment is `image/*`, and the message is
      neither `pending`, `failed` nor `deleted`;
    - its size is fixed before loading. With `width` and `height` known, the height is
      `min(240, h)` and the width is the matching aspect width, capped to the bubble's
      `78%` width minus padding (recomputing the height to keep the aspect). Without
      them, the box is `240 × 180`;
    - it holds an `Image` with `asynchronous: true`, `fillMode: PreserveAspectFit`,
      `autoTransform: true`, `cache: false`,
      `sourceSize: Qt.size(box.width * 2, box.height * 2)` (enough for HiDPI, and caps
      the decode), and `source` set to `"file://" + path` once it is known;
    - while loading, the box is a muted rectangle;
    - clicking it calls `root.mediaRequest("media.open", …)`, exactly as Open does;
    - an `Image` (not `AnimatedImage`) shows the first frame of a GIF or animated WebP.
  - **Pending messages** show the box with a muted placeholder and request nothing. The
    send-attachments `Accepted` fallback carries the media with its dimensions, so the
    box already has its final size. The confirmed message comes with a new id and loads
    then.
  - **When to request:** a delegate is in view when its rectangle intersects
    `msgList.contentY … contentY + height`. `msgList.cacheBuffer` is set to 0, so
    delegates exist only near the viewport. When `inView` becomes true and there is no
    state for the key, the delegate sends `media.preview`. On `Component.onDestruction`
    of a delegate whose key is still `busy`, it sends `media.preview.cancel`.
  - **Replies:**
    - a reply for a conversation that is no longer open is ignored. The conversation id
      is captured with the request, like `submit()` does in send-attachments;
    - `path`: stored, and the image loads;
    - `not_found`: `{ failed: true }`, and the box collapses to nothing, leaving the
      name row as today;
    - `busy` or `phone_offline`: retried once after 5 s if still in view (`tries < 2`),
      otherwise `failed`;
    - an `Image.Error` status also marks it `failed`.
  - **Clearing state:** `"p:"` entries are cleared on `openThread` of another
    conversation and when `client.connected` turns false. Unpair also disconnects.
  - **Scroll position:**
    - `mergeMessages` (`:86-118`) reads `var follow = msgList.atYEnd || msgList.count === 0`
      before replacing the array, and calls `positionViewAtEnd()` only when `follow` is
      true or `dropPending` is set (the initial load). That keeps the reading position
      when updates arrive while the user scrolls back;
    - while following, a height change in the last delegate (a box collapsing on
      failure) calls `positionViewAtEnd` again;
    - a box collapsing above the viewport isn't possible, since requests come only from
      delegates in view.

## Alternatives rejected

- **Reusing `media.open` for previews:** it calls `full()` first (`src/phone.rs:1653`),
  so it asks the phone to upload the full image, waits up to `FULL_MEDIUM_WAIT`, and
  caches full files in the store (`src/sync.rs:633`). That is the opposite of a
  preview.
- **Re-encoding thumbnails in the daemon:** it needs an image-decoding crate, and the
  parser risk only moves to the daemon. Rejected in the intent.
- **Base64 `data:` URLs in the protocol:** the NDJSON lines and queue cap
  (`MAX_QUEUED`, 8 MiB) would carry image bytes to every client, the bar included.
  Files in `media_dir` are how media already reach QML.
- **`AnimatedImage` for GIF:** animation costs CPU on every frame, and the intent chose
  the first frame.
- **Requesting previews for every delegate in the list:** with the default
  `cacheBuffer`, off-screen delegates would download too, and a full history would
  fill the queue. Only delegates in view request.
- **Cancelling running downloads:** with at most 2 running and a 2 MiB cap, a running
  job finishing is cheap, and stopping a stream mid-flight adds code for no real gain.

## Risks

- **Decoder flaws in the shell.** Qt's JPEG, PNG, GIF and WebP plugins run inside the
  Omarchy shell on bytes the sender controls. The format, size and dimension checks cut
  the attack surface; they don't remove it. A crash takes the bar down until the shell
  restarts. This is the residual risk the intent accepted.
- **Layout shift on failure.** A box that collapses after `not_found` changes the
  height of a delegate in view. Positions above don't move. When following the end, the
  list re-pins to the end.
- **Preview quality.** If the phone sends only a tiny inline preview, it is scaled up
  into the box and looks blurry. Open still shows the full picture.
- **Cache eviction.** It is by mtime in a 64 MiB directory, which costs one `read_dir`
  per written preview (a few dozen entries). That's cheap.
- **Mixed versions.** A new plugin with an old daemon sees no `"previews"` and shows
  name rows. An old plugin with a new daemon ignores the extra attachment fields and
  methods.

## Verification

- `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings` and
  `nix build .#plugin` pass.
- New unit tests in `src/sync.rs`:
  - `image_kind`: JPEG, PNG, WebP and GIF are accepted; a PNG named `.jpg` is accepted
    as `png`; text with an image MIME is refused;
  - `accept_preview` refuses:
    - bytes over 2 MiB;
    - a header claiming 50 MP (a hand-built PNG IHDR with a 10000 × 5000 size);
    - a header with a side over 16384;
    - truncated bytes that `imagesize` can't read;
  - `preview()`:
    - an unknown message or index gives `not_found`;
    - a non-image gives `not_found`;
    - inline-only gives a path straight away, with the file under `previews/`;
    - an uploaded preview queues one `Command::Preview`, and a second request for the
      same key adds a waiter without a second command;
    - a third different key while 2 are running is queued, not sent;
    - `Previewed` replies to all waiters and starts the queued job;
    - with the queue full (32 jobs), the next request gets `busy`;
  - `cancel_preview` removes a queued job and replies `busy` to its waiters, and leaves
    a running one alone;
  - `forget(rid)` removes the waiter;
  - `evict_previews` keeps the total ≤ 64 MiB, deletes the oldest first and never the
    file just written (using small files and a lowered budget passed in);
  - `Message::from_phone` carries `width` and `height`.
- `tests/serve.rs`: `hello.features` contains `"previews"`; `media.preview` with an
  empty message is `bad_request`; before pairing it gets the not-ready code of the other
  media methods.
- By hand on p620, after the deploy and a shell restart:
  - received and sent photos show inline;
  - clicking one opens the viewer;
  - a PDF still shows only its name row;
  - scrolling back through a picture-heavy chat stays smooth and doesn't jump when a
    new message arrives;
  - with the phone offline, the boxes fall back to name rows;
  - restarting the daemon with an old build (or reverting) hides the pictures.
