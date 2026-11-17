---
status: draft
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui)
spec: spec/2026-11-17-inline-image-previews.md
coders:
  1: codex   # sync/phone/proto: preview queue, waiters, tokens, image checks, eviction, streaming cap: tricky stateful logic
  2: codex   # serve: two methods, per-connection cancel, features: small but must match step 1 exactly
  3: coder   # QML: preview box, in-view set, debounce timer, generation guard, scroll anchoring: broad UI work
reviewers:
  - codex   # independent read of the whole diff against this plan
---

# Plan: Show pictures inline in the conversation

Self-contained: an implementer needs only this file and the code. The design reasons are
in the approved spec; this file says what to change and where.

## Approved decisions (from the spec)

- **A separate preview path.**
  - New methods `media.preview` and `media.preview.cancel`, both taking
    `{message, index}`. `media.preview` replies `{path}`.
  - It never calls `full()` and never writes to the store.
  - Order: the uploaded preview (`thumb_id`), then the `inline` preview if that fails or
    is bad. Never the full file.
- **Limits:**
  - preview bytes ≤ 2 MiB, checked while streaming, **before** each chunk is appended;
  - 2 jobs run at a time, and the queue holds ≤ 32 jobs;
  - a job queued for more than 45 s is dropped and its waiters get `busy`;
  - a job with no waiters is dropped while queued;
  - a download times out after 30 s;
  - the cache is 64 MiB in `media_dir/previews/`, oldest first by `(mtime, name)`.
- **Checks:**
  - magic bytes JPEG, PNG, WebP or GIF (`image_kind`, split out of `save_avatar`);
  - `imagesize` dimensions with `0 < w, h ≤ 16384` and `w*h ≤ 40_000_000` (in `u64`).
- **Protocol:**
  - `FEATURES = ["attachments", "previews"]`;
  - `proto::Attachment` gains `width` and `height` (sent only when both are in
    `1..=16384`) and `preview: bool` (image mime and a `thumb_id` or `inline`);
  - all three are skipped when zero or false.
- **Errors** use existing codes: `not_found` (no attachment or no usable preview),
  `phone_offline` (not ready or the download failed), `busy` (queue full, cancelled,
  expired, reset).
- **Cancel** removes only the calling connection's waiters.
- **QML:**
  - a fixed-size box: the box never changes size;
  - a `previewGen` counter guards against stale replies;
  - Thread owns the state, the in-view set and a 200 ms debounce timer;
  - at most 2 tries;
  - the list follows the end only when already at the end, otherwise it keeps an
    anchor message and offset.

## Rules for every step

- Work on branch `feat/inline-image-previews`. Never on `main`, never in `~/.config/nixos`.
- Implementers never stage, commit, push, deploy, switch, restart, garbage-collect or
  reboot. Claude commits after each step.
- `cargo test --workspace` and `cargo clippy --all-targets -- -D warnings` pass after
  every Rust step, run as `nix develop -c sh -c '…'`.
- Leave the `allow(dead_code)` attributes in `src/main.rs` alone.
- No message text, numbers, names, conversation ids, keys, file names or paths in logs
  above `debug`.
- **Trap:** no `tempfile` crate. Tests use a unique dir under `std::env::temp_dir()`, as
  `src/sync.rs:1546` and the step 1 helper `test_dir()` in `src/phone.rs` tests do.
- **Trap:** `src/phone.rs` has its own `log` module with `warn` only. Use
  `tracing::debug!` for detail.
- **Trap:** QML `Color` has `foreground`, `background`, `accent`, `urgent`, `muted` and no
  `surface`. Files with `pragma ComponentBehavior: Bound` need `required property` for
  `index` and `modelData` in delegates.
- **Trap:** QML maps (`root.ui`, `inView`) are replaced with a new object on every
  change; mutating them in place doesn't notify bindings.

## Steps

1. **`src/proto.rs`, `src/phone.rs`, `src/sync.rs`: the preview path in the daemon.**
   - **`proto.rs`:**
     - `Attachment` (`:133`) gains
       `#[serde(default, skip_serializing_if = "is_zero")] width: i64`, the same for
       `height`, and `#[serde(default, skip_serializing_if = "std::ops::Not::not")] preview: bool`.
       Add a private `fn is_zero(n: &i64) -> bool` if `proto.rs` has none.
     - `Message::from_phone` (`:228-240`) fills them from the parsed `phone::Medium`:
       - `width`/`height`: both only when both are in `1..=16384`, otherwise 0;
       - `preview = mime.starts_with("image/") && (!thumb_id.is_empty() || !inline.is_empty())`.
     - Fix every other `proto::Attachment { .. }` constructor; grep for them, tests
       included.
   - **`phone.rs`:**
     - `Command::Preview { id: u64, thumb_id: String, thumb_key: Vec<u8> }` and
       `Event::Previewed { id: u64, result: Result<Vec<u8>, String> }`.
     - Dispatch in `run_commands` next to `Command::Download` (`:1118`), with
       `spawn_local`, `tokio::time::timeout(30 s)` around the body, and
       `state.emit(Event::Previewed { .. })`.
     - The body is
       `async fn preview(state, thumb_id, thumb_key) -> Result<Vec<u8>, String>`:
       - `state.client()?`, then `client.download_media_stream(&thumb_id, &thumb_key)`
         (`gmessages-proto/src/client.rs:630`);
       - then loop `next_chunk()`, pushing each chunk through a pure helper
         `fn push_capped(buf: &mut Vec<u8>, chunk: &[u8], cap: usize) -> Result<(), String>`,
         which checks `buf.len() + chunk.len() > cap` **before** extending;
       - success only on `Ok(None)`.

       Errors: the generic `"preview download failed"` (or `"preview too large"`), with
       detail at `tracing::debug!`.
     - `pub const PREVIEW_MAX: usize = 2 * 1024 * 1024;`.
     - A unit test for `push_capped`: chunks of 3 + 3 under a cap of 5 fail on the
       second chunk, and the buffer stays at 3.
   - **`sync.rs`:**
     - **`image_kind`:** split `fn image_kind(b: &[u8]) -> Option<&'static str>` out of
       `save_avatar` (`:1324-1332`), adding `GIF87a` and `GIF89a` → `"gif"`.
       `save_avatar` calls it, keeps its own caps and still refuses `gif`, as today.
     - **State** on `Sync`, initialised in `new` (`:111` area):
       - `previews: HashMap<(String, u32), PathBuf>`;
       - `preview_jobs: VecDeque<PreviewJob>`;
       - `preview_running: HashMap<u64, PreviewJob>`;
       - `preview_waiters: HashMap<(String, u32), Vec<u64>>`.

       `struct PreviewJob { key: (String, u32), token: u64, thumb_id: String, thumb_key: Vec<u8>, inline: Vec<u8>, queued_at: Instant }`.
       Tokens come from `self.next_token` (`:32`).
     - **`pub fn preview(&mut self, rid: u64, message: &str, index: u32) -> Vec<proto::Out>`**
       does steps 1–7 of spec §3 in order:
       1. Find the medium with `media()`'s filter (`:518-523`). If it is missing, not
          `image/*`, or has neither a thumb nor inline bytes, reply `not_found`.
       2. On a cache hit whose file exists, reply the path. If the file is gone, drop
          the entry.
       3. Inline only: run `accept_preview`, then reply the path or `not_found`.
       4. If `!self.ready`, reply `phone_offline`.
       5. Join a queued or running job for the key.
       6. If the queue is full (32), reply `busy` with nothing stored.
       7. Otherwise store the waiter, queue the job and call `pump_previews()`.

       Replies go through the existing `Self::reply(rid, code, msg)` helper and an `Ok`
       `proto::Out::Reply` with `MediaData { path }`, as `finish` does (`:589`).
     - **`fn pump_previews(&mut self, now: Instant) -> Vec<proto::Out>`:**
       - drop queued jobs with no waiters;
       - drop jobs with `now - queued_at > 45 s`, replying `busy` "expired" to their
         waiters;
       - while `preview_running.len() < 2`, pop a job and send `Command::Preview`.
         If `self.commands.send(..)` returns false, reply `phone_offline` to its waiters
         and don't insert the job.

       Pass `now` in, so tests can move time.
     - **`fn accept_preview(&mut self, key: &(String, u32), bytes: &[u8], budget: u64) -> Option<PathBuf>`**
       runs, in order:
       - `image_kind`;
       - a length check against `PREVIEW_MAX`;
       - `imagesize::blob_size` with the bounds above;
       - `paths::create_private_dir(media_dir/previews)`;
       - `write_media` (`:1384`) to `<sha256(message ‖ 0u8 ‖ index BE ‖ "preview")>.<ext>`;
       - record the entry;
       - `evict_previews(&path, budget)`.

       Production passes `PREVIEW_BUDGET = 64 MiB`.
     - **`fn evict_previews(&mut self, keep: &Path, budget: u64)`:**
       - list `previews/` (skipping `*.tmp`) and sort by `(mtime, file name)`;
       - while the total is over `budget`, `remove_file` the oldest non-`keep` file. On
         an error, log at `debug`, count it as still present and continue;
       - drop the `previews` entries whose path was removed;
       - if the budget wasn't reached, log once at `warn` (no paths).
     - **`phone::Event::Previewed { id, result }`** in `handle` (next to `Downloaded`,
       `:1064`):
       - `preview_running.remove(&id)`; if it's absent, return no output;
       - on `Ok(bytes)` and `accept_preview` success, the path;
       - else, with non-empty `job.inline` and `accept_preview(inline)` success, that
         path;
       - else `phone_offline` (for `Err`) or `not_found` (for bad bytes);
       - reply to every waiter in `preview_waiters.remove(&job.key)`, then
         `pump_previews(Instant::now())`.
     - **`pub fn cancel_preview(&mut self, message: &str, index: u32, rids: &[u64]) -> Vec<proto::Out>`:**
       - remove those rids from the key's waiters and reply `busy` "cancelled" to each;
       - when no waiters are left, remove the key's queued job, but not a running one.
     - **`forget(rid)`** (`:686`) also removes `rid` from every list in
       `preview_waiters`.
     - **`reset_media()`** (`:803`):
       - clears `previews`, `preview_jobs` and `preview_running`;
       - drains `preview_waiters` and adds a `busy` reply for each rid to the output it
         already returns, matching how it handles `waiters` (`:807`).
   - **Tests** in `src/sync.rs`'s test module: every `src/sync.rs` item of the spec's
     Verification list.
     - Fixtures are byte literals: tiny valid PNG (reuse `png(w, h)`, `:1562`), JPEG
       SOI+SOF0, `GIF89a` with a logical screen size, and WebP `RIFF…WEBPVP8L` with
       its size.
     - Make a `Medium` JSON with `thumb_id`/`thumb_key`, or `inline` set, and store a
       message with it, as the download tests do (`:1687` area).
     - The test for "bad bytes then inline fallback" sends
       `Event::Previewed { Ok(b"junk") }` for a job whose inline bytes are a valid PNG.
     - For eviction, call `evict_previews` with a small budget and set mtimes with
       `File::set_modified`.
     - For the queue expiry, call `pump_previews` with `now + 46 s`.

   → verify with
   `nix develop -c sh -c 'cargo test --workspace && cargo clippy --all-targets -- -D warnings'`.

   Traps:
   - `Command::Preview` must be constructed by `pump_previews` in this step, or clippy
     flags a never-constructed variant;
   - `handle()`'s match on `phone::Event` is exhaustive, so add the `Previewed` arm here;
   - `Instant` here is `std::time::Instant`, as `serve.rs` uses.

2. **`src/proto.rs`, `src/serve.rs`, `tests/serve.rs`: methods and feature flag.**
   - `proto.rs`: `FEATURES` becomes `&["attachments", "previews"]`.
   - **`serve.rs`, next to the `"media.open" | "media.save"` arm (`:1127-1150`):**
     - **`"media.preview"`:**
       - `MediaParams` with an empty `message` gives `Err("message required")` (which
         is `bad_request`);
       - otherwise allocate a rid and insert `Pending { conn, id, until: now + 60 s }`;
       - `let out = self.sync.as_mut().expect("store checked").preview(rid, &p.message, p.index)`;
       - `self.outputs(out)` and `return`.
     - **`"media.preview.cancel"`:**
       - the same params check;
       - `let rids: Vec<u64> = self.pending.iter().filter(|(_, p)| p.conn == conn).map(|(r, _)| *r).collect();`;
       - `let out = sync.cancel_preview(&p.message, p.index, &rids)`;
       - `self.outputs(out)`;
       - then reply `{}` to this request through the existing `reply = Ok(json!({}))`
         path.

     Check that both method names reach this arm when the store is open but not
     paired. They should be gated like `media.open`: look at how `media.open` is
     refused before pairing (`tests/serve.rs` expects a code), and do the same.
   - **`tests/serve.rs`:**
     - `hello` asserts `features == ["attachments", "previews"]` (update the existing
       assert);
     - `media.preview` and `media.preview.cancel` with `"message": ""` give
       `bad_request`;
     - `media.preview` with a message before pairing gets the same error code as
       `media.open` gets in the existing test.

   → verify with the same command as step 1.

   Traps:
   - the arm returns early after `outputs`, as `media.open` does;
   - `self.pending` values carry `conn`; check the field name in `struct Pending`.

3. **`plugin/Thread.qml`: the picture box, requests and scroll anchoring.** (Client.qml
   is unchanged: `features` and `connected` already exist.) Read `Thread.qml` whole
   first.
   - **State** on `root`:
     - `property int previewGen: 0`;
     - `property var inView: ({})`;
     - `readonly property bool canPreview: root.client.features.indexOf("previews") >= 0`;
     - `readonly property real previewMaxW: Style.space(320)`;
     - `readonly property real previewMaxH: Style.space(240)`.
   - **`function resetPreviews()`:**
     - `previewGen++`;
     - assign `root.ui` a copy without the keys that start with `"p:"`;
     - `root.inView = ({})`.

     Call it:
     - at the start of `openThread` (`:68`);
     - in the `conversationId === ""` branch of `onConversationIdChanged` (`:57`);
     - from a `Connections` on `root.client` when `connected` turns false or `state`
       changes to anything but `"ready"`. Extend the existing `onStateChanged` at
       `:276` and add `onConnectedChanged`.
   - **`function setInView(keys, on)`:** a new object with those keys added or removed,
     then `previewTimer.restart()`.
   - **`Timer { id: previewTimer; interval: 200 }`**, firing `root.pumpPreviews()`:
     - **For each key in `inView` whose `ui` has no entry**, or whose entry has
       `busy == false && !failed && !path && tries < 2`:
       1. Parse the key (`"p:" + message + ":" + index`; the index is the last `:`
          part).
       2. Capture `gen = previewGen` and `conv = conversationId`.
       3. `setUi(key, { busy: true, tries: (old.tries || 0) + 1, gen })`.
       4. Send `client.send("media.preview", { message, index }, cb)`, guarded by
          `client.connected && canPreview`.

       The callback:
       - returns unless `gen === previewGen` and `ui[key]` exists with the same `gen`;
       - on ok: `setUi(key, { path: r.data.path, busy: false, gen })`;
       - on `not_found`: `failed`;
       - on `busy`, `phone_offline`, `timeout` or `disconnected`: `failed` when
         `tries >= 2`, else `busy: false`, then `previewTimer.restart()` so the next
         tick asks again while the key is in view.
     - **For each `ui` key with `p:` and `busy`** that isn't in `inView`: send
       `media.preview.cancel` with the same guards, and remove the entry. Its reply is
       ignored.
   - **In each message delegate** (`bubbleRow`, `:410`):
     - `readonly property bool inViewNow: root.visible && root.canPreview && !isPending && !isFailed && !isDeleted && (y + height > msgList.contentY) && (y < msgList.contentY + msgList.height)`;
     - `readonly property var previewKeys`: for each attachment index `i` with
       `preview === true`, `"p:" + modelData.id + ":" + i`;
     - `onInViewNowChanged: root.setInView(previewKeys, inViewNow)` and
       `Component.onCompleted: if (inViewNow) root.setInView(previewKeys, true)`.

     Never cancel from `onDestruction`.
   - **The box**, in the attachment `ColumnLayout` (`:511`), before the name `RowLayout`:
     `Item { visible: root.canPreview && att.modelData.preview === true && !bubbleRow.isDeleted }`.
     - Size: with `w = att.modelData.width` and `h = att.modelData.height`, if both are
       > 0 then `s = Math.min(previewMaxW / w, previewMaxH / h, 1)` and the box is
       `(Math.round(w * s), Math.round(h * s))`; else `(previewMaxW, Style.space(180))`.
       Set `implicitWidth`/`implicitHeight` to that and
       `Layout.maximumWidth: msgList.width * 0.78 - 2 * root.theme.padX`.
     - A `Rectangle` fill (`Commons.Color.muted`, opacity 0.15, radius
       `Style.cornerRadius`) is always behind.
     - A centred `Text` glyph `"\u{F02E9}"` (nf-md-image-off) is shown when the
       key's state is `failed`.
     - The `Image`:
       - `anchors.fill: parent`, `asynchronous: true`, `cache: false`,
         `autoTransform: true`, `fillMode: Image.PreserveAspectFit`;
       - `sourceSize: Qt.size(2 * implicitWidth, 2 * implicitHeight)`;
       - `source: st.path ? "file://" + st.path : ""`, where `st = root.ui[key] || ({})`;
       - `onStatusChanged`: on `Image.Error` with `st.path`, `setUi(key, { path: "", busy: false, gen: st.gen, tries: st.tries, failed: st.tries >= 2 })`
         and `previewTimer.restart()`.
     - A `MouseArea` over the box, with a pointing-hand cursor, calls
       `root.mediaRequest("media.open", String(bubbleRow.modelData.id), att.index)`.
   - **`mergeMessages`** (`:86-118`): replace the unconditional
     `Qt.callLater(positionViewAtEnd)`.
     - **Before** assigning `root.messages`, compute:
       - `var follow = msgList.atYEnd || root.messages.length === 0`;
       - `var ai = msgList.indexAt(0, msgList.contentY + 1)`;
       - `var anchorId = ai >= 0 ? root.messages[ai].id : ""`;
       - `var item = ai >= 0 ? msgList.itemAtIndex(ai) : null`;
       - `var offset = item ? msgList.contentY - item.y : 0`.
     - **After** assigning, inside `Qt.callLater`:
       - if `follow` or there is no `anchorId`, `positionViewAtEnd()`;
       - otherwise find the new index `ni` of `anchorId` in `root.messages`, or the
         first message with a later `timestamp_ms`. Then
         `msgList.positionViewAtIndex(ni, ListView.Beginning)` and
         `msgList.contentY += offset`.
   - **`msgList`:** set `cacheBuffer: 0`.

   → verify with `nix build .#plugin` (`qmllint`), then
   `nix develop -c sh -c 'cargo test --workspace'`.

   Traps:
   - `indexAt` takes content coordinates, hence `contentY + 1`;
   - `itemAtIndex` can be `null` when the item isn't instantiated;
   - `previewKeys` must use the same index as `media()`'s filtered list, which equals
     the position in `modelData.attachments` (the DTO list is built with the same
     filter, `src/proto.rs:228-240`);
   - `"\u{F02E9}"` needs the surrogate-safe `\u{…}` form already used in the file.

## Tests

```sh
nix develop -c sh -c 'cargo test --workspace && cargo clippy --all-targets -- -D warnings'
nix build .#plugin
```

Expected: everything passes, including the new tests in `sync.rs`, `phone.rs` and
`tests/serve.rs`.

By hand on p620 after the deploy and a shell restart:
- received and sent photos show inline;
- a picture still sending shows a placeholder, then the photo;
- clicking opens the viewer;
- a PDF has no box;
- scrolling back doesn't jump when a message arrives;
- with the phone offline, a box shows the "no preview" glyph without changing size;
- switching A → B → A shows the right pictures;
- an old daemon shows no boxes.

## Rollback

Revert the step commits on the branch, or after the merge revert the merge commit and
bump the `gmessages` input in nixos_config back. The protocol changes are additive, so
mixed versions keep working, with pictures hidden.
