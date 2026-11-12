pragma ComponentBehavior: Bound
import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons

Item {
    id: root

    property var shell: null
    property var manifest: null

    property int runningNotifications: 0
    property int droppedNotifications: 0
    property string pendingOpenConversation: ""

    Client {
        id: client
        role: "service"

        onMessageArrived: function(conv, msg, notify) {
            if (!notify || !msg) return;
            if (root.runningNotifications >= 3) {
                root.droppedNotifications += 1;
                return;
            }
            var sender = String(msg.sender || I18n.t("gmessages.app_name", "Messages"));
            var body = String(msg.text || "");
            root.spawnNotify(sender, body, conv);
        }

        onMissed: function(count) {
            var total = (Number(count) || 0) + root.droppedNotifications;
            root.droppedNotifications = 0;
            if (total <= 0) return;
            var summary = I18n.tn("gmessages.missed_messages", total, total + " new messages").replace("%n", String(total));
            if (root.runningNotifications < 3) {
                root.spawnNotify(I18n.t("gmessages.app_name", "Messages"), summary, "");
            } else {
                root.droppedNotifications += total;
            }
        }
    }

    Component {
        id: notifyProc
        Process {
            id: proc
            property string targetConversation: ""
            // Destroyed only once both the exit and the action on stdout are in.
            property bool exitedDone: false
            property bool streamDone: false
            function finish() {
                if (!proc.exitedDone || !proc.streamDone) return;
                root.runningNotifications = Math.max(0, root.runningNotifications - 1);
                root.flushDropped();
                proc.destroy();
            }
            stdout: StdioCollector {
                onStreamFinished: {
                    if (String(text).trim() === "open") {
                        root.openWindow(proc.targetConversation);
                    }
                    proc.streamDone = true;
                    proc.finish();
                }
            }
            onExited: {
                proc.exitedDone = true;
                proc.finish();
                // A notify-send that never started may not finish its stream.
                if (!proc.streamDone) graceTimer.createObject(root, { target: proc });
            }
        }
    }

    Component {
        id: graceTimer
        Timer {
            property var target: null
            interval: 1000
            running: true
            onTriggered: {
                if (target && !target.streamDone) {
                    target.streamDone = true;
                    target.finish();
                }
                destroy();
            }
        }
    }

    // Notifications over the three in flight become one summary as soon as a slot frees.
    function flushDropped() {
        if (root.droppedNotifications <= 0 || root.runningNotifications >= 3) return;
        var total = root.droppedNotifications;
        root.droppedNotifications = 0;
        var summary = I18n.tn("gmessages.missed_messages", total, total + " new messages").replace("%n", String(total));
        root.spawnNotify(I18n.t("gmessages.app_name", "Messages"), summary, "");
    }

    // Notification servers render body markup; message text is shown as written.
    function plain(text) {
        return String(text).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
    }

    // shortcut: notify-send takes the text as arguments, so it is in /proc/<pid>/cmdline
    // while the notification is open; a D-Bus client would avoid it on multi-user hosts.
    function spawnNotify(title, body, conv) {
        root.runningNotifications += 1;
        notifyProc.createObject(root, {
            command: [
                "@notifySend@",
                "-a", I18n.t("gmessages.app_name", "Messages"),
                "-A", "open=" + I18n.t("gmessages.action_open", "Open"),
                "--",
                title,
                root.plain(body)
            ],
            targetConversation: conv || "",
            running: true
        });
    }

    Loader {
        id: appLoader
        active: false
        source: "App.qml"
        onLoaded: {
            if (item) {
                item.shell = root.shell;
                item.manifest = root.manifest;
                item.visible = true;
                if (root.pendingOpenConversation !== "") {
                    item.openConversation(root.pendingOpenConversation);
                    root.pendingOpenConversation = "";
                }
            }
        }
    }

    // A window closed by the compositor (SUPER+W) cannot be shown again; drop it so the next open loads a fresh one.
    Connections {
        target: appLoader.item
        function onClosed() {
            Qt.callLater(function() { appLoader.active = false; });
        }
    }

    function toggleWindow() {
        if (!appLoader.active) {
            appLoader.active = true;
            return "ok";
        }
        if (appLoader.item) {
            appLoader.item.visible = !appLoader.item.visible;
        }
        return "ok";
    }

    function openWindow(conversation) {
        var conv = String(conversation || "");
        if (!appLoader.active) {
            root.pendingOpenConversation = conv;
            appLoader.active = true;
            return "ok";
        }
        if (appLoader.item) {
            appLoader.item.visible = true;
            if (conv !== "") {
                appLoader.item.openConversation(conv);
            }
        }
        return "ok";
    }

    IpcHandler {
        target: "olafkfreund.gmessages"

        function toggle(): string {
            return root.toggleWindow();
        }

        function open(conversation: string): string {
            return root.openWindow(conversation);
        }
    }
}
