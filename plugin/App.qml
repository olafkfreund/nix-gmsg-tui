pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import QtQuick.Layouts
import Quickshell
import qs.Commons
import qs.Commons as Commons

FloatingWindow {
    id: root

    property var shell: null
    property var manifest: null

    title: I18n.t("gmessages.app_name", "Messages")
    appId: "olafkfreund.gmessages"
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

    // Plain keys belong to a text field that has focus, not to navigation.
    readonly property bool editing: threadView.editing || convList.editing
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
        var focusedConv = (root.visible && root.active && root.ready && root.activeConversation !== "")
            ? root.activeConversation
            : null;
        var key = focusedConv === null ? "__null__" : focusedConv;
        if (key === root.lastReportedFocus) return;
        root.lastReportedFocus = key;
        appClient.send("focus", { conversation: focusedConv });
    }

    onVisibleChanged: root.syncFocus()
    onActiveChanged: root.syncFocus()
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
    }

    // Shortcuts (application context)
    Shortcut {
        sequence: "j"
        context: Qt.WindowShortcut
        enabled: !root.editing
        onActivated: {
            if (root.ready && !root.helpOpen) convList.moveSelection(1);
        }
    }

    Shortcut {
        sequence: "k"
        context: Qt.WindowShortcut
        enabled: !root.editing
        onActivated: {
            if (root.ready && !root.helpOpen) convList.moveSelection(-1);
        }
    }

    Shortcut {
        sequence: "Down"
        context: Qt.WindowShortcut
        enabled: !threadView.editing
        onActivated: {
            if (root.ready && !root.helpOpen) convList.moveSelection(1);
        }
    }

    Shortcut {
        sequence: "Up"
        context: Qt.WindowShortcut
        enabled: !threadView.editing
        onActivated: {
            if (root.ready && !root.helpOpen) convList.moveSelection(-1);
        }
    }

    Shortcut {
        sequence: "Return"
        context: Qt.WindowShortcut
        enabled: !root.editing
        onActivated: {
            if (root.ready && !root.helpOpen) convList.activateCurrent();
        }
    }

    Shortcut {
        sequence: "i"
        context: Qt.WindowShortcut
        enabled: !root.editing
        onActivated: {
            if (root.ready && !root.helpOpen && root.activeConversation !== "") {
                threadView.focusComposer();
            }
        }
    }

    Shortcut {
        sequence: "Ctrl+Return"
        context: Qt.WindowShortcut
        onActivated: {
            if (root.ready && !root.helpOpen && root.activeConversation !== "") {
                threadView.sendFromShortcut();
            }
        }
    }

    Shortcut {
        sequence: "/"
        context: Qt.WindowShortcut
        enabled: !root.editing
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
        enabled: !root.editing
        onActivated: {
            root.helpOpen = !root.helpOpen;
        }
    }

    Shortcut {
        sequence: "q"
        context: Qt.WindowShortcut
        enabled: !root.editing
        onActivated: {
            root.visible = false;
        }
    }

    Shortcut {
        sequence: "Escape"
        context: Qt.WindowShortcut
        onActivated: {
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
                focused: root.visible && root.active
                conversationId: root.activeConversation
                conversationName: root.activeConversationName

                onBackRequested: {
                    root.activeConversation = "";
                    root.activeConversationName = "";
                    convList.selectedId = "";
                }

                onConversationRead: function(id) {
                    convList.markReadLocal(id);
                }
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
