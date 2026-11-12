# gmessages-tui

**Google Messages (RCS and SMS) in your terminal.** Pair it with your Android phone, the way Google Messages for web does, and read and answer your texts without leaving the terminal.

- **Native, not a wrapped web page**: a Rust program speaking the Google Messages for web protocol itself, through [gmessages-proto](gmessages-proto/), a library written for it.
- **Images in the terminal**: thumbnails inline (kitty graphics protocol), pasted with `Ctrl+V`, opened full size with `o`.
- **The whole phone**: inbox, archive, spam and trash folders kept in sync both ways; new chats with contacts or numbers, groups; read state, typing, desktop notifications; messages not sent shown as on the phone, with resend.
- **Keyboard first**: fuzzy `:emoji` picker, search across chats and messages ignoring accents, pinned chats, batch actions.
- **Offline reading**: history in a local store encrypted with SQLCipher, its key in the OS keyring.

Built on the protocol work of [mautrix-gmessages](https://github.com/mautrix/gmessages) and its library [libgm](https://github.com/mautrix/gmessages/tree/main/pkg/libgm), by Tulir Asokan and the mautrix contributors: they reverse-engineered Google Messages for web, and this project would not exist without them.

**Not affiliated with Google.** Google Messages is a trademark of Google LLC.

## Omarchy plugin (Nix)

This fork turns gmessages-tui into **Messages**, an Omarchy shell plugin (`olafkfreund.gmessages`). It has a bar icon with an unread count, a window with your conversations and one thread, desktop notifications, and pairing from the window. It has two parts:

- `gmessages-tui serve`, a user service that owns the session and the encrypted store. It talks to the plugin over `$XDG_RUNTIME_DIR/gmessages-tui/daemon.sock`, a socket only your user can open.
- `plugin/`: the Quickshell QML that the shell loads.

The terminal interface isn't built in this fork; use [upstream gmessages-tui](https://github.com/louisrubet/gmessages-tui) for it.

Flake outputs (x86_64-linux):

| Output | What |
|---|---|
| `packages.gmessages-tui` (`default`) | the binary, with `serve` and the sign-in window |
| `packages.plugin` | the plugin directory, with `notify-send` patched in |
| `overlays.default` | `gmessages-tui` and `gmessages-plugin` |
| `homeManagerModules.default` | `services.gmessages`: the package and the `gmessages-tui` user service |
| `checks` | both packages, with the Rust tests and a QML parse check |

With home-manager on its own:

```nix
{
  imports = [ inputs.gmessages.homeManagerModules.default ];
  services.gmessages.enable = true;
}
```

On a Nixarchy host, as p620 does, add the plugin too:

```nix
{ inputs, pkgs, ... }:
{
  home-manager.users.<you> = {
    imports = [ inputs.gmessages.homeManagerModules.default ];
    services.gmessages.enable = true;
    programs.nixarchy.plugins."olafkfreund.gmessages".src =
      inputs.gmessages.packages.${pkgs.system}.plugin;
    local.omarchy.enableOnce = [ "olafkfreund.gmessages" ];
  };
}
```

State lives where it did: the session, the store and `phone.log` in `~/.local/share/gmessages-tui/`, the cache in `~/.cache/gmessages-tui/`, and the store key in the keyring under the service `gmessages-tui`. The service logs to the journal:

```sh
journalctl --user -u gmessages-tui
```

## How it works

```
gmessages-tui  ──HTTPS──▶  Google's relay (as messages.google.com)  ◀──▶  Google Messages on your phone
```

Google Messages has no public API. Like Google Messages for web, gmessages-tui never talks to the phone directly: both talk to a Google relay. The phone holds the messages and sends them, so **it must stay online**.

- Everything exchanged with the phone is encrypted with keys shared at pairing; media are encrypted per file.
- No third-party server: gmessages-tui talks only to Google, as the web client does.
- The protocol is documented in [doc/protocol/](doc/protocol/): a specification written from the analysis of [libgm](https://github.com/mautrix/gmessages/tree/main/pkg/libgm), the Go library of the mautrix-gmessages Matrix bridge, whose authors discovered it.
- [gmessages-proto](gmessages-proto/) implements it in Rust from that specification alone (a clean-room implementation, without libgm's code). It is a reusable library, AGPL-3.0 like the rest.
- The interface is [chat-tui-core](https://github.com/louisrubet/chat-tui-core), shared with [signal-tui](https://github.com/louisrubet/signal-tui), a Signal client built the same way.

## FAQ

- **iPhone?** No: Google Messages for web, and so gmessages-tui, need Google Messages on an Android phone.
- **Is it official?** No. It uses the same undocumented API as messages.google.com; Google may change it at any time, and its terms of service apply. Use at your own risk.
- **Does my phone need to be on?** Yes, and online: it sends and receives everything, as with the web client.
- **Where do my messages go?** From your phone to your terminal, through Google's relay, as with messages.google.com. Locally, they are kept in an encrypted store (see [Security details](#security-details)).
- **What is out of scope?** Calls, stickers, and recording voice messages.

## Security details

The state holds the session (`session.json`: it gives access to your messages through the phone), the store and the log (`phone.log`).

- **Encrypted at rest**: the store (`messages.db`: conversations, messages, media) uses SQLCipher.
- **The key** is 32 random bytes generated on the first start, kept in the OS keyring: Secret Service on Linux (GNOME Keyring, KWallet), Keychain on macOS, Credential Manager on Windows, under the service `gmessages-tui`.
  To see it on Linux: `secret-tool search service gmessages-tui`.
- **No keyring** (headless Linux, SSH): set `GMESSAGES_TUI_PASSPHRASE` for the service; an empty one is refused. A locked keyring is retried every 30 s until it is unlocked; a key missing for an existing store is reported, never replaced by a new one.
- **Files** are created readable by you only (Unix).
- **Sign-in**: the window is private and saves no browser profile; once paired, the session is all that is kept.
- **Unpair**: remove the device from your phone (*Device pairing*); the service then shows pairing again. Uninstalling: disable the service, then delete the keyring entry and the two state directories.
- **Limits**: a program running in your session can read the unlocked keyring. Full disk encryption protects the rest of your machine.

## Upgrading from 1.2 and earlier

Earlier versions embedded gmd, a Go helper built on libgm. Its session is converted on the first start of 1.3, without pairing again; the old file is kept as `session.gmd.json`.

## Dev

```sh
cargo run -- serve                            # the service
cargo test --workspace                        # the app and gmessages-proto
cargo run --example wry_signin --features wry-signin   # does Google accept the webview?
GMESSAGES_TUI_LOG=gmessages_proto=trace cargo run -- serve   # log everything to phone.log
```

## Credits

- **[mautrix-gmessages](https://github.com/mautrix/gmessages)** and **[libgm](https://github.com/mautrix/gmessages/tree/main/pkg/libgm)**, by Tulir Asokan and the mautrix contributors (AGPL-3.0): the reverse engineering of Google Messages for web that this project stands on. The protocol specification in [doc/protocol/](doc/protocol/) was written from their work, and gmessages-tui up to 1.2 ran on libgm itself.
- [UKEY2](https://github.com/google/ukey2) by Google, the key exchange of account pairing.
- The photos of the demo GIF: Wikimedia Commons, CC BY-SA (authors in [examples/demo/](examples/demo/README.md)).

## License

[AGPL-3.0-only](LICENSE), gmessages-tui and gmessages-proto alike.
