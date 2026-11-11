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
  library. Its `Event` and `Command` enums are the natural protocol, but they are not
  serializable yet.
- The store sync logic (SQLCipher cache, what is fetched, what is up to date) sits in
  `src/main.rs`, mixed with the TUI.
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
5. After a restart, the session and the encrypted store reopen without pairing again.
6. The TUI is no longer packaged. The flake ships the `serve` backend and the plugin.
7. `nix flake check` builds everything and runs the Rust test suites and the backend
   protocol tests in the sandbox.

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

- The backend never prompts on a terminal. The store key comes from the Secret Service
  keyring, else `GMESSAGES_TUI_PASSPHRASE`; anything missing is shown in the panel, not
  worked around quietly.
- Credentials stay where they are today: the SQLCipher store, plus the 0600 session file
  (an upstream design, unchanged here). Nothing secret crosses into QML beyond what it
  displays, and nothing is logged.
- The plugin follows Omarchy's conventions: theme and accent live, `theme.radius`
  corners, keyboard first (`?` lists shortcuts), and every visible string translatable.
  It never edits Omarchy's own config.
- Builds are offline in the Nix sandbox, with every dependency pinned by hash.
- WebKit's sandbox stays on for the sign-in window.
- No build or switch on p620 without the owner's OK at that step. No other host.
- Google Messages for web allows one active web session per account: the backend takes
  over from any other web client the owner has open, as the TUI does.

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
6. Near-parity is large. Is it fine to deliver it in stages (pairing and text chat first,
   then media, reactions and contacts), each one usable on p620?
