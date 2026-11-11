---
status: draft
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui; see open question 1)
author: olafkfreund
---

# Intent: NixOS flake for gmessages-tui

## Problem

gmessages-tui v1.3.6 has packaging for Ubuntu, Fedora and the AUR (`packaging/`), but none
for Nix. The owner wants to install it on p620 the way the rest of that NixOS machine is
managed: declared in its configuration and built from this repository.

Native requirements no existing recipe covers for Nix:
- Both variants: OpenSSL (system, for rusqlite's `bundled-sqlcipher`), D-Bus (Secret
  Service keyring) and `pkg-config`.
- The default `wry-signin` variant also needs GTK3, WebKitGTK 4.1 and libsoup3, plus
  `glib-networking` in the runtime environment, or the sign-in window cannot make TLS
  connections.

## Proposed outcome

Build (proved in the Nix sandbox):
- `nix build` produces `gmessages-tui`, and `nix run . -- --version` prints `v1.3.6`.
- A lean variant without the webview builds, and its closure contains no WebKitGTK.
- `nix flake check` runs the workspace test suite (both crates) for both feature sets, or
  names any test that cannot run in the sandbox and why.

Install and runtime (proved on p620, with the owner present at each step):
1. p620 gets the package declaratively from this flake.
2. The sign-in window opens (or the lean build pairs with `--manual`).
3. Pairing with the phone succeeds, by whichever method Google accepts.
4. After a restart, the saved session and the encrypted store reopen without pairing
   again.

## Affected users and systems

- This repository: new Nix files only.
- p620 (NixOS, x86_64), the only host in scope, through its own configuration.
- The owner, as the only user. Upstream (louisrubet/gmessages-tui) is unaffected.

## Constraints

- No changes to Rust sources or `Cargo.lock` for packaging, so the fork stays easy to
  rebase on upstream.
- Builds run offline in the sandbox once dependencies are fetched. Every dependency,
  including the two git ones (`chat-tui-core`, `emoji-expander`), is pinned by content hash.
- WebKit's own sandbox is kept, not disabled to make the window work.
- The store key lives in the Secret Service keyring, else a passphrase. The flake writes no
  secrets and asks for none. Known upstream limit: `src/key.rs:29` aborts on first run if
  the keyring is reachable but refuses to store, instead of falling back. p620 therefore
  needs a working, unlocked Secret Service provider, or `GMESSAGES_TUI_PASSPHRASE`.
- No build or switch on p620 without the owner's OK at that step. No other host is touched.
- x86_64-linux is required. aarch64-linux is nice to have, not verified.

## Open questions

1. Issues are disabled on the fork. Enable them so this task gets an issue number, or go
   ahead without one?
2. Code review found upstream bugs (below). None stops a build. The `key.rs:29` keyring
   abort can stop first run on p620. Fix any of them in this fork as a separate task,
   report them upstream, or leave them?
3. How should p620 consume the flake: as an input of the p620 configuration repo (which
   repo and path?), or `nix profile install` first for a quick test? Is a NixOS or
   home-manager module wanted, or is adding the package enough?
4. Which Secret Service provider runs on p620 (gnome-keyring, KeePassXC, none)?
5. Default package: the webview build (bigger closure, sign-in window) or the lean build?

## Code review findings (context for question 2)

Codex (gpt-6.1-sol) and agy (model not reported). Verified by Claude unless marked.
- `gmessages-proto/src/codec.rs:61`: `String::truncate(40)` on a byte index panics on
  non-ASCII network data. Confirmed.
- `src/key.rs:29`: a keyring write failure aborts startup instead of falling back to a
  passphrase. An empty `GMESSAGES_TUI_PASSPHRASE` is accepted. Confirmed.
- `src/phone.rs:468`: the session (Google cookies, keys) is plaintext JSON at mode 0600
  beside the encrypted store. Confirmed; this is the upstream design.
- `src/store.rs:56`: schema migrations run outside a transaction. Confirmed.
- `gmessages-proto/src/relay.rs:467,832`: a MAC failure resolves as an empty successful
  reply. Both reviewers flagged it; plausible, not traced end to end.
- `gmessages-proto/src/relay.rs:869`: one failed activation may leave the client unready.
  agy only; plausible.
- `gmessages-proto/src/conn.rs:337`: `Set-Cookie` is applied from any host. agy only. Low:
  the one caller that fetches outside URLs (`fetch_group_avatar`) is unused by the app.
- Also reported, not verified: no overall HTTP request timeout, `synced` set from the
  current `latest`, media accepted when truncated to the header, network TTL overflow.
