pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import QtQuick.Layouts
import Quickshell
import qs.Commons
import qs.Commons as Commons
import "Errors.js" as Errors

FloatingWindow {
    id: root

    property var shell: null
    property var manifest: null

    title: I18n.t("gmessages.app_name", "Messages")
    implicitWidth: 920
    implicitHeight: 640
    minimumSize: Qt.size(420, 380)
    color: Commons.Color.background

    Theme { id: appTheme }

    Client {
        id: appClient
        role: "app"
    }

    property string activeConversation: ""
    property string activeConversationName: ""
    property bool helpOpen: false
    property string lastReportedFocus: "__unset__"

    // Modal layers: window shortcuts stand down while one is open.
    property bool confirmOpen: false
    property string confirmTitle: ""
    property string confirmBody: ""
    property string confirmLabel: ""
    property var confirmAction: null
    property bool newChatOpen: false
    property bool menuOpen: false
    readonly property bool modal: root.confirmOpen || root.newChatOpen || root.lifecycle !== "" || root.uninstallFailed

    // "" | "unpairing" | "removing" | "removed": the daemon restarts itself for both.
    property string lifecycle: ""
    property string note: ""
    property bool noteError: false
    readonly property bool uninstallFailed: appClient.state === "fatal" && String(appClient.detail) === "uninstall_failed"

    function ask(title, body, label, action) {
        root.menuOpen = false;
        root.note = "";
        root.confirmTitle = title;
        root.confirmBody = body;
        root.confirmLabel = label;
        root.confirmAction = action;
        root.confirmOpen = true;
    }

    function closeConfirm() {
        root.confirmOpen = false;
        root.confirmAction = null;
    }

    // The daemon drops the connection as it restarts; that is the expected end of the request.
    function lifecycleRequest(method, phase) {
        root.note = "";
        root.noteError = false;
        if (!appClient.connected) {
            root.noteError = true;
            root.note = Errors.format(I18n, "disconnected", "Disconnected from the background service.");
            return;
        }
        root.lifecycle = phase;
        // Neither phase ends sooner than the restart; a stuck one is cleared by the timer.
        lifecycleTimer.restart();
        appClient.send(method, {}, function(r) {
            // A disconnect after the request went out is the daemon restarting.
            if (r.ok || (r.error && r.error.code === "disconnected")) return;
            lifecycleTimer.stop();
            root.lifecycle = "";
            root.noteError = true;
            root.note = Errors.formatError(I18n, r.error);
        });
    }

    function askUnpair() {
        root.ask(
            I18n.t("gmessages.unpair_title", "Unpair phone?"),
            I18n.t("gmessages.unpair_body", "This signs Messages out of your phone. Your stored messages stay on this computer."),
            I18n.t("gmessages.unpair_action", "Unpair"),
            function() { root.lifecycleRequest("unpair", "unpairing"); });
    }

    function askUninstall() {
        root.ask(
            I18n.t("gmessages.uninstall_title", "Remove all data?"),
            I18n.t("gmessages.uninstall_body", "This deletes your stored messages, the pairing, the keyring entry and the cache. It cannot be undone."),
            I18n.t("gmessages.uninstall", "Remove all data"),
            function() { root.lifecycleRequest("uninstall", "removing"); });
    }

    // Old daemon stops in up to 5 s, the new process may revoke for 20 s before it binds.
    Timer {
        id: lifecycleTimer
        interval: 45000
        onTriggered: {
            if (root.lifecycle === "removing") {
                root.lifecycle = "removed";
                appClient.autoRetry = false;
            } else if (root.lifecycle === "unpairing") {
                root.lifecycle = "";
                root.noteError = true;
                root.note = Errors.format(I18n, "timeout", "Request timed out.");
            }
        }
    }


    // Plain keys belong to a text field that has focus, not to navigation.
    readonly property bool editing: threadView.editing || convList.editing
    // FloatingWindow is a Quickshell wrapper with no `active`; ask the native window.
    readonly property var nativeWindow: root.contentItem ? root.contentItem.Window.window : null
    readonly property bool windowActive: root.nativeWindow ? root.nativeWindow.active === true : false
    readonly property bool twoColumns: root.width >= 720
    readonly property bool ready: appClient.connected && !appClient.mismatch && appClient.state === "ready"
    readonly property bool threadVisible: root.ready && root.activeConversation !== "" && (root.twoColumns || root.activeConversation !== "")

    function openConversation(convId) {
        var c = String(convId || "");
        if (c === "") return;
        convList.selectConversation(c, "");
    }

    function syncFocus() {
        if (!appClient.connected || appClient.state !== "ready") {
            root.lastReportedFocus = "__unset__";
            return;
        }
        var focusedConv = (root.visible && root.windowActive && root.ready && root.activeConversation !== "")
            ? root.activeConversation
            : null;
        var key = focusedConv === null ? "__null__" : focusedConv;
        if (key === root.lastReportedFocus) return;
        root.lastReportedFocus = key;
        appClient.send("focus", { conversation: focusedConv });
    }

    onVisibleChanged: {
        root.syncFocus();
    }
    onReadyChanged: {
        if (!root.ready) root.newChatOpen = false;
    }
    onWindowActiveChanged: root.syncFocus()
    onActiveConversationChanged: root.syncFocus()

    Connections {
        target: appClient
        function onConnectedChanged() {
            root.lastReportedFocus = "__unset__";
            root.syncFocus();
        }
        function onStateChanged() {
            root.lastReportedFocus = "__unset__";
            root.syncFocus();
        }
        // A fresh handshake after the restart: the request is over, its outcome is the state.
        function onResynced() {
            if (root.lifecycle === "unpairing") {
                let d = String(appClient.detail || "");
                if (d === "unpaired" || d === "unpaired_local" || d === "unpair_failed") {
                    root.noteError = d !== "unpaired";
                    root.note = Errors.format(I18n, d, d);
                }
            }
            if (root.lifecycle !== "") {
                lifecycleTimer.stop();
                root.lifecycle = "";
            }
        }
    }

    // Shortcuts (application context)
    Shortcut {
        sequence: "j"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) convList.moveSelection(1);
        }
    }

    Shortcut {
        sequence: "k"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) convList.moveSelection(-1);
        }
    }

    Shortcut {
        sequence: "Down"
        context: Qt.WindowShortcut
        enabled: !threadView.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) convList.moveSelection(1);
        }
    }

    Shortcut {
        sequence: "Up"
        context: Qt.WindowShortcut
        enabled: !threadView.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) convList.moveSelection(-1);
        }
    }

    Shortcut {
        sequence: "Return"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) convList.activateCurrent();
        }
    }

    Shortcut {
        sequence: "i"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen && root.activeConversation !== "") {
                threadView.focusComposer();
            }
        }
    }

    Shortcut {
        sequence: "Ctrl+Return"
        context: Qt.WindowShortcut
        enabled: !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen && root.activeConversation !== "") {
                threadView.sendFromShortcut();
            }
        }
    }

    Shortcut {
        sequence: "/"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) {
                if (!root.twoColumns && root.activeConversation !== "") {
                    root.activeConversation = "";
                }
                convList.openFilter();
            }
        }
    }

    Shortcut {
        sequence: "?"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            root.helpOpen = !root.helpOpen;
        }
    }

    Shortcut {
        sequence: "n"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) root.newChatOpen = true;
        }
    }

    Shortcut {
        sequence: "a"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) convList.toggleFolder();
        }
    }

    Shortcut {
        sequence: "e"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (root.ready && !root.helpOpen) convList.archiveCurrent();
        }
    }

    Shortcut {
        sequences: ["Delete", "#"]
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            if (!root.ready || root.helpOpen) return;
            if (!threadView.deleteHovered()) convList.deleteCurrent();
        }
    }

    Shortcut {
        sequence: "q"
        context: Qt.WindowShortcut
        enabled: !root.editing && !root.modal
        onActivated: {
            root.visible = false;
        }
    }

    Shortcut {
        sequence: "Escape"
        context: Qt.WindowShortcut
        enabled: !root.modal
        onActivated: {
            if (root.menuOpen) {
                root.menuOpen = false;
                return;
            }
            if (root.helpOpen) {
                root.helpOpen = false;
                return;
            }
            if (convList.filterActive || convList.filterText !== "") {
                convList.clearFilter();
                return;
            }
            if (root.activeConversation !== "") {
                root.activeConversation = "";
                root.activeConversationName = "";
                convList.selectedId = "";
                return;
            }
            root.visible = false;
        }
    }

    Rectangle {
        anchors.fill: parent
        color: Commons.Color.background

        ColumnLayout {
            anchors.fill: parent
            spacing: 0

            // Menu bar: outside both views, so it works in every state.
            RowLayout {
                Layout.fillWidth: true
                Layout.leftMargin: appTheme.padX
                Layout.rightMargin: appTheme.padX
                Layout.preferredHeight: appTheme.ctl

                Text {
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    elide: Text.ElideRight
                    text: root.note
                    color: root.noteError ? Commons.Color.urgent : Commons.Color.muted
                    font.family: appTheme.family
                    font.pixelSize: appTheme.caption
                }

                Rectangle {
                    implicitWidth: appTheme.ctl - Style.space(6)
                    implicitHeight: appTheme.ctl - Style.space(6)
                    radius: appTheme.radius
                    color: "transparent"

                    Text {
                        anchors.centerIn: parent
                        textFormat: Text.PlainText
                        text: "\u22EE"
                        color: Commons.Color.foreground
                        font.family: appTheme.family
                        font.pixelSize: appTheme.subtitle
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.menuOpen = !root.menuOpen
                    }
                }
            }

            Item {
                Layout.fillWidth: true
                Layout.fillHeight: true

                PairView {
                    id: pairView
                    anchors.fill: parent
                    visible: !root.ready
                    client: appClient
                    theme: appTheme
                }

                RowLayout {
                    anchors.fill: parent
                    visible: root.ready
                    spacing: 0

                    ConversationList {
                        id: convList
                        visible: root.twoColumns || root.activeConversation === ""
                        Layout.preferredWidth: root.twoColumns ? Math.min(Style.space(320), Math.round(root.width * 0.36)) : root.width
                        Layout.fillWidth: !root.twoColumns
                        Layout.fillHeight: true
                        client: appClient
                        theme: appTheme

                        onNewChatRequested: root.newChatOpen = true
                        onConfirmRequested: function(title, body, label, action) { root.ask(title, body, label, action); }
                        onConversationHidden: function(id) {
                            if (id === root.activeConversation) {
                                root.activeConversation = "";
                                root.activeConversationName = "";
                            }
                        }
                        onConversationRemoved: function(id) {
                            if (id === root.activeConversation) {
                                root.activeConversation = "";
                                root.activeConversationName = "";
                            }
                        }

                        onConversationSelected: function(id, name) {
                            root.activeConversation = id;
                            root.activeConversationName = name;
                        }
                    }

                    Rectangle {
                        visible: root.twoColumns
                        Layout.fillHeight: true
                        width: 1
                        color: Commons.Color.popups.border
                    }

                    Thread {
                        id: threadView
                        visible: root.twoColumns || root.activeConversation !== ""
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        client: appClient
                        theme: appTheme
                        compact: !root.twoColumns
                        modal: root.modal
                        focused: root.visible && root.windowActive
                        conversationId: root.activeConversation
                        conversationName: root.activeConversationName
                        conversationAvatar: convList.avatars[root.activeConversation] || ""
                        conversationColor: convList.selectedItem ? String(convList.selectedItem.color || "") : ""
                        conversationGroup: convList.selectedItem ? convList.selectedItem.group === true : false

                        onBackRequested: {
                            root.activeConversation = "";
                            root.activeConversationName = "";
                            convList.selectedId = "";
                        }

                        onConfirmRequested: function(title, body, label, action) { root.ask(title, body, label, action); }

                        onConversationRead: function(id) {
                            convList.markReadLocal(id);
                        }
                    }
                }
            }
        }

        // Unpair / remove-all-data progress, or an uninstall that failed and can be retried.
        Rectangle {
            anchors.fill: parent
            visible: root.lifecycle !== "" || root.uninstallFailed
            color: Commons.Color.background

            MouseArea { anchors.fill: parent }

            ColumnLayout {
                anchors.centerIn: parent
                width: Math.min(parent.width - appTheme.padX * 2, Style.space(420))
                spacing: appTheme.gap

                Text {
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    horizontalAlignment: Text.AlignHCenter
                    text: {
                        if (root.lifecycle === "unpairing") return I18n.t("gmessages.unpairing", "Unpairing…");
                        if (root.lifecycle === "removing") return I18n.t("gmessages.removing", "Removing all data…");
                        if (root.lifecycle === "removed") return I18n.t("gmessages.removed", "All data removed. Messages starts again at your next login.");
                        return Errors.format(I18n, "uninstall_failed", "Could not remove all data. Try again.");
                    }
                    color: root.lifecycle === "" ? Commons.Color.urgent : Commons.Color.foreground
                    font.family: appTheme.family
                    font.pixelSize: appTheme.body
                }

                Text {
                    visible: root.lifecycle === "" && root.note !== ""
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    wrapMode: Text.Wrap
                    horizontalAlignment: Text.AlignHCenter
                    text: root.note
                    color: Commons.Color.urgent
                    font.family: appTheme.family
                    font.pixelSize: appTheme.caption
                }

                Rectangle {
                    visible: root.lifecycle === "" && root.uninstallFailed
                    Layout.alignment: Qt.AlignHCenter
                    implicitWidth: retryText.implicitWidth + Style.space(24)
                    implicitHeight: retryText.implicitHeight + Style.space(10)
                    radius: appTheme.radius
                    color: Commons.Color.accent

                    Text {
                        id: retryText
                        anchors.centerIn: parent
                        textFormat: Text.PlainText
                        text: I18n.t("gmessages.retry", "Retry")
                        color: Commons.Color.background
                        font.family: appTheme.family
                        font.pixelSize: appTheme.body
                        font.bold: true
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.lifecycleRequest("uninstall", "removing")
                    }
                }
            }
        }

        // App menu
        MouseArea {
            anchors.fill: parent
            visible: root.menuOpen
            onClicked: root.menuOpen = false
        }

        Rectangle {
            visible: root.menuOpen
            anchors.top: parent.top
            anchors.right: parent.right
            anchors.topMargin: appTheme.ctl
            anchors.rightMargin: appTheme.padX
            width: menuCol.implicitWidth + Style.space(16)
            height: menuCol.implicitHeight + Style.space(8)
            radius: appTheme.radius
            color: Commons.Color.popups.background
            border.width: 1
            border.color: Commons.Color.popups.border

            Column {
                id: menuCol
                anchors.centerIn: parent
                spacing: Style.space(2)

                Repeater {
                    model: [
                        { act: "unpair", label: I18n.t("gmessages.unpair", "Unpair phone"), show: appClient.state === "ready" || appClient.state === "connecting" || appClient.state === "expired" },
                        { act: "uninstall", label: I18n.t("gmessages.uninstall", "Remove all data"), show: true }
                    ]

                    Rectangle {
                        id: menuItem
                        required property var modelData
                        visible: menuItem.modelData.show
                        width: Math.max(Style.space(160), itemText.implicitWidth + Style.space(16))
                        height: menuItem.modelData.show ? itemText.implicitHeight + Style.space(10) : 0
                        radius: appTheme.radius
                        color: itemMouse.containsMouse ? Util.alpha(Commons.Color.foreground, 0.08) : "transparent"

                        Text {
                            id: itemText
                            anchors.verticalCenter: parent.verticalCenter
                            anchors.left: parent.left
                            anchors.leftMargin: Style.space(8)
                            textFormat: Text.PlainText
                            text: menuItem.modelData.label
                            color: menuItem.modelData.act === "uninstall" ? Commons.Color.urgent : Commons.Color.foreground
                            font.family: appTheme.family
                            font.pixelSize: appTheme.bodySmall
                        }

                        MouseArea {
                            id: itemMouse
                            anchors.fill: parent
                            hoverEnabled: true
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                if (menuItem.modelData.act === "unpair") root.askUnpair();
                                else root.askUninstall();
                            }
                        }
                    }
                }
            }
        }

        NewChat {
            anchors.fill: parent
            client: appClient
            theme: appTheme
            open: root.newChatOpen
            onCloseRequested: root.newChatOpen = false
            onStarted: function(conv) {
                root.newChatOpen = false;
                convList.clearFilter();
                convList.setFolder("inbox");
                convList.selectConversation(conv, "");
            }
        }

        Confirm {
            anchors.fill: parent
            theme: appTheme
            open: root.confirmOpen
            title: root.confirmTitle
            body: root.confirmBody
            actionLabel: root.confirmLabel
            onCancelled: root.closeConfirm()
            onAccepted: {
                var act = root.confirmAction;
                root.closeConfirm();
                if (act) act();
            }
        }

        HelpOverlay {
            id: helpOverlay
            anchors.fill: parent
            theme: appTheme
            open: root.helpOpen
            onCloseRequested: root.helpOpen = false
        }
    }
}
