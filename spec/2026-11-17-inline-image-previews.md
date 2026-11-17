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
- caps: about 240 logical px tall, 2 MiB, 40 MP, a 64 MiB cache, 2 downloads at a time;
- no re-encoding in the daemon.

The model is the avatar path that already exists (`src/phone.rs:1046` `avatars`,
`src/sync.rs:202` `avatar_result`, `src/sync.rs:1320` `save_avatar`). The daemon fetches
bytes, checks them by content and size, writes a private file, and QML shows it with
`Image`.

The key simplification is that **a picture box never changes height once shown.** The
daemon tells the plugin up front which attachments have a preview at all, and their
size when known. The box is drawn at its final size, and stays that size while loading,
when loaded and when the download fails (then it shows a muted "no preview" glyph; the
name row underneath is always there). Loading therefore never moves the list, which
removes most of the scroll-anchoring problem.

```
Thread (in-view set) ─media.preview {message,index}─▶ serve ─▶ Sync::preview
  Sync: cache hit (file exists)? reply │ join existing job │ queue (≤32) │ run ≤2
  phone::Command::Preview{id, thumb_id, thumb_key} ─▶ stream ≤2 MiB ─▶ Event::Previewed
  Sync: check image (else inline fallback) ─▶ previews/<hash>.<ext> ─▶ evict to 64 MiB ─▶ reply
Thread (debounced) ─media.preview.cancel {message,index}─▶ drops this connection's waiters
```

### 1. Protocol (`src/proto.rs`)

- `FEATURES` becomes `["attachments", "previews"]`.
- `Attachment` (`:131`) gains three fields, filled in `Message::from_phone` (`:228`), all
  `#[serde(default, skip_serializing_if = …)]`:
  - `width` and `height` (`i64`) from `Medium`, sent only when both are in
    `1..=16384`;
  - `preview: bool`, true when the mime is `image/*` and `thumb_id` or `inline` is
    non-empty.

  Old plugins ignore all three.
- New methods taking the existing `MediaParams { message, index }`:
  - `media.preview` replies with `MediaData { path }`;
  - `media.preview.cancel` replies with `{}`.
- Errors use the existing codes:
  - `not_found`: no such attachment, or no usable preview;
  - `phone_offline`: not ready, or the download failed with no inline fallback;
  - `busy`: the queue is full, the job was cancelled, or it waited too long in the queue.

### 2. Daemon request handling (`src/serve.rs`, next to `media.open` at `:1127`)

- `media.preview` adds a pending entry with a 60 s deadline, then calls
  `sync.preview(rid, &message, index)`.
- `media.preview.cancel`: serve collects the rids in `self.pending` whose `conn` is this
  connection, calls `sync.cancel_preview(&message, index, &rids)` and replies `{}`.
  That way one connection can never cancel another's request.
- Both refuse an empty `message` with `bad_request`.

### 3. Sync: the preview path (`src/sync.rs`)

**State:**
- `previews: HashMap<(String, u32), PathBuf>`;
- `preview_jobs: VecDeque<PreviewJob>`;
- `preview_running: HashMap<u64, PreviewJob>`, keyed by token;
- `preview_waiters: HashMap<(String, u32), Vec<u64>>`.

`PreviewJob { key, token, thumb_id, thumb_key, inline: Vec<u8>, queued_at: Instant }`.
Tokens come from the existing `next_token` counter.

**`preview(rid, message, index)`:**
1. Find the medium with the same filter `media()` uses (`:518-523`), so indexes match
   the plugin. If it is missing, reply `not_found`. If it isn't `image/*`, or has
   neither a `thumb_id` nor `inline`, reply `not_found` too.
2. Cache hit: the `previews` entry exists **and** the file exists. Reply with its path.
   If the entry's file is gone, drop the entry and go on.
3. Inline only (`thumb_id` empty): `accept_preview(inline)`, then reply with the path, or
   `not_found`.
4. Not ready: reply `phone_offline`.
5. **Join first:** if a job for the key is queued or running, push `rid` to its waiters
   and return.
6. **Capacity:** if `preview_jobs.len() == 32`, reply `busy` without storing anything.
7. Otherwise store the waiter, queue the job and call `pump_previews()`.

**`pump_previews()`:**
- Drop queued jobs that have no waiters left. Drop those queued for more than 45 s,
  replying `busy` to their waiters, so the 60 s pending deadline is never reached while
  a job is queued.
- Then, while fewer than 2 are running, pop a job and send
  `Command::Preview { id: token, thumb_id, thumb_key }`. If `Commands::send` returns
  false (`src/phone.rs:420`), the job doesn't go into `preview_running`; reply
  `phone_offline` to its waiters, so a slot can't be held for ever.

**`accept_preview(key, bytes) -> Option<PathBuf>`:**
- `image_kind(&bytes)` is the magic-byte check split out of `save_avatar` (`:1324-1332`),
  plus `GIF87a` / `GIF89a`. It gives `jpg`, `png`, `webp`, `gif` or `None`.
- The length must be ≤ 2 MiB.
- `imagesize::blob_size` must succeed with `0 < w, h ≤ 16384` and `w * h ≤ 40_000_000`
  (checked in `u64`).
- What this proves: the container's **header** is one of four formats and declares a
  bounded canvas. It does not prove the body is well formed. Truncated or crafted
  frames still reach Qt's decoder, which is the residual risk the intent accepted.
  Tests use real GIF and WebP fixtures to confirm `imagesize` reads the logical screen
  or canvas size.
- On success, `write_media` writes the file to
  `media_dir/previews/<sha256(message ‖ 0 ‖ index ‖ "preview")>.<ext>`, creating the
  directory with `paths::create_private_dir`. The name never collides with
  `media.open`'s files. The function records the entry and runs `evict_previews`.
- `save_avatar` keeps its own caps and calls `image_kind`.

**`evict_previews(keep, budget)`:**
- List the directory and sort by `(mtime, name)` for a stable order.
- While the total is over the budget (64 MiB in production; tests pass a smaller one),
  remove the oldest file other than `keep`, and its `previews` entry.
- A failed removal is logged at `debug`, its size is still counted, and the loop moves
  to the next file. If the budget can't be reached, that's logged once at `warn` with
  no paths.

**`Event::Previewed { id, result }`:** take the job from `preview_running`; an unknown
token is ignored, which makes late results after a reset harmless.
- `Ok(bytes)` and `accept_preview` succeeds: reply the path to every waiter of the key.
- Otherwise, if `job.inline` isn't empty and `accept_preview(inline)` succeeds: reply
  that path. This is the fallback from the uploaded preview to the inline one.
- Otherwise reply `phone_offline` (a download error) or `not_found` (bad bytes).
- Then `pump_previews()`.

**`cancel_preview(key, rids)`:** remove those rids from the key's waiters and reply
`busy` "cancelled" to each. A queued job left with no waiters is dropped. A running one
finishes, and its file is cached for next time.

**`forget(rid)` (`:686`):** also removes the rid from every waiter list. A queued job
with no waiters left is then dropped at the next `pump_previews()`.

**`reset_media()` (`:803`), the unpair and store-reset path:** it also clears
`previews`, `preview_jobs` and `preview_waiters`, and replies `busy` to those waiters,
as it already does for the media waiters. It also clears `preview_running`, so results
arriving later find no token and are dropped. `clear_media` already removes the files.

### 4. Phone (`src/phone.rs`)

- New `Command::Preview { id, thumb_id, thumb_key }`, dispatched next to
  `Command::Download` (`:1118`). It runs under `tokio::time::timeout(30 s)`:
  - `client.download_media_stream(..)` (`gmessages-proto/src/client.rs:630`);
  - for each `next_chunk()`, it checks `total + chunk.len() > 2 MiB` **before**
    extending the buffer, and fails without extending if so;
  - only `Ok(None)`, the end of stream after the decryptor's `finish`, counts as
    success.
- It emits `Event::Previewed { id, result: Result<Vec<u8>, String> }`. The error text is
  generic, with the detail at `debug`.
- It never calls `full()` and never writes to the store.
- Memory bound: the accepted buffer is ≤ 2 MiB. One transport chunk and the decryptor
  state are extra and bounded by reqwest's chunking, not by this code. That is noted,
  not engineered around.

### 5. Plugin (`plugin/Thread.qml`; `Client.qml` unchanged)

**State**, owned by `Thread` and never by delegates:
- `root.ui["p:<message>:<index>"] = { path, busy, failed, tries, gen }`;
- `property int previewGen`: incremented by `openThread`, by closing (the
  `conversationId === ""` branch, `:57`), when `client.connected` turns false, and when
  `client.state` leaves `"ready"` (unpair goes through `login`). Each increment also
  removes every `"p:"` entry. Clearing a path unloads its `Image`.
- `property var inView: ({})`: the keys currently in view.
- `readonly property bool canPreview: client.features.indexOf("previews") >= 0`.

**Geometry**, which doesn't depend on the bubble or the window, so `sourceSize` is fixed
per box:
- `previewMaxW = Style.space(320)`, `previewMaxH = Style.space(240)`;
- with valid `width` and `height` (both > 0): `scale = min(maxW/w, maxH/h, 1)` and
  `box = (round(w*scale), round(h*scale))`; otherwise the box is `maxW × Style.space(180)`;
- the box also gets `Layout.maximumWidth: msgList.width * 0.78 - 2 * padX`. In a narrow
  window it shrinks, and the `Image` inside fits with `PreserveAspectFit`;
- the box sets its own `implicitWidth` and `implicitHeight`, which feed
  `bubbleContent.implicitWidth` (`:449`), not the other way round.

**The box**, inside each attachment's `ColumnLayout` (`:511`), above the name row:
- shown when `canPreview && att.preview === true && !deleted`;
- for `pending` or `failed` messages it shows only the muted placeholder and never
  requests. A pending message's later confirmed copy has a new id and requests then;
- it holds an `Image` with `asynchronous: true`, `cache: false`, `autoTransform: true`,
  `fillMode: PreserveAspectFit`, and `sourceSize: Qt.size(2 * box.w, 2 * box.h)` (fixed
  per box);
- its `source` is `"file://" + path` once the key has a path, else `""`. On
  `Image.Error` with a path, the plugin clears the path and asks again once
  (`tries < 2`). That covers a file evicted under a rebuilt delegate. After that, the
  state is `failed`;
- while loading, the box shows a muted fill; when `failed`, the same box shows a muted
  glyph. Its size never changes;
- clicking it runs `root.mediaRequest("media.open", …)`, as Open does;
- an `Image`, not an `AnimatedImage`, shows the first frame of an animated image.

**When to request.** Each delegate keeps
`inViewNow = root.visible && bubbleRow.y + bubbleRow.height > msgList.contentY && bubbleRow.y < msgList.contentY + msgList.height`
(`bubbleRow.y` is in ListView content coordinates, `:410`). On a change, it writes or
removes its preview keys in `root.inView`, through a helper that assigns a new object.
All attachments of a message in view count as in view. A message with several pictures
is one bubble, and requesting them all is fine at 2 at a time.

A `Timer` in `Thread` (200 ms, restarted on every `inView` change):
- for each key in view with no state: send `media.preview` with the captured
  `{ conv, gen, key }`, and set `busy`, `tries + 1` and `gen`;
- for each `busy` key not in view: send `media.preview.cancel`, then remove the entry.

Delegate rebuilds within 200 ms don't cancel anything.

**Replies:** apply only when `gen === root.previewGen`, the entry still exists and its
`gen` matches. That handles A → B → A.
- `path`: stored;
- `not_found`: `failed`;
- `busy`, `phone_offline` or `timeout`: if `tries < 2` and the key is still in view,
  clear `busy` so the next timer tick asks again; otherwise `failed`;
- a reply to a cancel is ignored.

All sends check `client.connected && canPreview` first.

**Scroll position.** `mergeMessages` (`:86-118`) anchors on the first visible message,
not on the end. Before replacing the array it computes:
- `follow = msgList.atYEnd || root.messages.length === 0`;
- `anchor`, the id of the delegate at `msgList.indexAt(0, msgList.contentY)`;
- `offset = contentY - item.y`.

After replacing the array:
- if `follow`, `positionViewAtEnd()`;
- otherwise, find the anchor's new index, `positionViewAtIndex(i, ListView.Beginning)`,
  then `contentY += offset`.

The current unconditional `positionViewAtEnd` goes. This covers history updates
(`historyArrived` also passes `dropPending`, `:249`), live messages and older pages. Box
heights are stable, so loading pictures needs no anchoring.

## Alternatives rejected

- **Reusing `media.open`:** it calls `full()` first (`src/phone.rs:1653`) and caches full
  files in the store (`src/sync.rs:633`).
- **Re-encoding in the daemon:** it needs a new crate, and the risk only moves to the
  daemon (decided in the intent).
- **Base64 `data:` URLs over the socket:** that puts image bytes through NDJSON and the
  8 MiB queue cap, to every client.
- **Collapsing a failed box back to the name row only:** that changes heights after
  the fact, which forces anchor corrections for every failure. A fixed box with a glyph
  is simpler and calmer. The name row is there in both cases.
- **Per-attachment visibility:** mapping nested layout items into ListView coordinates
  isn't reactive in QML. Per-message visibility costs at most a few extra small
  downloads.
- **Cancelling running downloads:** at most 2 run, capped at 2 MiB and 30 s, and the
  result is cached anyway.
- **`AnimatedImage`:** constant CPU, and the intent chose the first frame.
- **Cancelling from `Component.onDestruction`:** delegates are rebuilt on every merge
  (`:120`), so it would cancel work the replacement still wants.

## Risks

- **Decoder flaws in the shell.** The header checks narrow what reaches Qt, but don't
  validate frame data. A crash takes the shell down until it restarts. This is the
  accepted residual risk.
- **Anchoring.** It relies on `indexAt` and `positionViewAtIndex`. If a merge removes
  the anchor message, the list falls back to the nearest following message, or to the
  end.
- **Inline-only previews** are small and look blurry when scaled into the box. Open
  shows the real picture.
- **Transient memory** per download is one chunk plus the decryptor state above the
  2 MiB buffer.
- **Mixed versions:** a new plugin with an old daemon gets no `"previews"` and no
  `preview` flags, so no boxes. An old plugin with a new daemon ignores the new fields.

## Verification

- `cargo test --workspace`, `cargo clippy --all-targets -- -D warnings` and
  `nix build .#plugin` pass.
- Unit tests in `src/sync.rs`:
  - **`image_kind` and `accept_preview`, with tiny real fixtures** for PNG, JPEG, GIF
    and WebP (VP8L) as byte literals:
    - a PNG named `.jpg` is accepted as `png`;
    - text is refused;
    - over 2 MiB is refused;
    - a PNG header claiming 10000 × 5000 is refused (over 40 MP);
    - 20000 × 1 is refused (over 16384);
    - 0 × 10 is refused;
    - a truncated header is refused;
    - a GIF's logical screen size is read correctly.
  - **`preview()`:**
    - an unknown message or index, or a non-image, gives `not_found`;
    - inline-only gives a path under `previews/` straight away;
    - a cache hit whose file was deleted downloads again;
    - an uploaded preview sends one `Command::Preview`, and a second request for the
      same key joins it with no second command;
    - a third key while 2 run is queued;
    - a full queue gives `busy`, with no waiter stored;
    - `Commands::send` returning false frees the slot and replies `phone_offline`;
    - `Previewed` with bad bytes and good inline bytes gives the inline path;
    - with bad bytes and no inline bytes, `not_found`;
    - a token left from before a reset is ignored.
  - **The queue:**
    - a job queued for more than 45 s is dropped with `busy`;
    - a job left with no waiters after `forget` is dropped and never sent.
  - **`cancel_preview`** removes only the given rids. Another connection's waiter on the
    same key still gets the path.
  - **`reset_media`** clears all preview state and replies to its waiters.
  - **`evict_previews`:**
    - with a small budget, it deletes the oldest first, by mtime then name, and never
      `keep`;
    - when removing a file fails (a read-only directory), it skips that file and
      reaches the budget with the next one.
  - **`Message::from_phone`** sets `preview`, `width` and `height`, and drops dimensions
    outside `1..=16384`.
- `src/phone.rs`: the streaming cap stops at the first chunk that would go over the
  limit, tested with a fake chunk iterator through a small helper that takes chunks.
- `tests/serve.rs`:
  - `hello.features` contains `"previews"`;
  - `media.preview` and `media.preview.cancel` with an empty message are `bad_request`;
  - before pairing, `media.preview` gets the same code as `media.open`.
- By hand on p620, after the deploy and a shell restart:
  - received and sent photos show inline;
  - a picture still sending shows a placeholder, then the photo;
  - clicking opens the viewer;
  - a PDF has no box;
  - scrolling back doesn't jump when new messages arrive;
  - with the phone offline, boxes show the "no preview" glyph without changing size;
  - switching A → B → A shows the right pictures;
  - unpairing clears them;
  - an old daemon (a revert) shows no boxes.
