---
status: draft
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui; see open question 1)
author: olafkfreund
---

# Intent: Google Messages as an Omarchy shell plugin, packaged with Nix

## Problem

gmessages-tui is a terminal app. On the owner's nixarchy/Omarchy desktop, messaging should
look and behave like the rest of the shell: a bar badge for unread messages, a panel that
follows the theme and accent, and desktop notifications. The omarchy shell is Quickshell
with plugins, and omamail already proves the pattern (a Rust backend run as one
`serve` process speaking JSON over stdin/stdout, with QML for the service, bar widget and
panel). gmessages-tui has no such backend mode, and there is no Nix packaging to put any of
it on p620.

What exists to build on:
- `src/phone.rs` (the phone layer: pairing, sync, send, media) is independent of the TUI
  library. Its `Event` and `Command` enums are internal: they carry Google cookies
  (`PairGoogle`) and media decryption keys (`Medium`), so they cannot go to QML as they are.
- The store sync logic (SQLCipher cache, missed and old messages, what is up to date) sits
  in `src/main.rs`, mixed with the TUI, and so does the cookie-paste fallback used when
  Google refuses the embedded sign-in window.
- Pairing by Google sign-in opens a separate WebKitGTK window process
  (`--internal-google-signin`), which works without a terminal.

## Proposed outcome

On p620, from one Nix flake in this repository:
1. An omarchy-shell plugin appears (service, bar widget, panel) and follows the theme,
   accent and window rounding live, like the first-party plugins.
2. The bar shows the unread count; a new message raises a desktop notification.
3. The panel pairs with the phone (Google sign-in window, emoji to tap, or QR), with no
   terminal involved.
4. The panel does what the TUI does today: conversation list, history, send and reply,
   media (view and save), reactions, start a chat from contacts, archive, restore and
   delete, and unpair.
5. After a restart (backend, shell or machine), the session and the encrypted store
   reopen without pairing again. Existing state paths and the keyring entry name
   (`gmessages-tui`) are kept, so data already on disk stays readable.
   Destructive actions ask first, as the TUI does. Unpair keeps the message store;
   only an explicit "remove all data" deletes it.
6. If Google refuses the sign-in window, the panel offers the manual way (paste the
   cookies of a signed-in browser), as the TUI does today.
7. The badge counts unread conversations. Notifications fire once per new incoming
   message: not for history replayed after a reconnect, not for messages sent from the
   phone or the panel, and not while that conversation is open and focused.
8. The TUI is no longer packaged, and its terminal-only dependencies leave the build.
   The flake ships the `serve` backend and the plugin.
9. `nix flake check` builds everything and runs the Rust test suites and backend
   protocol tests over real pipes in the sandbox. What the sandbox cannot prove (the
   sign-in window, the keyring, the live phone) is checked by hand on p620.

Two upstream bugs found in review are fixed, because the headless backend depends on them:
- `src/key.rs:29`: a keyring that refuses to store the key aborts startup. It should report
  the problem and fall back, and an empty `GMESSAGES_TUI_PASSPHRASE` should be refused.
- `gmessages-proto/src/codec.rs:61`: a byte-index truncation panics on non-ASCII data.
All review findings are reported upstream (louisrubet/gmessages-tui).

## Affected users and systems

- This repository: a new backend mode in Rust, a new QML plugin, Nix packaging. The fork
  diverges from upstream in `src/` from here on.
- p620 (NixOS x86_64, nixarchy): the only host. It gets the backend and the plugin through
  its own configuration.
- The owner, as the only user.

## Constraints

- The backend never prompts on a terminal. Key order stays as today:
  `GMESSAGES_TUI_PASSPHRASE` if set (empty is refused), else the Secret Service keyring. A
  locked or missing keyring leaves the backend running and the panel showing the problem
  with a retry. The backend never recreates or overwrites a store it cannot open.
- The protocol to QML is its own small, versioned JSON contract above the store and sync
  layer, not the internal enums. Cookies, session keys and media keys never leave the
  backend; media reaches QML as files in a private cache directory. Nothing secret is
  logged.
- The plugin and backend check each other's protocol version at startup and the panel
  reports a mismatch (for example after a rebuild while the shell keeps the old one).
- Exactly one backend owns the store: a second one refuses, with a clear reason. The
  backend shuts down cleanly (session saved, sign-in window closed) on stdin EOF or
  SIGTERM, so a plugin reload or shell exit leaves nothing behind. A send whose delivery
  is uncertain is reported, never resent automatically.
- Credentials stay where they are today: the SQLCipher store, plus the 0600 session file
  (an upstream design, unchanged here).
- The plugin follows Omarchy's conventions: theme and accent live, `theme.radius`
  corners, keyboard first (`?` lists shortcuts), and every visible string translatable.
  Backend errors are codes the plugin translates; English is the only language shipped
  at first.
  It never edits Omarchy's own config.
- Builds are offline in the Nix sandbox, with every dependency pinned by hash.
- WebKit's sandbox stays on for the sign-in window.
- No build or switch on p620 without the owner's OK at that step. No other host.
- Google Messages for web allows one active web session per account: the backend takes
  over from any other web client the owner has open, as the TUI does.
- The repository is AGPL-3.0-only; the plugin code is under the same license.

## Open questions

1. Issues are disabled on the fork. Enable them so this task gets an issue number, or go
   ahead without one?
2. Plugin id and name: `olafkfreund.gmessages` / "Messages", or something else?
3. How does p620 get the plugin: through a nixarchy/home-manager option that installs
   plugins from a flake (if nixarchy has one), or linked into `~/.config/omarchy/plugins`?
   Which repository holds p620's configuration?
4. Which Secret Service provider runs on p620 (gnome-keyring, KeePassXC, none)?
5. Should the panel open as a shell panel, or as its own window like omamail's
   `openOnClick: Window`?
6. Near-parity is large. Proposed stages, each usable on p620 before the next starts:
   (a) Nix package, `serve` backend, pairing, conversation list, history, send, badge,
   notifications; (b) media, reactions; (c) contacts and new chats, archive, restore,
   delete, unpair. Agree?
