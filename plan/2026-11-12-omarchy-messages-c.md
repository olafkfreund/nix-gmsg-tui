---
status: approved
issue: none (issues are disabled on the fork)
spec: spec/2026-11-11-omarchy-messages.md
stage: c (contacts and new chats, archive, restore, delete, unpair, remove all data)
coders:
  1: codex   # Sync: one actions map, Done/Contacts/Started routing, gone events, effects that outlive replies: small, tricky logic
  2: codex   # serve: markers, re-exec, startup unpair and uninstall, fatal retry, bounded flush: small, tricky logic
  3: coder   # QML: Confirm, NewChat, folder tabs and row actions, app menu, modal key guard: plain and broad
reviewers:
  - codex   # independent read of the whole diff against this plan
  - agy     # second independent read; disagreement is the point
---

# Plan: Messages plugin, stage c: contacts, new chats, archive, delete, unpair, remove all data

Revised after Codex and agy reviewed the first draft (39f63d4). Both found the same core
problem: unpair and uninstall inside the running daemon race with tasks that
`Control::stop()` does not end:
- connect and follow can save a session again (`phone.rs:723-742`, `:794-796`);
- queued `Incoming::Phone`, `Pasted` and `PairDone` events reach the new `Sync`;
- the key lookup in `spawn_blocking` (`serve.rs:231`) can create a key after an
  uninstall.

They also found:
- a new `Sync` does not clear `media/` and `avatars/`;
- deleting the `lock` file breaks single ownership;
- the reply was not flushed before exit, and the exit path recreated the runtime dirs;
- blanket `not_paired` replies misreport sends that already went out;
- `forget(rid)` dropped the effects of an action;
- partial-failure ordering could leave a store without its key;
- the draft changed the spec's method names and dropped `delete.message` and `gone`;
- QML gaps: a folder-blind upsert, stale pages, key collisions, a menu that can't be
  reached outside `ready`, and a deleted open chat.

This revision runs unpair and uninstall in a fresh process, before anything else starts,
which removes the whole race class. The rest is fixed point by point below.

## What is decided (copied from the approved spec)

- **Scope (spec decision 6, stage c):** contacts and new chats, archive, restore, delete,
  unpair, and remove all data (`uninstall`).
- **Protocol (spec §2 table, stage c):**
  - Methods: `contacts {offset, limit}`, `chat.start {numbers}`, `archive`, `restore`,
    `delete.chat {conversation}`, `delete.message {conversation, message}`, `unpair`,
    `uninstall`.
  - Event: `gone {conversation}`.
  - Deadlines: `unpair` 30 s, stage c actions 30 s. A client waits the deadline plus
    5 s.
- **Disconnects (spec §2):** a client's pending replies are dropped, but their effects
  still reach the store and are broadcast.
- **Destructive methods (spec §1):** unpair keeps the store. Uninstall deletes the store,
  its keyring entry, the cache and the data dir. They are daemon methods, never CLI flags
  (`cli.rs:43-44` keeps refusing `--unpair` and `--uninstall`).
- **Shutdown (spec §1):** SIGTERM stops the phone (session saved), kills the sign-in
  child, clears `runtime/media`, and removes the socket.
- **Plugin (spec §3):**
  - Stage c's destructive actions go through `Confirm`.
  - Strings go through `I18n` with English fallbacks; backend codes map to strings in
    `Errors.js`.
- **DTOs carry no secrets (spec §2):** every new DTO joins the `dtos_carry_no_secrets`
  test (`proto.rs:586`).
- **Verification (spec item 9):**
  - A new chat starts.
  - Archive and restore work, and delete asks first.
  - Unpair keeps the store; uninstall removes everything.
  - The owner OKs each switch, and the shell journal shows no QML errors (spec
    `:427-432`).

## What exists already (code read, 2026-11-12)

- **Phone layer:** `phone.rs:171-208` has
  `Command::{Archive, Restore, DeleteChat, DeleteMessage, Contacts, StartChat}`.
  `run_commands` (`phone.rs:1095-1268`) runs them and answers with:
  - `Event::Done{id,error}` (`:79`);
  - `Event::Contacts{id,contacts,error}` (`:66`);
  - `Event::Started{id,conversation: Option,error}` (`:72`).
  Nothing sends these commands, and `sync.rs:919-924` drops the events.
- **Unpair and keyring:**
  - `phone::unpair(path) -> Result<Unpaired, String>` (`phone.rs:1724`) loads
    `session.json`, revokes with Google (20 s), and deletes the file whatever Google
    answers.
  - `key::forget(path)` (`key.rs:72`) deletes the keyring entry. It treats
    `NoStorageAccess` as success.
  - Neither function has callers.
- **Gone:** `sync.rs:830` forgets a conversation, hides the row and broadcasts it.
- **Replies:** `Daemon::outputs` (`serve.rs:444-456`) writes a reply only while its rid is
  pending, so a late reply to an expired or disconnected request is dropped.
- **Startup and shutdown:**
  - `serve()` (`serve.rs:111-135`) runs, in order: dirs, flock on `<data>/lock`, clearing
    media and avatars, the socket, `serve_bound`, then removing the socket and clearing
    media and avatars.
  - The shutdown tail (`serve.rs:198-209`) aborts pairing, clears clients, and calls
    `control.stop()` within 4 s.
- **Unit:** `Restart=on-failure`, `RestartPreventExitStatus=3`
  (`nix/hm-module.nix:35-38`).

## Decided here (new in this plan, for approval)

1. **Contacts and new chats.** Both are `ready`-gated, with a 30 s deadline.
   - **`contacts {offset, limit ≤ 200}` → `{items: [{name, number}], total}`.**
     - With `offset == 0`, `Sync` fetches afresh (`Command::Contacts`).
     - It keeps the list for paging until the next `offset == 0`. The phone sends at most
       350 (`client.rs:517-521`).
     - The 200 cap keeps a reply under the 1 MiB line limit.
   - **`chat.start {numbers: [1..=10 non-empty]}` → `{conversation}`.**
     - The row goes through the same path as `Event::Conversation` (`sync.rs:801-828`):
       store, avatar queue, unread, broadcast. Then comes the reply.
     - The plugin sends one number in stage c.
   - **Errors:** a phone error, or `Started` with no conversation, replies `phone_offline`
     with a fixed text. Phone error strings go nowhere: they can hold numbers
     (`phone.rs:1596`).
2. **Archive, restore and delete.** These are `archive {conversation}`,
   `restore {conversation}`, `delete.chat {conversation}` and
   `delete.message {conversation, message}`, all `ready`-gated with a 30 s deadline.
   - `delete.chat` deletes for good (`DeleteChat{for_good: true}`), as Messages for web
     does after its confirm. There is no trash view.
   - **On success:**
     - `archive` and `restore` send `Command::Lookup([conversation])`, so the phone's
       answer writes the new `folder`. The phone usually pushes the change as well.
     - `delete.chat` runs the existing Gone handling (`sync.rs:830`) and also broadcasts
       `gone {conversation}`.
     - `delete.message` calls `store.forget_message` and broadcasts
       `gone {conversation, message}`. **Amendment:** the spec's `gone` gains an
       optional `message`, because the spec names no event for a deleted message.
   - **On failure:** the reply is `phone_offline` "the phone refused".
   - **Effects outlive replies:** `Sync.actions: HashMap<u64, Action>` holds what each id
     does on success.
     - `forget(rid)` does **not** remove an action; the daemon drops the late reply.
     - `reset_media` (pair or unpair) drains `actions` with `not_paired` replies.
3. **Unpair and uninstall run in a fresh process.**
   - **When asked:**
     - Both are refused with `busy` while a `send` or any stage c action is pending,
       so no outcome gets misreported.
     - `unpair` needs a store, and replies `not_paired` when there is no
       `session.json`.
     - `uninstall` works in every state (`serve.rs:622-630` lets it through).
   - **Marker and shutdown:**
     1. The daemon writes the marker `<data>/pending-unpair` or
        `<data>/pending-uninstall` (0600, `create`), then replies `{}`.
     2. It stops taking requests, drops each client's sender, and waits for the writer
        tasks for at most 1 s, so the reply goes out.
     3. It runs the normal shutdown tail: pairing aborted, phone stopped with the session
        saved for the revoke, socket removed, media and avatars cleared.
     4. `run()` then re-execs its own binary with `serve` (`CommandExt::exec` on
        `current_exe()`). The flock fd is close-on-exec, so the new process takes the
        lock.
   - **At startup**, after the lock and before the socket and the key lookup:
     - **`pending-unpair`:** run `phone::unpair(<data>/session.json)` and remove the
       marker. The result becomes the first state's `detail`:
       - `unpaired` when revoked;
       - `unpaired_local` when Google wasn't told;
       - `unpair_failed` on `Err`, when the session file can't be deleted.

       Then startup goes on as usual: the store is kept and the state is `login`.
     - **`pending-uninstall`**, in this order:
       1. `phone::unpair`, with its outcome logged by `eprintln!` (the journal) only.
       2. Remove everything in the data dir except `lock` and the marker, and remove the
          cache dir. `NotFound` counts as done.
       3. `key::forget`, always, whatever the key source.
          - It now returns an error for `NoStorageAccess`, because an unreachable
            keyring is not proof of deletion.
          - The one exception is when `GMESSAGES_TUI_PASSPHRASE` is set: then the store
            never used the keyring, and the journal notes the skip.
       4. Remove the marker and exit 0. The unit isn't restarted; it starts again at
          the next login.

       The store goes before its key, so a failure never leaves a store that has lost
       its key.
     - **Uninstall failures:**
       - On any failure the marker stays. Startup goes on into state `fatal` with detail
         `uninstall_failed`, and the reason goes to the journal.
       - In `fatal` only `state` and `uninstall` (retry) work.
       - Each step is idempotent, so a retry, or the next login, finishes the job.
   - **What goes from the spec:**
     - Spec §1 says to stop the phone "without saving the session"; here it is saved
       first so that the revoke uses the newest one, then deleted. The net effect is the
       same.
     - Spec §1 says the data dir is deleted; here the data dir keeps the empty `lock`
       file, because deleting it would let a second daemon take ownership mid-delete.
   - **Signals:** a SIGTERM during the startup unpair waits for its 20 s at most. That is
     inside systemd's stop timeout.
4. **Logs.** Phone and store errors in the new paths go to the journal with `eprintln!`
   and no message text, numbers, names or ids. `phone.rs:1326-1328` (`lookup`) also stops
   logging conversation ids, because archive and restore now call it.
5. **Plugin.**
   - **`Confirm.qml`:** a modal overlay in `App.qml` with title, body, Cancel (focused
     first) and a red action button. `Esc` cancels and `Enter` presses the focused
     button.
   - **Modal key guard:** while `Confirm` or `NewChat` is open, App sets `modal: true`,
     and every window `Shortcut` in `App.qml:79-197` adds `&& !root.modal`.
   - **`ConversationList.qml`, the folder tabs:**
     - Inbox and Archived, with `a` to toggle.
     - `folder` goes into the existing request (`:86`).
     - `upsertConversation` (`:116`, `:127`) tests `(c.folder || "inbox") === root.folder`.
     - `fetchPage` (`:85-105`) carries a generation number, so a stale page is dropped.
     - The unread badge stays inbox-only.
   - **Row actions.** They show on hover and on the current row.
     - `e` archives in Inbox and restores in Archived.
     - `Delete` or `#` runs `delete.chat` through Confirm.
     - The row stays dimmed until its `conversation` or `gone` event arrives, or for
       10 s.
     - A failure shows in the list's status line.
     - If the deleted chat is the open one, App clears `activeConversation`.
   - **Message delete:** in `Thread.qml`, a "Delete" action on a message's hover (`Del`
     on the focused message) runs `delete.message` through Confirm. A
     `gone {conversation, message}` event removes it from the thread.
   - **`NewChat.qml`**, opened by `n` or a "+" button:
     - a filter over `contacts`, paged by 200 and fetched each time it opens;
     - when the text is a phone number (`/^\+?[0-9 ()-]{3,}$/`), a first row "Start a
       chat with <number>";
     - Enter or a click runs `chat.start`, then opens the chat with the Inbox tab
       selected and the filter cleared.
   - **App menu:** a "⋮" button at the window's top-right, above both `PairView` and
     the main layout, so it works in every state.
     - It holds "Unpair phone" (shown when `state` is `ready`, `connecting` or
       `expired`) and "Remove all data" (always shown).
     - Both go through Confirm. Remove's body names what goes: messages, pairing, the
       keyring entry and the cache.
   - **After the request:**
     - App shows "Unpairing…" or "Removing all data…" and treats a disconnect as
       expected.
     - After an uninstall, if no `hello` arrives within 15 s, App shows "All data
       removed. Messages starts again at your next login." and stops its client's
       retry.
     - A `hello` with `fatal`/`uninstall_failed` shows the error and a Retry button
       instead.
     - The bar and service clients keep their normal backoff.
   - **`Client.qml:69`:** 35 s for `contacts`, `chat.start`, `archive`, `restore`,
     `delete.*`, `unpair` and `uninstall`. Add a `gone` event case.
   - **`HelpOverlay.qml`:** add `n`, `a`, `e`, `Delete` and the menu.
   - **`Errors.js`:** add `unpaired`, `unpaired_local`, `unpair_failed` and
     `uninstall_failed`.

## Rules for every step

- Work on branch `feat/stage-c`. Never on `main`, never in `~/.config/nixos`.
- Implementers never stage, commit, push, deploy, switch, restart, garbage-collect or
  reboot. Claude commits after each step.
- `cargo test --workspace` and `cargo clippy --all-targets -- -D warnings` pass after
  every Rust step. Run them with `nix develop -c sh -c '…'`, because the host lacks the
  openssl headers.
- Leave the `allow(dead_code)` attributes in `main.rs` alone. Removing them fails
  `-D warnings` on items that are still unused.
- No message text, numbers, contact names, conversation ids or keys go into `phone.log`,
  `tracing` or the journal.
- **Trap:** a git flake ignores untracked files, so a new file is committed before
  `nix build`.
- **Trap:** `qs.Commons` `Color` has `foreground`, `background`, `accent`, `urgent` and
  `muted`, and no `surface`.
- **Trap:** files with `pragma ComponentBehavior: Bound` need `required property` for
  `index` and `modelData` in every delegate.
- **Trap:** the plugin's `ui`-style maps store objects, never bare `true` (b2 review
  fix A).

## Steps

1. **`src/proto.rs`, `src/sync.rs`, `src/phone.rs:1326-1328`: the six actions in
   `Sync`** (decisions 1, 2 and 4).
   - `proto.rs`:
     - `ContactsParams {offset, limit}`, `ContactsData {items, total}`, `ContactDto`;
     - `StartChatParams {numbers}`, `StartChatData {conversation}`;
     - `DeleteMessageParams {conversation, message}`;
     - `GoneEvent {conversation, message?}`;
     - reuse `ConversationParams` (`proto.rs:318`);
     - add the new DTOs to `dtos_carry_no_secrets`.
   - `sync.rs`:
     - `Action`, `actions`, `contacts_cache`, and the methods `contacts`, `start_chat`,
       `archive`, `restore`, `delete_chat` and `delete_message`;
     - `pub fn busy(&self) -> bool`, true while a send or an action is pending;
     - routing in place of `sync.rs:919-924`, and `reset_media` draining `actions`.
   - Unit tests with `Commands::detached()`:
     - archive and restore that succeed reply `{}` and send `Lookup`;
     - a `delete.chat` that succeeds hides the row and broadcasts `gone`;
     - a `delete.message` that succeeds forgets the message and broadcasts `gone` with
       it;
     - a failed `Done` replies `phone_offline` with a fixed text;
     - `Contacts` pages, and its error replies `phone_offline`;
     - `Started` stores, broadcasts, then replies, and with `None` replies
       `phone_offline`;
     - after `forget(rid)` the effect still happens and no reply is produced;
     - `reset_media` drains `actions` with `not_paired`;
     - `busy()` is true while an action is pending.
   - → verify with `cargo test --workspace && cargo clippy --all-targets -- -D warnings`.
2. **`src/serve.rs`, `src/key.rs:72`: the methods, markers, re-exec and startup work**
   (decision 3).
   - Dispatch the six actions (30 s, `ready`-gated), plus `unpair` and `uninstall`.
   - Let `uninstall` through the `sync.is_none()` gate.
   - Add the marker write, the bounded flush, and an exit value from `serve()` that tells
     `run()` to re-exec.
   - Do the startup marker work before `UnixListener::bind`, and add the `fatal`
     `uninstall_failed` state.
   - `key::forget` returns an error for `NoStorageAccess`.
   - `tests/serve.rs`:
     - `unpair` with no session replies `not_paired`.
     - `uninstall` in `login`: the reply arrives, the process re-execs (the socket comes
       back, then goes) and exits 0 within 10 s. The temp data dir holds only `lock`,
       and the cache dir and socket are gone.
     - `uninstall` in `store_unreadable` (a garbage `messages.db`) does the same.
     - A planted `pending-uninstall` with a read-only data dir starts in `fatal`
       `uninstall_failed` and keeps the marker.
     - The six actions in `login` reply `not_paired`.
     - The tests run with `GMESSAGES_TUI_PASSPHRASE` and no keyring. With the
       passphrase set, an unreachable keyring counts as done, because no key was ever
       put there; the journal says so. Without the passphrase it is a failure
       (decision 3).
   - → verify as step 1.
3. **`plugin/`: `Confirm.qml`, `NewChat.qml`, `ConversationList.qml`, `Thread.qml`,
   `App.qml`, `Client.qml`, `HelpOverlay.qml`, `Errors.js`** (decision 5).
   - → verify with `nix build .#plugin --no-link`, and `qmllint` on the changed files with
     no new warnings beyond the existing `[unqualified]` style.
4. **Lead: deploy and live checks.** Commit, push and merge to main, relock the NixOS
   worktree, and post on the agent bus. The owner OKs each switch. Then:
   1. p620: a new chat with a number starts and opens.
   2. p620: archive moves a chat to Archived, and restore brings it back.
   3. p620: delete of a test chat asks first, then the chat is gone on the phone. A
      deleted test message goes too.
   4. p620: the shell journal shows no QML errors from the plugin.
   5. razer, which is unpaired and holds nothing of value:
      - pair, then unpair: the phone's device list drops it, and the store is kept;
      - then Remove all data: `~/.local/share/gmessages-tui` holds only `lock`,
        `~/.cache/gmessages-tui` is gone, there is no keyring entry, and the unit is
        inactive;
      - `systemctl --user start gmessages-tui` gives `login` with a fresh store.

## Deviations

- **Step 1, `busy()`:**
  - A send that timed out or disconnected stops counting as busy.
  - A forgotten action still applies its effect, but stops counting as busy.
  - Without this, a phone that never answers would block unpair and uninstall until
    the next restart.
- **Step 1, `chat.start` reply:** `{conversation}` carries the conversation id. The full
  row arrives first, as the `conversation` event.

- **Step 3, message delete:**
  - `Delete` acts on the message under the pointer, if there is one, and otherwise on
    the current conversation row. Thread has no message focus.
  - The hovered message is cleared when the chat changes.
- **Step 3, app menu:** "⋮" sits in a thin strip at the window's top, not an overlay,
  because an overlay collides with the list header's buttons in single-column mode.
- **Step 3, after unpair or uninstall:**
  - The wait is 45 s, not 15 s: the old daemon's stop plus the new process's revoke (up
    to 20 s) runs past 15 s.
  - A request sent while the socket is already down shows the error instead of
    "removed".
  - `Client` gains `autoRetry` and `reconnect()`, so the window can stop retrying after
    an uninstall.
- **Step 3, review fixes** (Opus reviewer):
  - a duplicate `onVisibleChanged`, which stopped `App.qml` loading;
  - an "Unpairing…" overlay that never ended;
  - a stale hovered message;
  - two fallbacks on one I18n key, and the new-chat row string order;
  - the un-dim timer for each attempt;
  - keypad Enter;
  - a chat hidden by the phone closes if it is the open one.

## Tests

```bash
nix develop -c sh -c 'cargo test --workspace && cargo clippy --all-targets -- -D warnings && cargo fmt --check'
nix flake check
nix build .#gmessages-tui .#plugin
```

All green, plus the checks in step 4. Unpair and uninstall are never run on p620.

## Rollback

- Revert this stage's commits, relock the NixOS worktree, and switch.
- Stage c changes no schema. A leftover `pending-*` marker is harmless to the old code,
  and can be removed by hand.
- An unpair is undone by pairing again.
- An uninstall can't be undone, because the store's key goes with it. That is why it is
  checked on razer only.
