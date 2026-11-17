---
status: approved
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui)
spec: spec/2026-11-17-inline-image-previews.md
coders:
  1: codex   # all Rust: preview queue, waiters, tokens, image checks, eviction, streaming cap, serve wiring: tricky stateful logic
  2: coder   # QML: preview box, visible-range scan, debounce timer, request ids, scroll anchoring: broad UI work
reviewers:
  - codex   # independent read of the whole diff against this plan
---

# Plan: Show pictures inline in the conversation

Self-contained: an implementer needs only this file and the code. The design reasons are
in the approved spec, including its "Amendments" section, which is approved together
with this plan.

## Approved decisions

- **A separate preview path.**
  - Methods `media.preview` and `media.preview.cancel`, both taking `{message, index}`.
    `media.preview` replies `{path}`.
  - It never calls `full()` and never writes to the store.
  - Order: the uploaded preview (`thumb_id`), then the `inline` preview if that fails or
    is bad. Never the full file.
- **Limits:**
  - preview bytes ≤ 2 MiB, checked while streaming **before** each chunk is appended;
  - 2 jobs run at a time, and the queue holds ≤ 32 jobs;
  - a job queued for more than **25 s** is dropped and its waiters get `busy`;
  - a job with no waiters is dropped while queued;
  - a download times out after 30 s;
  - the cache is 64 MiB in `media_dir/previews/`, evicted oldest first by
    `(mtime, name)`.
- **Checks:**
  - magic bytes JPEG, PNG, WebP or GIF;
  - `imagesize` dimensions with `0 < w, h ≤ 16384` and `w*h ≤ 40_000_000` (in `u64`).
- **Protocol:**
  - `FEATURES = ["attachments", "previews"]`;
  - `proto::Attachment` gains `width` and `height` (sent only when both are in
    `1..=16384`) and `preview: bool` (image mime and a `thumb_id` or `inline`);
  - all three are skipped when zero or false.
- **Errors:**
  - `not_found`: no attachment, or no usable preview;
  - `phone_offline`: not ready, the download failed, or the command couldn't be sent;
  - `busy`: queue full, cancelled, expired, or reset.
- **Cancel** removes only the calling connection's waiters.
- **Panel-sent pictures** have no preview data and show the name row (amendment 2).
- **QML:**
  - a fixed-size box;
  - the in-view set is computed by Thread from the visible index range;
  - each request has its own id and is stamped with the current `previewGen`;
  - `tries` counts failures only, at most 2;
  - the list follows the end only when already at the end, otherwise it keeps an
    anchor message and offset.

## Rules for every step

- Work on branch `feat/inline-image-previews`. Never on `main`, never in `~/.config/nixos`.
- Implementers never stage, commit, push, deploy, switch, restart, garbage-collect or
  reboot. Claude commits after each step.
- Rust checks:
  `nix develop -c sh -c 'cargo test --workspace && cargo clippy --all-targets -- -D warnings'`.
- Leave the `allow(dead_code)` attributes in `src/main.rs` alone.
- No message text, numbers, names, conversation ids, keys, file names or paths in logs
  above `debug`.
- **Trap:** no `tempfile` crate.
  - `src/sync.rs` tests make a unique dir with `std::env::temp_dir()` and a random
    suffix; see the helper around `src/sync.rs:1572`.
  - `src/phone.rs` tests already have `test_dir()` (`:2060`).
- **Trap:** `src/phone.rs` has its own `log` module with `warn` only. Use
  `tracing::debug!` for detail.
- **Trap:** QML `Color` has `foreground`, `background`, `accent`, `urgent`, `muted`, and
  no `surface`. `pragma ComponentBehavior: Bound` needs `required property` for `index`
  and `modelData` in delegates.
- **Trap:** QML maps (`root.ui`) are replaced with a new object on every change;
  `setUi` already merges patches (`plugin/Thread.qml:124`).

## Steps

1. **All Rust: `src/proto.rs`, `src/phone.rs`, `src/sync.rs`, `src/serve.rs`,
   `tests/serve.rs`.**

   **`proto.rs`:**
   - `FEATURES` (`:12`) becomes `&["attachments", "previews"]`.
   - `Attachment` (`:133`) gains:
     - `#[serde(default, skip_serializing_if = "is_zero")] pub width: i64` and the same
       for `height`;
     - `#[serde(default, skip_serializing_if = "std::ops::Not::not")] pub preview: bool`.

     Add a private `fn is_zero(n: &i64) -> bool` if none exists.
   - In `Message::from_phone` (`:228-240`), from the parsed `phone::Medium`:
     - `width`/`height`: set both only when both are in `1..=16384`, else 0;
     - `preview = mime.starts_with("image/") && (!thumb_id.is_empty() || !inline.is_empty())`.
   - Fix every other `proto::Attachment { .. }` literal; grep for them, tests included.

   **`phone.rs`:**
   - `pub const PREVIEW_MAX: usize = 2 * 1024 * 1024;`.
   - `Command::Preview { id: u64, thumb_id: String, thumb_key: Vec<u8> }`.
   - `Event::Previewed { id: u64, result: Result<Vec<u8>, String> }`.
   - Dispatch in `run_commands` next to `Command::Download` (`:1118`): `spawn_local` →
     `tokio::time::timeout(Duration::from_secs(30), preview(&state, thumb_id, thumb_key))`
     → `state.emit(Event::Previewed { id, result })`. A timeout gives
     `Err("preview download failed")`.
   - `async fn preview(state: &State, thumb_id: String, thumb_key: Vec<u8>) -> Result<Vec<u8>, String>`:
     - `state.client()?`;
     - `client.download_media_stream(&thumb_id, &thumb_key)` (`gmessages-proto/src/client.rs:630`);
     - loop on `next_chunk()`: on `Some(c)`, call
       `push_capped(&mut buf, &c, PREVIEW_MAX)?`; on `None`, break (success);
     - errors are generic (`"preview download failed"` / `"preview too large"`), with
       detail at `tracing::debug!`.
   - `fn push_capped(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> Result<(), String>`:
     if `buf.len() + chunk.len() > cap`, return `Err` **without** extending; else
     extend.
   - Test: `push_capped` with cap 5 and chunks `[1,2,3]`, then `[4,5,6]`. The second
     call fails and `buf.len() == 3`.

   **`sync.rs`:**
   - **`image_kind`.** Split `fn image_kind(b: &[u8]) -> Option<&'static str>` out of
     `save_avatar` (`:1324-1332`), adding `GIF87a`/`GIF89a` → `"gif"`. `save_avatar`
     calls it, refuses `"gif"` explicitly (avatars stay jpg/png/webp, as now) and keeps
     its own caps.
   - **State** on `Sync`, initialised in `new`:
     - `previews: HashMap<(String, u32), PathBuf>`;
     - `preview_jobs: VecDeque<PreviewJob>`;
     - `preview_running: HashMap<u64, PreviewJob>`;
     - `preview_waiters: HashMap<(String, u32), Vec<u64>>`.

     `struct PreviewJob { key: (String, u32), token: u64, thumb_id: String, thumb_key: Vec<u8>, inline: Vec<u8>, queued_at: Instant }`
     uses `std::time::Instant`. Tokens come from `self.next_token += 1` (`:32`, `:174`).
     A key with an **empty** waiter vec counts as having no waiters.
   - **`pub fn preview(&mut self, rid: u64, message: &str, index: u32) -> Vec<proto::Out>`**
     runs, in order:
     1. Find the medium with the same filter as `media()` (`:518-523`, valid `Medium`
        JSON, then `.nth(index)`). If it is missing, not `image/*`, or has an empty
        `thumb_id` and empty `inline`, reply `not_found` "no preview".
     2. If `previews[key]` exists and `path.exists()`, reply the path; if the file is
        gone, remove the entry.
     3. If `thumb_id` is empty: `accept_preview(&key, &inline, PREVIEW_BUDGET)`, then
        reply the path or `not_found`.
     4. If `!self.ready`, reply `phone_offline`.
     5. If a job for the key is in `preview_jobs` or `preview_running`, push `rid` to
        `preview_waiters[key]` and return `vec![]`.
     6. If `preview_jobs.len() >= 32`, reply `busy` "queue full" and store nothing.
     7. Insert the waiter, push the job (`queued_at: Instant::now()`) and **return**
        `self.pump_previews(Instant::now())`.

     Replies use `Self::reply(rid, code, msg)` for errors and, for success,
     `proto::Out::Reply { rid, result: Ok(to_value(proto::MediaData { path }).expect(..)) }`,
     as `finish` does (`:589`).
   - **`pub fn pump_previews(&mut self, now: Instant) -> Vec<proto::Out>`:**
     1. Retain only queued jobs whose key has non-empty waiters.
     2. Drop jobs with `now.duration_since(queued_at) > 25 s`. **Remove** their key from
        `preview_waiters`, and reply `busy` "expired" to each of those rids.
     3. While `preview_running.len() < 2`, pop the front job and call
        `self.commands.send(Command::Preview { id: job.token, .. })`. If that returns
        `false`, **remove** the key's waiters and reply `phone_offline` to them, and
        don't insert the job. Otherwise insert it into `preview_running`.
     4. Return all replies.
   - **`fn accept_preview(&mut self, key: &(String, u32), bytes: &[u8], budget: u64) -> Option<PathBuf>`**
     runs, in order:
     - `image_kind(bytes)?`;
     - `bytes.len() <= phone::PREVIEW_MAX`;
     - `imagesize::blob_size(bytes).ok()?` with the dimension bounds in `u64`;
     - `paths::create_private_dir(&self.media_dir.join("previews"))`;
     - `write_media` (`:1384`) to
       `previews/<hex sha256(message bytes ‖ [0] ‖ index.to_be_bytes() ‖ b"preview")>.<ext>`;
     - insert the entry into `previews`;
     - `self.evict_previews(&path, budget)`.

     Production uses `const PREVIEW_BUDGET: u64 = 64 * 1024 * 1024`.
   - **`fn evict_previews(&mut self, keep: &Path, budget: u64)`:**
     1. `read_dir(previews)`, skipping names that end in `.tmp`, and collect
        `(mtime, name, path, len)`; sort ascending.
     2. Sum the lengths. Then, for each entry in order while the total is over
        `budget`: skip `keep`; on a successful `remove_file`, subtract its length and
        drop any `previews` entry with that path; on an error, `tracing::debug!` and
        continue.
     3. If the total is still over the budget, `tracing::warn!("preview cache over budget")`
        once, with no paths.
   - **`phone::Event::Previewed { id, result }`** in `handle()` (next to `Downloaded`,
     `:1064`):
     1. `let Some(job) = self.preview_running.remove(&id) else { return Vec::new() };`.
     2. `path` = on `Ok(bytes)`, `accept_preview(&job.key, &bytes, PREVIEW_BUDGET)`;
        when that gives `None` and `job.inline` isn't empty,
        `accept_preview(&job.key, &job.inline, PREVIEW_BUDGET)`.
     3. The reply for each rid in `self.preview_waiters.remove(&job.key).unwrap_or_default()`:
        the path if there is one; else `phone_offline` "preview download failed" if
        `result` was `Err`; else `not_found` "no preview".
     4. Extend with `self.pump_previews(Instant::now())`, and return.
   - **`pub fn cancel_preview(&mut self, message: &str, index: u32, rids: &[u64]) -> Vec<proto::Out>`:**
     - take the matching rids out of `preview_waiters[key]` and reply `busy` "cancelled"
       to each;
     - if the vec is now empty, remove the key and drop its job from `preview_jobs`,
       but not from `preview_running`.
   - **`forget(rid)`** (`:686`): also remove `rid` from every `preview_waiters` vec. Empty
     vecs are fine, because pump drops their queued jobs.
   - **`reset_media()`** (`:803`):
     - also clear `previews`, `preview_jobs` and `preview_running`;
     - drain `preview_waiters` and append one `Self::reply(rid, proto::ErrorCode::Busy, "reset")`
       per rid. This is a **separate** chain from the existing `NotPaired` replies
       (`:816`).

   **`serve.rs`:**
   - **In `expire()` (`:712`), before computing `expired`:**
     `if let Some(sync) = &mut self.sync { let out = sync.pump_previews(Instant::now()); self.outputs(out); }`.
     It runs every 100 ms (`:300`). Check that `self.outputs` can be called there
     (`&mut self`, no conflicting borrow); if not, collect the outputs first.
   - **Next to the `"media.open" | "media.save"` arm (`:1127-1150`):**
     - **`"media.preview"`:**
       - `params::<proto::MediaParams>`;
       - an empty message gives `reply = Err("message required".into())`;
       - otherwise allocate a rid, insert `Pending { conn, id, until: Instant::now() + Duration::from_secs(60) }`,
         `let out = self.sync.as_mut().expect("store checked").preview(rid, &p.message, p.index); self.outputs(out); return;`.
     - **`"media.preview.cancel"`:**
       - the same params check;
       - `let rids: Vec<u64> = self.pending.iter().filter(|(_, p)| p.conn == conn).map(|(r, _)| *r).collect();`;
       - `let out = self.sync.as_mut().expect("store checked").cancel_preview(&p.message, p.index, &rids); self.outputs(out);`;
       - `reply = Ok(serde_json::json!({}))`, then fall through to the normal reply.

     Both pass the same store gate as `media.open` (`:883`), which checks that `sync`
     exists, not pairing. Don't add a ready gate.

   **Tests** (all in this step):
   - **`src/sync.rs`.** Fixtures:
     - a tiny **valid** 1×1 PNG (the 67-byte literal from the `src/phone.rs` tests);
     - a minimal JPEG (`FF D8 FF E0 … SOF0` with 8×6);
     - `GIF89a` with logical screen 3×2;
     - WebP `RIFF….WEBPVP8L` with 2×2.

     Keep `png(w, h)` (`:1562`) for header-only dimension cases. Messages with media
     are stored as the download tests do (`:1687` area), with a `Medium` JSON that sets
     `thumb_id` + `thumb_key` or `inline`.
     - `image_kind` and `accept_preview`:
       - each real fixture is accepted with the right extension;
       - a PNG under a `.jpg` mime is still `png`;
       - `b"hello"` is refused;
       - 2 MiB + 1 bytes are refused;
       - `png(10000, 5000)` is refused;
       - `png(20000, 1)` is refused;
       - `png(0, 10)` is refused;
       - a truncated PNG header is refused;
       - the GIF reads as 3×2.
     - `preview()`:
       - an unknown message, a bad index or a non-image gives `not_found`;
       - inline-only gives a path under `media_dir/previews/` straight away;
       - a cache hit whose file was deleted queues a download again;
       - a thumb gives exactly one `Command::Preview`, and a second rid for the same
         key adds a waiter with no new command;
       - a third key while 2 run is queued with no command;
       - with 32 queued, the next distinct key gets `busy` and `preview_waiters`
         doesn't contain it;
       - with a closed command channel (drop the receiver from
         `phone::Commands::detached()`), pump replies `phone_offline` and
         `preview_running` stays empty.
     - `Previewed`:
       - `Ok(valid)` replies the path to all waiters and starts the next queued job;
       - `Ok(b"junk")` with valid inline bytes replies the inline path;
       - `Ok(b"junk")` without inline bytes gives `not_found`;
       - `Err(..)` without inline bytes gives `phone_offline`;
       - an unknown token gives no output.
     - Queue:
       - `pump_previews(now + 26 s)` drops a queued job and replies `busy` to its
         waiters;
       - after `forget` empties a queued job's waiters, the next pump drops it and sends
         nothing.
     - `cancel_preview` with only connection A's rid leaves B's waiter, which later gets
       the path.
     - `reset_media` clears all four preview structures and replies `busy` to the
       preview waiters.
     - `evict_previews` with budget 10 and three 6-byte files whose mtimes are set with
       `File::set_modified`: it removes the oldest first and never `keep`. With an
       undeletable file (the directory chmod 0500 for the test), it skips that file,
       continues, and the test restores the permissions.
     - `Message::from_phone`: `preview` is true for an image with a thumb, false for a
       PDF and false for an image without thumb or inline; `width`/`height` are kept for
       640×480 and dropped for 0×480 and 20000×10.
   - **`src/phone.rs`:** the `push_capped` test above.
   - **`tests/serve.rs`:**
     - update the `hello` assert to `["attachments", "previews"]`;
     - `media.preview` and `media.preview.cancel` with `"message": ""` give
       `bad_request`;
     - after the daemon reaches `login` (as the existing test does), `media.open` and
       `media.preview` for `{"message":"nope","index":0}` **both** give `not_found`.

   → verify with the Rust check command.

   Traps:
   - this step is one unit, so every new item has a production caller (serve) and
     clippy sees no dead code;
   - every `pump_previews` result must reach `Daemon::outputs`, through the returned
     `Vec` or `expire()`;
   - `handle()`'s `phone::Event` match is exhaustive.

2. **`plugin/Thread.qml`: the picture box, requests and scroll anchoring.** `Client.qml`
   is unchanged (`features` and `connected` exist). Read `Thread.qml` whole first.

   **State** on `root`:
   - `property int previewGen: 0`;
   - `property int previewReq: 0`;
   - `readonly property bool canPreview: root.client.features.indexOf("previews") >= 0`;
   - `readonly property real previewMaxW: Style.space(320)`;
   - `readonly property real previewMaxH: Style.space(240)`.

   A preview entry is `root.ui["p:" + msgId + ":" + index] = { path, busy, failed, tries, req, gen }`.

   **`resetPreviews()`:** `previewGen++`, then assign `root.ui` a copy without its `p:`
   keys. Call it:
   - at the start of `openThread` (`:68`);
   - in the `conversationId === ""` branch (`:57`);
   - in a `Connections` on `root.client`, from `onConnectedChanged` when not connected
     and from the existing `onStateChanged` (`:276`) when `state !== "ready"`.

   **`firstVisible(y)`:** returns the index at content `y`, trying
   `msgList.indexAt(0, y)`, then `y + msgList.spacing + 1`, then `y - msgList.spacing - 1`.
   It may return -1.

   **`visibleKeys()`:**
   - `first = firstVisible(contentY + 1)` and `last = firstVisible(contentY + height - 1)`;
   - if `first < 0`, use 0; if `last < 0`, use `messages.length - 1`;
   - for each `i` in `[first, last]` whose message is not `pending`, `failed` or
     `deleted`, and each attachment index `j` with `preview === true`, push
     `"p:" + id + ":" + j`.

   It returns `[]` unless `root.visible && root.canPreview && client.connected`.

   **`Timer { id: previewTimer; interval: 200; onTriggered: root.pumpPreviews() }`**,
   restarted from `msgList.onContentYChanged`, `onHeightChanged`, `onMessagesChanged`,
   `onVisibleChanged` and `onCanPreviewChanged`.

   **`pumpPreviews()`:**
   - Compute `want = visibleKeys()` as a set.
   - **For each key in `want`** whose entry is missing, or has `!busy && !failed && !path`:
     1. `req = ++previewReq`; `gen = previewGen`.
     2. `setUi(key, { busy: true, req, gen, tries: old.tries || 0 })`.
     3. `client.send("media.preview", { message, index }, cb)`. Parse `message` and
        `index` from the key; the index is after the last `:`.
     4. The callback first checks `e = root.ui[key]` and
        `e && e.req === req && e.gen === gen && gen === previewGen`, else returns. Then:
        - `ok`: `setUi(key, { path: r.data.path, busy: false })`;
        - `not_found`: `setUi(key, { failed: true, busy: false })`;
        - `busy`, `phone_offline`, `timeout` or `disconnected`: `t = e.tries + 1`, then
          `setUi(key, { busy: false, tries: t, failed: t >= 2 })` and
          `previewTimer.restart()`.
   - **For each `p:` key** that is `busy` and not in `want`:
     - `client.send("media.preview.cancel", { message, index }, null)`, if connected
       and `canPreview`;
     - `setUi(key, { busy: false, req: 0 })`. `tries` is unchanged, and `req: 0` makes
       the late reply be ignored.

   **The box**, in the attachment `ColumnLayout` (`:511`) before the name `RowLayout`:
   `Item { id: box; visible: root.canPreview && att.modelData.preview === true && !bubbleRow.isDeleted }`,
   with:
   - `readonly property string key: "p:" + bubbleRow.modelData.id + ":" + att.index`;
   - `readonly property var st: root.ui[box.key] || ({})`;
   - `readonly property real bw` and `bh`: with `w = Number(att.modelData.width) || 0`
     and `h = …height`, if both are > 0, `s = Math.min(root.previewMaxW / w, root.previewMaxH / h, 1)`
     and the box is `Math.round(w * s)` × `Math.round(h * s)`; else `root.previewMaxW`
     × `Style.space(180)`;
   - `implicitWidth: box.bw; implicitHeight: box.bh; Layout.maximumWidth: msgList.width * 0.78 - 2 * root.theme.padX`.

   Inside it:
   - `Rectangle { anchors.fill: parent; radius: Style.cornerRadius; color: Commons.Color.muted; opacity: 0.15 }`;
   - `Text { anchors.centerIn: parent; visible: box.st.failed === true; text: "\u{F02E9}"; color: Commons.Color.muted; textFormat: Text.PlainText }`;
   - an `Image`:
     - `anchors.fill: parent`, `asynchronous: true`, `cache: false`,
       `autoTransform: true`, `fillMode: Image.PreserveAspectFit`;
     - `sourceSize: Qt.size(2 * box.bw, 2 * box.bh)`;
     - `source: box.st.path ? "file://" + box.st.path : ""`;
     - `onStatusChanged`: if `status === Image.Error && box.st.path`, then
       `t = (box.st.tries || 0) + 1`, `setUi(box.key, { path: "", busy: false, tries: t, failed: t >= 2 })`
       and `previewTimer.restart()`;
   - `MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: root.mediaRequest("media.open", String(bubbleRow.modelData.id), att.index) }`.

   Pending or failed messages show the box (when `preview` is true) with no request,
   because `visibleKeys` skips them.

   **`mergeMessages`** (`:86-118`): replace the trailing `Qt.callLater(positionViewAtEnd)`.
   - **Before** assigning `root.messages`:
     - `follow = msgList.atYEnd || root.messages.length === 0`;
     - `ai = firstVisible(msgList.contentY + 1)`;
     - `anchor = ai >= 0 ? root.messages[ai] : null` (keep `id` and `timestamp_ms`);
     - `item = ai >= 0 ? msgList.itemAtIndex(ai) : null`;
     - `offset = item ? msgList.contentY - item.y : 0`.
   - **After** assigning, in `Qt.callLater`:
     - if `follow` or `!anchor`, `positionViewAtEnd()` and return;
     - `ni` = the index of `anchor.id` in the new list; if that is -1, the first index
       whose `timestamp_ms >= anchor.timestamp_ms`; if that is also -1,
       `positionViewAtEnd()` and return;
     - `msgList.positionViewAtIndex(ni, ListView.Beginning); msgList.contentY += offset`.
   - Then `previewTimer.restart()`.

   → verify with `nix build .#plugin` (its install check runs `qmllint`), then
   `nix develop -c sh -c 'cargo test --workspace'`.

   Deviations made while implementing (same behaviour):
   - `onVisibleChanged`, `onCanPreviewChanged` and `onMessagesChanged` sit on `root`, where
     those properties live; `onContentYChanged` and `onHeightChanged` are on `msgList`;
   - a helper `previewParams(key)` parses `{message, index}` for both request and cancel;
   - every error code except `not_found` counts as a retryable failure (cancelled replies
     are still ignored through `req: 0`);
   - the anchor is looked up only when not following the end;
   - the "no preview" glyph gets the theme's font family and body size.
   Traps:
   - `indexAt` takes content coordinates;
   - `itemAtIndex` can be `null`;
   - attachment indexes match the daemon's (both filter valid `Medium` the same way,
     `src/proto.rs:228-240`, `src/sync.rs:518`);
   - don't set `cacheBuffer`;
   - don't send anything from `Component.onDestruction`.

## Tests

```sh
nix develop -c sh -c 'cargo test --workspace && cargo clippy --all-targets -- -D warnings'
nix build .#plugin
```

Expected: everything passes, including all the new tests listed in step 1.

By hand on p620 after the deploy and a shell restart:
- received photos and photos sent from the phone show inline;
- a picture sent from the panel shows its name row until preview data arrives;
- clicking a picture opens the viewer;
- a PDF has no box;
- scrolling back doesn't jump when a message arrives;
- with the phone offline, boxes show the "no preview" glyph without changing size;
- switching A → B → A shows the right pictures;
- unpairing clears them;
- an old daemon shows no boxes.

## Rollback

Revert the step commits, or the merge commit, and bump the `gmessages` input in
nixos_config back. The protocol changes are additive, so mixed versions keep working,
with pictures hidden.
