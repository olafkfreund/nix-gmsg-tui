---
status: draft
issue: none (issues are disabled on the fork)
spec: spec/2026-11-11-omarchy-messages.md
stage: a (packaging, backend, pairing, conversation list, history, send, badge, notifications)
coders:
  1: codex   # Nix packaging of the current tree: small, needs exact hashes and build iteration
  2: codex   # two small, tricky bug fixes with tests
  3: agy     # large mechanical removal of the TUI: many files, little logic
  4: codex   # store migration in a transaction, typed send outcome, pairing cancel
  5: coder   # wire types and the secrets test: plain serde work
  6: codex   # moving Sync and the notify/unread rules: subtle logic
  7: codex   # daemon core: lock, socket, handshake, dispatch, signals
  8: agy     # QML plugin: many files, follows meetingai's shape
  9: coder   # plugin.nix, hm-module.nix, flake outputs
reviewers:
  - codex   # independent read of the whole diff against this plan
  - agy     # second independent read; disagreement is the point
---

# Plan: Messages plugin, stage a

## What is decided (copied from the approved spec)

**Product.** Google Messages becomes an Omarchy shell plugin, `olafkfreund.gmessages`
("Messages", AGPL-3.0-only). It has three parts:
- a background service (notifications);
- a bar widget (unread badge);
- a `panel` entry `App.qml`, which is a Quickshell `FloatingWindow` (app id
  `olafkfreund.gmessages`).

The TUI is no longer built. Stage a delivers packaging, the backend, pairing, the
conversation list, history, send, the badge and notifications. Stages b (media,
reactions) and c (contacts, new chats, archive, restore, delete, unpair, uninstall) get
their own plans later.

**Backend.** `gmessages-tui serve` runs as a systemd user unit and is the only owner of
the store.
- The CLI is `serve`, `--version` and the hidden `--internal-google-signin`. `--unpair`
  and `--uninstall` leave the CLI.
- Paths (new `src/paths.rs`):
  - data `dirs::data_dir()/gmessages-tui`: session, store, `phone.log`, `lock`;
  - cache `dirs::cache_dir()/gmessages-tui`;
  - runtime `$XDG_RUNTIME_DIR/gmessages-tui`: socket and `media/`. systemd creates it
    (`RuntimeDirectory=`), and `serve` clears it at startup.
- Lifecycle:
  - `flock` with O_CLOEXEC on `<data>/lock`. A second instance exits 1 with "already
    running (pid N)".
  - With the lock held: remove a stale socket, bind `daemon.sock` (0600), accept only
    peers with the same uid (`SO_PEERCRED`).
  - Startup order: lock, socket, key, store, phone.
  - SIGTERM or SIGINT: `phone::Control::stop`, kill the sign-in child, clear `media/`,
    remove the socket, exit within 5 s.
- Store key:
  - `GMESSAGES_TUI_PASSPHRASE` if set; empty is refused. Else the keyring, through
    `spawn_blocking`.
  - A locked, missing or refusing keyring gives the state `locked` or `keyring_missing`
    and a retry on `key.retry` and every 30 s. Never fatal.
  - A store that won't open gives `store_unreadable` and is never recreated.
- Logging: `phone.log` at `info`, cut to its last 1 MiB at startup. It never logs message
  text, cookies, tokens or the pairing URL.

**Protocol v1** (`src/proto.rs`, the only serialized types). NDJSON, at most 1 MiB per
line. A longer line, or more than 1000 queued outgoing lines, disconnects that client.

- Handshake:
  - daemon → `{"event":"hello","data":{"protocol":1,"version":…,"state":…,"detail":…,"unread":N}}`;
  - client → `{"id":0,"method":"hello","params":{"protocol":1,"role":"app|bar|service"}}`.
  - On a mismatch, either side reports `protocol_mismatch`, and the daemon refuses
    everything else from that client.
- Requests and replies:
  - `{"id":N,"method","params"}` → `{"id":N,"ok":true,"data"}` or
    `{"id":N,"ok":false,"error":{"code","message"}}`.
  - Ids are per connection, mapped to global ids internally.
  - A malformed line gets `{"id":null,"ok":false,"error":{"code":"bad_request"}}`.
  - When a client drops, its pending replies are dropped, but their effects still apply
    and are broadcast.
- Deadlines: `send` 120 s, everything else in stage a 15 s. Clients wait the deadline
  plus 5 s.
- States: `starting`, `locked`, `keyring_missing`, `store_unreadable`, `login`,
  `pairing`, `connecting`, `ready`, `expired`, `fatal`. The `state` event is
  `{state, detail}`. A phone logout becomes `login` with detail `unpaired_remotely`.
- Errors: `not_paired`, `locked`, `phone_offline`, `send_rejected`, `send_uncertain`,
  `not_found`, `bad_request`, `unknown_method`, `protocol_mismatch`, `busy`, `timeout`,
  `internal`.
- Stage a methods:
  - `hello`; `state` → `{state, detail, unread}`; `key.retry`;
  - `pair.start {method: google|qr|manual}`; `pair.cancel`;
  - `conversations {folder, offset, limit≤200}` → `{items, total}`;
  - `history {conversation}` → `{messages, truncated}` (the last 200 stored; the phone
    fetch lands later as a `history` event);
  - `send {conversation, text, reply_to?}` → `{status: sent|accepted, message}`;
  - `read {conversation}`; `focus {conversation|null}`.
- Stage a events: `state`, `pairing {emoji?, qr_svg?, manual_url?, message?}`,
  `conversation`, `history {conversation, messages}`,
  `message {conversation, message, notify}`, `missed {count}`, `unread {count}`,
  `typing`.

**Notify rules.** `notify` is true only when all of these hold:
- the message is live (`old: false`) and new to the store (`known: false`);
- it isn't deleted and isn't from me;
- no client's `focus` names its conversation.

Focus belongs to the connection that set it and is cleared on disconnect. A reconnect's
missed messages give one `missed {count}`. `unread` counts inbox conversations with the
stored `unread` flag set; it is re-sent on every change and is correct offline.

**Secrets.** No DTO carries cookies, keys, tokens or media keys. Sign-in uses the
backend's window. The manual path is the localhost page of `src/web.rs`: its URL is shown,
and the cookies go from the browser straight to the backend. `web::Page` aborts its server
on drop.

**Phone layer changes.**
- `pair` keeps its `CancelToken` in `State`, for a new `Command::CancelPairing`.
- `send` returns `Sent(msg) | Accepted(msg) | Rejected(reason) | Uncertain(reason)`.
- `Command::Read` clears the local `unread` flag.
- The store gets `unread INTEGER NOT NULL DEFAULT 0` on `conversations`, through
  migration v5 inside one transaction with its `user_version` update.

**Plugin** (`plugin/`):
- `Client.qml`: `Socket` to `$XDG_RUNTIME_DIR/gmessages-tui/daemon.sock`, handshake,
  per-id deadlines, reconnect with backoff, mismatch shown.
- `Service.qml`: `@notifySend@ -a Messages -A open=Open -- <sender> <text>`, at most 3
  `Process`es at a time. Open shows the window on that conversation. It owns the IPC
  target `olafkfreund.gmessages` (`toggle`, `open <conversation>`).
- `BarWidget.qml`: unread badge, distinct icons for not paired, locked and offline.
- `App.qml` with `ConversationList`, `Thread`, `Composer`, `PairView`, `HelpOverlay`:
  one column narrow, two wide. It sends `focus` while a thread is open and the window is
  active.
- `Theme.qml` from `qs.Commons` `Style`; strings through `I18n`; error codes through
  `Errors.js`.
- Keys: `j`/`k`/arrows, `Enter`, `i`, `Ctrl+Enter`, `Esc`, `/`, `?`, `q`.

**Nix.**
- `nix/package.nix`: `buildRustPackage`, `cargoLock.lockFile`.
  - Native inputs: `pkg-config`, `wrapGAppsHook3`, `perl`.
  - Inputs: `openssl`, `dbus`, `gtk3`, `webkitgtk_4_1`, `libsoup_3`, `glib-networking`.
  - `doCheck` with `--workspace`.
- `nix/plugin.nix`: copy `plugin/`, substitute `@notifySend@`, parse-check every QML
  file.
- `nix/hm-module.nix` `services.gmessages.{enable,package}`: `home.packages` and a user
  unit `gmessages-tui` (`serve`; `PartOf`/`After`/`WantedBy=graphical-session.target`;
  `RuntimeDirectory=gmessages-tui`; `Restart=on-failure`; no sandboxing options).
- `flake.nix` (nixos-unstable): `packages.x86_64-linux.{default,gmessages-tui,plugin}`,
  `overlays.default`, `homeManagerModules.default`, `devShells.default`, `checks`.
- p620's host module in `~/.config/nixos` sets the nixarchy plugin option. The flake's
  module never does.

## Rules for every step

- **Coders (codex, agy, coder):**
  - Edit only the files their step names.
  - May run `nix develop -c cargo build`, `nix develop -c cargo test`,
    `nix develop -c cargo clippy` and `nix develop -c cargo fmt`. Step 1 may also run
    `nix build`.
  - Never stage, commit, push, deploy, switch, or restart services, and never touch
    `~/.config/nixos`, `main` or another host.
- **Claude (session):** reviews each step's diff, runs `nix flake check`, and commits
  each step as its own commit, naming the step.
- **A step is done when:** its "verify" passes, `cargo clippy -- -D warnings` is clean for
  the files it touched, and `cargo fmt --check` passes.
- **Repo traps:**
  - The runtime is single-threaded (`tokio::task::LocalSet`, `Rc` state in `main.rs`),
    so new async code uses `spawn_local` and never needs `Send`.
  - `build.rs` shells out to `git`. In the Nix sandbox there is no `.git`, and it falls
    back to `v<CARGO_PKG_VERSION>`.
  - The sign-in window re-execs `current_exe()`. With `wrapGAppsHook3`, the wrapped
    binary passes its environment to the child.
  - Tests in `src/signin/native.rs` spawn `sh` and read `/proc`.
  - A shell hook here blocks commands whose text mentions restarting services or
    rebooting: keep that out of commands and heredocs.

## Steps

1. **Package the current tree first, as a baseline.** Coder: codex.
   - Files: new `flake.nix`, `flake.lock`, `nix/package.nix`.
   - The package builds the current tree unchanged (TUI included), with the inputs
     above.
   - `cargoLock.outputHashes` for `chat-tui-core-1.12.1` and `emoji-expander-1.2.2`.
     These are temporary: step 3 removes chat-tui.
   - `devShells.default` has rustc, cargo, clippy, rustfmt, rust-analyzer and the same
     inputs.
   - Verify:
     ```bash
     nix build .#gmessages-tui
     ./result/bin/gmessages-tui --version   # prints v1.3.6
     nix develop -c cargo test --workspace  # green, or a named skip list with reasons
     ```
   - Traps: the git deps are pinned by tag in `Cargo.toml` and by commit in
     `Cargo.lock` (lines 543 and 1099). The hash comes from the first failing build.

2. **Bug fixes.** Coder: codex.
   - Files: `src/key.rs`, `gmessages-proto/src/codec.rs:57-70`.
   - `key.rs`:
     - an empty `GMESSAGES_TUI_PASSPHRASE` is an error;
     - `store_key` returns a typed `KeyError { Locked, Missing, Refused(String),
       Empty }` instead of prompting, and the rpassword fallback is deleted;
     - a failed `set_password` is `Refused`, never a panic or an abort;
     - keyring calls stay synchronous here (step 7 wraps them in `spawn_blocking`).
   - `codec.rs`: `PbLiteError::ty` truncates to 40 chars on a char boundary.
   - Tests:
     - `key.rs`: empty env refused; env wins over the keyring.
     - `codec.rs`: a 21×"é" JSON string doesn't panic and ends with "…".
   - Verify:
     ```bash
     nix develop -c cargo test -p gmessages-proto codec
     nix develop -c cargo test key
     ```
   - Traps: `main.rs:177` calls `key::store_key`. Keep it compiling with a
     `map_err(|e| e.to_string())` until step 7 replaces it.

3. **Remove the TUI.** Coder: agy.
   - Files: `Cargo.toml`, `Cargo.lock`, `src/main.rs`, `src/cli.rs`, `src/pair.rs`,
     `src/web.rs`, `src/store.rs` (Attachment only), new `src/paths.rs`, `nix/package.nix`
     (outputHashes).
   - `Cargo.toml`: drop `chat-tui-core`, `crossterm` and `rpassword`; add `qrcode` and
     `dirs` at the versions already in `Cargo.lock`. Regenerate the lock with
     `cargo update -w` only, no upgrades.
   - `src/paths.rs`: `data()`, `cache()`, `runtime()` per the spec, and
     `create_private_dir` moved here from `main.rs:872`.
   - `src/main.rs`:
     - delete `run`, `events`, `GMessages`, `Waiting`, `answer`, `discussion`, `message`,
       `attachment`, `stored_discussion`, `folder`, `unpair`, `uninstall`,
       `unpair_session` and the `chat_tui` imports;
     - move `Sync` (`main.rs:370-573`) as-is into a new `src/sync.rs`, keeping its
       chat-tui-typed code in a `#[cfg(any())]` block for step 6 to port;
     - `main()` dispatches `serve` (a stub that prints "not yet" and exits 2),
       `--version` and `--internal-google-signin`.
   - `src/cli.rs`: the commands become `Serve`, `Version` and `InternalSignin`. Update
     its tests.
   - `src/pair.rs`:
     - delete `read_terminal`, `curl_cookies`, `link`, the crossterm imports and the
       terminal thread in `ManualPairing::start`/`Drop`;
     - `say` is replaced by a `tracing::info!`;
     - `ManualPairing` exposes `url()`;
     - keep `parse`, `pairs` and the cookie tests that don't use cURL.
   - `src/web.rs`: `Page` keeps the server's `JoinHandle` and aborts it in `Drop`. Add a
     test: after drop, the port refuses connections.
   - `src/store.rs:9`: replace `chat_tui::data::Attachment` with a local
     `pub struct Attachment` with the same fields, deriving `Debug`, `Clone` and
     `PartialEq`.
   - `nix/package.nix`: remove the chat-tui hash, and the emoji-expander one if it's no
     longer in the lock.
   - Verify:
     ```bash
     nix develop -c cargo build
     nix develop -c cargo test --workspace
     grep -rn 'chat_tui\|crossterm\|rpassword' src Cargo.toml   # no output
     nix build .#gmessages-tui
     ```
   - Traps: README mentions of the TUI stay until step 9. `src/signin/native.rs` must
     keep compiling untouched.

4. **Store and phone changes.** Coder: codex.
   - Files: `src/store.rs`, `src/phone.rs`.
   - `store.rs`:
     - `SCHEMA` 5; migration v5 adds the `unread` column inside one transaction with its
       `PRAGMA user_version = 5`;
     - a store newer than 5 is refused with an error, never downgraded;
     - `save_conversations` writes `unread`;
     - add `set_unread(id, bool)` and `unread_count()` (inbox only, not hidden).
   - `phone.rs`:
     - `State` keeps `pairing_cancel: Option<gm::CancelToken>`, and the new
       `Command::CancelPairing` cancels it;
     - `send` returns `pub enum SendOutcome { Sent(Message), Accepted(Message),
       Rejected(String), Uncertain(String) }`: transport errors after `send_message`
       began are `Uncertain`, a non-`Accepted` result is `Rejected`, and the echo timeout
       is `Accepted`;
     - `Event::Sent` carries `SendOutcome`;
     - `Command::Read` is unchanged on the phone side (the store is updated in `Sync`,
       step 6).
   - Tests:
     - a v4 store migrates to v5 and keeps its rows;
     - a failed migration leaves v4 intact (inject a failure with a `#[cfg(test)]`
       hook);
     - `unread_count` ignores archived and hidden conversations.
   - Verify:
     ```bash
     nix develop -c cargo test store
     nix develop -c cargo test phone
     ```
   - Traps: the `Forget` guard in `send` (`phone.rs:1311`) must still run on every path.

5. **Wire types.** Coder: coder.
   - Files: new `src/proto.rs`.
   - Serde types for every stage-a method's params and reply data, every event, the
     envelope (`Request`, `Reply`, `EventLine`), `State`, `ErrorCode` (snake_case) and
     `PROTOCOL: u32 = 1`.
   - DTOs: `Conversation {id, name, group, folder, unread, last_ms, snippet}`, and
     `Message {id, conversation, from_me, sender, text, timestamp_ms, quote?, deleted,
     failed, attachments: [{name, mime, size}]}`, with no keys.
   - Conversions: `From<&phone::Conversation>`, `From<&phone::Message>`,
     `From<&store::Conversation>`.
   - Tests:
     - round trip of each envelope;
     - the secrets test: build every source type with marker strings in each secret
       field (`Medium` keys, cookie maps, tokens), serialize every DTO, and assert that
       no marker appears;
     - a bad line maps to `bad_request`.
   - Verify:
     ```bash
     nix develop -c cargo test proto
     ```
   - Traps: `phone::Message.media` holds `Medium` with keys: never derive a DTO from it
     by `Serialize` passthrough.

6. **Port `Sync`.** Coder: codex.
   - Files: `src/sync.rs`.
   - Port the step 3 `#[cfg(any())]` code to emit `proto` events.
   - `Sync::handle(phone::Event) -> Vec<proto::EventLine>` covers `Ready`, `History`,
     `Message`, `Conversation`, `Gone`, `Typing`, `Sent`, `Status`, `Expired`, `Fatal`
     and the pairing events.
   - Notify rules exactly as decided. `Sync` holds the focus set (connection id →
     conversation) and exposes `set_focus(conn, Option<id>)` and `drop_conn(conn)`.
   - Missed messages give one `missed {count}` once every fetch is done (the old
     `Event::Missed`).
   - `unread {count}` is emitted when `store.unread_count()` changes. `read(conv)` calls
     `Command::Read` and `store.set_unread(conv, false)`.
   - `Fatal` from a phone logout becomes state `login`/`unpaired_remotely`; other
     `Fatal`s become `fatal`.
   - Tests (with `Commands::detached()` and a temp store):
     - a live new message notifies;
     - a known message update doesn't;
     - `old` replay doesn't, and gives one `missed`;
     - from-me doesn't;
     - a focused conversation doesn't, and focus clears on `drop_conn`;
     - `unread` changes after `read`.
   - Verify:
     ```bash
     nix develop -c cargo test sync
     ```
   - Traps: `Sync::live` (old `main.rs:460-496`) has the `known` and `deleted` checks.
     Keep them.

7. **The daemon.** Coder: codex.
   - Files: new `src/serve.rs`, `src/main.rs` (the `serve` arm), `src/key.rs` call site,
     new `tests/serve.rs`.
   - Startup:
     - `serve()` follows the lifecycle exactly as decided (lock → socket → key → store →
       phone);
     - the key comes through `spawn_blocking`, with the `locked`/`keyring_missing` retry
       loop (30 s, plus `key.retry`);
     - `phone.log` is cut to 1 MiB.
   - Clients:
     - per-client reader and writer tasks, the 1 MiB line limit, and a 1000-line
       outgoing queue limit;
     - handshake gate, id mapping, deadlines, and broadcast of every
       `Sync::handle` output.
   - Pairing:
     - `pair.start` drives `pair::Pairing`/`phone::Command::PairGoogle|PairQr`;
     - the QR is rendered to SVG with `qrcode`, the manual URL comes from
       `ManualPairing::url()`, and the emoji and messages go out as `pairing` events;
     - `pair.cancel` sends `CancelPairing` and drops the pairing.
   - Signals: SIGTERM/SIGINT through `tokio::signal::unix`, then the cleanup as decided.
   - `tests/serve.rs` (spawns `env!("CARGO_BIN_EXE_gmessages-tui") serve` with temp
     `XDG_DATA_HOME`, `XDG_CACHE_HOME` and `XDG_RUNTIME_DIR`, and
     `GMESSAGES_TUI_PASSPHRASE=test`):
     - hello, then state `login`;
     - the client's hello is accepted; protocol 2 gets `protocol_mismatch`, and the
       next method is refused;
     - garbage gets `bad_request` and the socket stays open;
     - a second `serve` exits 1 with "already running";
     - `kill -9`, then a new `serve` binds again (stale socket);
     - SIGTERM: the socket is gone, `media/` is empty, exit within 5 s;
     - `conversations` on an empty store gives `{items:[], total:0}`.
   - Verify:
     ```bash
     nix develop -c cargo test --test serve
     nix develop -c cargo test --workspace
     ```
   - Traps:
     - `LocalSet`: wrap the server in the same runtime as `phone::Phone`
       (`main.rs:83`).
     - Never log request params (they hold message text).
     - Tests must not touch the real keyring: the env passphrase short-circuits it.

8. **The QML plugin.** Coder: agy.
   - Files: new `plugin/manifest.json`, `plugin/Client.qml`, `plugin/Service.qml`,
     `plugin/BarWidget.qml`, `plugin/App.qml`, `plugin/ConversationList.qml`,
     `plugin/Thread.qml`, `plugin/Composer.qml`, `plugin/PairView.qml`,
     `plugin/HelpOverlay.qml`, `plugin/Theme.qml`, `plugin/Errors.js`,
     `plugin/README.md`.
   - Shape, imports and Style use follow
     `/nix/store/clj2idy649r7lzd2wqy0w56r8sxxv46y-nixarchy-meetingai-plugin/{manifest.json,Client.qml,Theme.qml,BarWidget.qml,Service.qml}`
     (read only). The manifest follows
     `~/.config/omarchy/plugins/omamail/manifest.json` for the `panel` kind.
   - Behaviour exactly as decided:
     - the handshake with `role`;
     - `focus` sent only while a thread is open and the window is active, and `null`
       otherwise;
     - `PairView` shows the emoji, the QR SVG (as an `Image` from a data URL) and the
       manual URL, with buttons for google, qr and manual;
     - `Thread` asks `history` on open and merges `history` and `message` events by
       message id;
     - `Composer` sends with `Ctrl+Enter` and shows `accepted` as "sending…" and
       `send_uncertain` as "may not have been sent";
     - every visible string uses `I18n` the way first-party plugins do (copy the
       pattern from meetingai's `UiText.qml`).
   - Verify:
     ```bash
     nix develop -c qmllint plugin/*.qml   # parse only; unresolved qs.* imports are expected
     ```
     Plus a review against the meetingai shape.
   - Traps:
     - Never `console.log` message text.
     - No `localStorage` or files of its own: all state comes from the daemon.
     - `@notifySend@` stays a literal placeholder in `Service.qml`.

9. **Nix outputs.** Coder: coder.
   - Files: `nix/plugin.nix`, `nix/hm-module.nix`, `flake.nix`, `README.md` (a new
     section "Omarchy plugin (Nix)" at the top; the TUI usage sections are replaced by a
     pointer to it).
   - Contents exactly as decided.
   - `checks.x86_64-linux`:
     - `gmessages-tui` (with tests);
     - `plugin` (its `installCheckPhase` parses each QML file with `qmllint` from
       `qt6.qtdeclarative` and checks that `@notifySend@` was substituted);
     - `hm-module`: evaluate a minimal home-manager config that enables the module and
       checks the unit's `ExecStart`.
   - Verify:
     ```bash
     nix flake check
     nix build .#plugin
     grep -c notify-send result/Service.qml   # 1 or more, and no @notifySend@
     ```
   - Traps: home-manager is needed only for the eval check. Add it as an input with
     `inputs.nixpkgs.follows = "nixpkgs"`.

10. **Install and test on p620.** Implementer: Claude, with the owner.
    - Files: `~/.config/nixos/flake.nix` (new input `gmessages`, pointing at this
      branch's path), and a new `hosts/common/nixos/omarchy-gmessages.nix` modelled on
      `omarchy-omateams.nix`:
      - import `inputs.gmessages.homeManagerModules.default`;
      - `services.gmessages.enable = true`;
      - `programs.nixarchy.plugins."olafkfreund.gmessages".src = …plugin`;
      - `local.omarchy.enableOnce = [ "olafkfreund.gmessages" ]`.

      Imported from `hosts/p620/nixos/` only.
    - Before switching: post on the agent bus, show the owner the diff and the
      `nixos-rebuild build` result, and get an explicit OK.
    - Then switch, and run the spec's p620 checks 1–7 with the owner:
      - check 1: the user manager's environment has `WAYLAND_DISPLAY`;
      - add: a 1 MiB line reaches the real `Client.qml`;
      - add: `phone.log` holds no message text.
    - Commit in `~/.config/nixos` only when the owner says so, following that repo's
      own rules.
    - Verify: all checks pass, or each failure is written down with its cause before
      any fix.
    - Traps: `~/.config/nixos` has its own `AGENTS.md`/CLAUDE rules; read them first.
      Never touch p510 or razer.

## Tests

| When | Command | Expected |
|------|---------|----------|
| After each of steps 1–9 | that step's verify | green |
| After step 9 | `nix flake check` | all checks pass |
| After step 9 | `nix run . -- --version` | `v1.3.6` |
| Step 10 | the spec's p620 checks 1–7 and the two additions | each confirmed with the owner |
| Review | Opus review (plan path + `git diff main...`), then Codex and agy, read-only | findings reported; none approves anything |

## Rollback

- In this repo, each step is its own commit on `feat/nixos-flake`: revert the step's
  commit. `main` is untouched until the PR is merged.
- On p620: remove the import of `omarchy-gmessages.nix` and the `gmessages` input, then
  `nixos-rebuild switch` (with the owner's OK), or boot the previous generation. The
  store in `~/.local/share/gmessages-tui` stays. Deleting it is a manual step the owner
  decides.
