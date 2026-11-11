# Messages (`olafkfreund.gmessages`)

Omarchy shell plugin for Google Messages (`gmessages-tui serve`).

## Components

- `Service.qml` — background service handling desktop notifications (`notify-send`) and IPC (`omarchy-shell olafkfreund.gmessages toggle|open`).
- `BarWidget.qml` — status bar icon and unread badge.
- `App.qml` — floating window (`olafkfreund.gmessages`) with pairing (`PairView.qml`), two-column / responsive conversation list (`ConversationList.qml`), message thread (`Thread.qml`), composer (`Composer.qml`), and shortcut help (`HelpOverlay.qml`).
- `Client.qml` — NDJSON Unix domain socket client (`$XDG_RUNTIME_DIR/gmessages-tui/daemon.sock`, protocol v1).
- `Theme.qml` & `Errors.js` — shared typography/metrics and localized error/status mapping.
