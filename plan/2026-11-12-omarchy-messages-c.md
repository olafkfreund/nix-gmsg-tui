---
status: draft
issue: none (issues are disabled on the fork)
spec: spec/2026-11-11-omarchy-messages.md
stage: c (contacts and new chats, archive, restore, delete, unpair, remove all data)
coders:
  1: codex   # Sync: three pending maps, Done/Contacts/Started routing, delete via the Gone path: small, tricky logic
  2: codex   # serve: unpair and uninstall lifecycle (stop, revoke, drain, restart or exit): small, tricky logic
  3: coder   # QML: Confirm, NewChat, list folder switch and row actions, App menu, Client deadlines: plain and broad
reviewers:
  - codex   # independent read of the whole diff against this plan
  - agy     # second independent read; disagreement is the point
---

# Plan: Messages plugin, stage c: contacts, new chats, archive, delete, unpair, remove all data

## What is decided (copied from the approved spec)

- **Scope (spec decision 6, stage c):** contacts and new chats, archive, restore, delete,
  unpair, and remove all data (`uninstall`).
- **Destructive methods (spec §1):** `unpair` and `uninstall` stop the phone, finish or
  fail in-flight work, then delete. Unpair keeps the store. Uninstall deletes the store,
  its keyring entry, the cache and the data dir. They are daemon methods, never CLI flags
  (`cli.rs:43-44` keeps refusing `--unpair` and `--uninstall`).
- **Deadlines (spec §2):** `unpair` 30 s, stage c actions 30 s. The client waits the
  deadline plus 5 s.
- **Plugin (spec §3):** stage c's destructive actions go through `Confirm`. Strings go
  through `I18n` with English fallbacks; backend codes map to strings in `Errors.js`.
- **Verification (spec, item 9):** a new chat starts. Archive and restore work, and
  delete asks first. Unpair keeps the store; uninstall removes everything.

## What exists already (code read, 2026-11-12)

- **Phone layer:** `phone.rs:171-205` has `Command::{Archive, Restore, DeleteChat,
  Contacts, StartChat}`. `run_commands` (`phone.rs:1095-1268`) runs them and answers
  with `Event::Done{id,error}` (`:79`), `Event::Contacts{id,contacts,error}` (`:66`) and
  `Event::Started{id,conversation,error}` (`:72`).
  - Nothing sends these commands.
  - `sync.rs:919-924` drops the events as "stage-a phone event ignored".
- **Unpair:** `phone::unpair(path)` (`phone.rs:1724`) loads `session.json`, revokes the
  device with Google (20 s), and deletes the file whatever Google answers. It returns
  `Unpaired::{NotPaired, Revoked, Forgotten(reason)}`.
- **Keyring:** `key::forget(path)` (`key.rs:72`) deletes the store's keyring entry. A
  missing entry or keyring counts as success. Neither function has callers.
- **Archive:** archived means `folder = 'archive'` (`store.rs:121`). The list filter is
  `sync.rs:979-985`.
- **Gone path:** `Event::Gone` handling (`sync.rs:830`) already forgets a conversation's
  messages, hides the row and broadcasts it.
- **Unit:** `Restart=on-failure`, `RestartPreventExitStatus=3` (`nix/hm-module.nix:35-38`),
  so an exit 0 is not restarted.

## Decided here (new in this plan, for approval)

1. **Protocol methods.** All need `ready`. The deadline is 30 s.

   | Method | Params | Reply `data` |
   |---|---|---|
   | `contacts` | `{}` | `{contacts: [{name, number}]}`, the phone's list as `phone.rs:1221` filters it |
   | `chat.start` | `{number}`, one non-empty number | `{conversation: Conversation}`. The row is stored and broadcast as a `conversation` event first. |
   | `conversation.archive` | `{conversation}` | `{}` |
   | `conversation.restore` | `{conversation}` | `{}` |
   | `conversation.delete` | `{conversation}` | `{}` |

   - `chat.start`: groups are not in stage c, because `start_chat` with several numbers
     can need an RCS group confirmation (`types.rs:1390`). A `chat.start` that fails
     replies `phone_offline` with a fixed message, never the phone's error text.
   - `conversation.delete` deletes for good: `DeleteChat{for_good: true}`, as Messages
     for web does after its confirm. There is no trash view in the plugin.
2. **Store after an action:**
   - Archive or restore that succeeds sends `Command::Lookup(vec![conversation])`, so the
     phone's own answer writes the new `folder`. There's no local folder setter.
   - Delete that succeeds runs the existing `Gone` handling for that conversation.
   - A failure replies `phone_offline` "the phone refused".
3. **Pending replies.** `Sync` keeps a single `actions: HashMap<u64, Action>`, where
   `Action` is `Archive(conv) | Restore(conv) | Delete(conv) | Contacts | Start`.
   - `Done` is routed through it, after the existing `reacts` check.
   - `forget(rid)` and `reset_media` drain it as well: `reset_media` replies
     `not_paired`, the same as reacts.
4. **`unpair`.** It needs the store (a `sync`) and works in any state that has one; with
   none it replies `locked`. The deadline is 30 s. Steps, in order:
   1. Cancel a pairing in progress (as `pair.cancel` does).
   2. `control.stop()`, which saves the freshest session for the revoke.
   3. Reply `not_paired` to every other pending request on every connection.
   4. `phone::unpair(&data/session.json)`.
   5. Start a fresh phone and `Sync` on the same open store, through a function
      extracted from `got_key` (`serve.rs:283-302`). This is what clears media,
      avatars and pending maps.
   6. Set state `login`, then reply `{revoked: bool}`, `false` meaning Google wasn't
      told. Google's reason goes to the journal only.
5. **`uninstall`** ("Remove all data"). It works in every state, `locked` and
   `store_unreadable` included. Steps 1–3 are the same as unpair, then:
   1. Run `phone::unpair` if `session.json` exists, ignoring its outcome except in the
      journal.
   2. Drop `sync` and the store, which closes SQLCipher.
   3. `key::forget(&data/messages.db)`, skipped when `GMESSAGES_TUI_PASSPHRASE` is set.
   4. Remove the cache dir and the runtime `media/` and `avatars/` dirs.
   5. Remove the data dir with `remove_dir_all`. The `lock` file goes too: the open
      flock fd keeps the lock until exit.
   6. Reply `{}`, flush every client's queue, remove the socket, and exit 0. The unit is
      not restarted; it starts again at the next login.

   A failed delete replies `internal` with the path's kind only (store, key, cache,
   data), and the daemon keeps running.
6. **Plugin.**
   - **`Confirm.qml`:** a modal overlay in `App.qml` with title, body, a Cancel button
     (focused first) and a red action button. `Esc` cancels and `Enter` presses the
     focused button.
   - **`ConversationList.qml`, the folder switch:**
     - two tabs above the list: Inbox and Archived;
     - the `a` key toggles them;
     - the folder is sent in the existing `conversations` request (`:86`);
     - the unread badge stays inbox-only.
   - **Row actions:** they show on hover and for the current row.
     - `e` archives in Inbox and restores in Archived.
     - `Delete` or `#` deletes, through Confirm: "Delete this conversation and its
       messages on the phone too?".
     - While an action is in flight the row is dimmed, and a failure shows in the list's
       status line.
   - **`NewChat.qml`:** the `n` key or a "+" button opens it.
     - It shows a filter field over `contacts`, fetched once each time it opens.
     - When the text is a phone number (`/^\+?[0-9 ()-]{3,}$/`), a first row reads
       "Start a chat with <number>".
     - Enter or a click runs `chat.start`, then opens the returned conversation.
   - **Header menu:** a "⋮" button in `App.qml` with "Unpair phone" and "Remove all
     data", both through Confirm.
     - Remove's body names what goes: messages, pairing, the keyring entry, the cache.
     - After `uninstall` the window shows "All data removed. Messages starts again at
       your next login." It does not reconnect-spin.
   - **`Client.qml:69`:** 35 s for `contacts`, `chat.start`, `conversation.*`, `unpair`
     and `uninstall`.
   - **`HelpOverlay.qml`:** add `n`, `a`, `e` and `Delete`.
   - **`Errors.js`:** add `unpaired_ok` and `unpaired_local` (Google not told).

## Rules for every step

- Work on branch `feat/stage-c`. Never on `main`, never in `~/.config/nixos`.
- Implementers never stage, commit, push, deploy, switch, restart, garbage-collect or
  reboot. Claude commits after each step.
- `cargo test --workspace` and `cargo clippy --all-targets -- -D warnings` pass after
  every Rust step. Run them with `nix develop -c sh -c '…'`, because the host lacks the
  openssl headers.
- No message text, numbers, contact names, conversation ids or keys go into `phone.log`
  or `tracing`.
- **Trap:** a git flake ignores untracked files, so a new file is committed before
  `nix build`.
- **Trap:** `qs.Commons` `Color` has `foreground`, `background`, `accent`, `urgent` and
  `muted`, and no `surface`.
- **Trap:** files with `pragma ComponentBehavior: Bound` need `required property` for
  `index` and `modelData` in every delegate.
- **Trap:** the plugin's `ui`-style maps store objects, never bare `true`. See b2's
  review fix A.

## Steps

1. **`src/proto.rs`, `src/sync.rs`: the five actions in `Sync`.**
   - `proto.rs`: `ContactsData`, `ContactDto {name, number}`, `StartChatParams {number}`,
     `ConversationParams {conversation}`.
   - `sync.rs`:
     - `Action` and `actions` (decision 3), with `pub fn contacts(rid)`,
       `start_chat(rid, number)`, `archive(rid, conv)`, `restore(rid, conv)` and
       `delete(rid, conv)`.
     - Replace the drop at `sync.rs:919-924` with routing: `Done` → decision 2,
       `Contacts` → the reply, `Started` → upsert, broadcast `conversation`, then reply.
     - Extend `forget` and `reset_media`.
   - Unit tests with `Commands::detached()`:
     - archive and restore that succeed reply `{}` and send `Lookup`;
     - a delete that succeeds hides the row and broadcasts it;
     - a failed `Done` replies `phone_offline`;
     - `Started` stores the row and broadcasts before replying;
     - `reset_media` answers pending actions with `not_paired`.
   - → verify with `cargo test --workspace && cargo clippy --all-targets -- -D warnings`.
2. **`src/serve.rs` (and `src/main.rs` `allow(dead_code)` notes): methods and lifecycle.**
   - Dispatch the five methods (30 s, `ready`-gated).
   - Extract `start_phone(store)` from `got_key`.
   - Add `unpair` (decision 4) and `uninstall` (decision 5).
   - Remove the `allow(dead_code)` notes that stage c settles (`main.rs:4`, `:13`,
     `:17`) where the code is now used.
   - `tests/serve.rs`:
     - `unpair` in `login` (no session) replies `{revoked: false}`, and the state stays
       `login`;
     - `uninstall` replies `{}`, the process exits 0 within 5 s, and the temp data and
       cache dirs and the socket are gone;
     - the five actions in `login` reply `not_paired`.
   - → verify as step 1.
3. **`plugin/`: `Confirm.qml`, `NewChat.qml`, `ConversationList.qml`, `App.qml`,
   `Client.qml`, `HelpOverlay.qml`, `Errors.js`** (decision 6).
   - → verify with `nix build .#plugin --no-link`, and `qmllint` on the changed files with
     no new warnings beyond the existing `[unqualified]` style.
4. **Lead: deploy and live checks on p620.** Commit, push, relock the NixOS worktree
   (input `github:olafkfreund/nix-gmsg-tui` follows main after merge; until then a branch
   ref), post on the agent bus, and switch p620. Then:
   1. A new chat with a number starts and opens.
   2. Archive moves a chat to Archived; restore brings it back.
   3. Delete asks first, then the chat is gone on the phone.
   4. Unpair: the phone's "Device pairing" list drops this device, the window shows
      sign-in, and history is still in the store after pairing again.
   5. Uninstall is checked on razer, never on p620. razer is unpaired and its store
      holds nothing of value.
      - Afterwards `~/.local/share/gmessages-tui` and `~/.cache/gmessages-tui` are
        gone, the keyring has no `gmessages-tui` entry, and the unit is inactive.
      - After `systemctl --user start gmessages-tui`, the state is `login` with a fresh
        store.

## Tests

```bash
nix develop -c sh -c 'cargo test --workspace && cargo clippy --all-targets -- -D warnings && cargo fmt --check'
nix flake check
nix build .#gmessages-tui .#plugin
```

All green, plus the checks in step 4: 1–4 on p620, 5 on razer.

## Rollback

- Revert this stage's commits, relock the NixOS worktree, and switch.
- Stage c changes no schema.
- An unpair is undone only by pairing again.
- An uninstall can't be undone, because the store's key leaves the keyring with it.
  That is why check 5 runs on razer.
