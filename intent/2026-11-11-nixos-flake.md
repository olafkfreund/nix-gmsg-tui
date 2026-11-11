---
status: draft
issue: none (issues are disabled on olafkfreund/nix-gmsg-tui; see open question 1)
author: olafkfreund
---

# Intent: NixOS flake for gmessages-tui

## Problem

gmessages-tui v1.3.6 ships packages for Ubuntu, Fedora and the AUR (`packaging/`), but
nothing for Nix. There is no `flake.nix`, so it cannot be built reproducibly, declared in a
NixOS configuration, or installed on p620 the way the rest of that machine is managed.

The default `wry-signin` feature opens a WebKitGTK window. On NixOS that window needs GTK3,
WebKitGTK 4.1, libsoup3, D-Bus and `glib-networking` (for TLS), all wrapped into the
binary's environment, which no existing recipe covers.

## Proposed outcome

- `nix build` on this repo produces a working `gmessages-tui` with the Google sign-in
  window, and `nix run . -- --version` prints `v1.3.6`.
- A variant without the webview (manual or QR pairing only) is also available.
- `nix flake check` builds the package and runs the crate's own test suite.
- A NixOS module lets a host enable it with one option, and the flake exposes an overlay.
- p620 can install it from this flake, and pairing with the phone works there.

## Affected users and systems

- This repository: new `flake.nix`, `flake.lock` and probably a `nix/` folder. No Rust
  source changes are needed for packaging.
- p620 (NixOS, x86_64): gets the package through its own configuration. That is the only
  host in scope.
- The owner, as the only user. Upstream (louisrubet/gmessages-tui) is unaffected.

## Constraints

- No changes to Rust sources or `Cargo.lock` for packaging. The fork stays easy to rebase
  on upstream.
- The build must work offline in the Nix sandbox. The two git dependencies
  (`chat-tui-core` v1.12.1, `emoji-expander` v1.2.2) are pinned by hash.
- The store key stays in the Secret Service keyring, falling back to a passphrase. The
  flake writes no secrets and asks for none.
- No build or switch on p620 without the owner's OK at that step. Nothing is deployed to
  other hosts.
- x86_64-linux is required. aarch64-linux is nice to have, but its build is not verified.

## Open questions

1. Issues are disabled on the fork. Enable them so this task gets an issue number, or go
   ahead without one?
2. Code review findings (Codex + agy) are upstream bugs, not packaging blockers. Fix the
   small safe ones in this fork as a separate task, report them upstream, or leave them?
3. NixOS module only, or also a home-manager module (`programs.gmessages-tui`)?
4. How does p620 consume it: as a flake input in the p620 configuration repo (which repo
   and path?), or `nix profile install` first for a quick test?
5. Should the default package be the webview build (bigger closure, sign-in window) or
   the lean build?
