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
            property string collectedOut: ""
            stdout: StdioCollector {
                onStreamFinished: {
                    proc.collectedOut = String(text).trim();
                    if (proc.collectedOut === "open") {
                        root.openWindow(proc.targetConversation);
                    }
                }
            }
            onExited: {
                root.runningNotifications = Math.max(0, root.runningNotifications - 1);
                proc.destroy();
            }
        }
    }

    function spawnNotify(title, body, conv) {
        root.runningNotifications += 1;
        notifyProc.createObject(root, {
            command: [
                "@notifySend@",
                "-a", I18n.t("gmessages.app_name", "Messages"),
                "-A", "open=" + I18n.t("gmessages.action_open", "Open"),
                "--",
                title,
                body
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
