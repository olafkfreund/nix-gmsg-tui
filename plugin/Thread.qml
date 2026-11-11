pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui

Item {
    id: root

    required property var client
    required property var theme
    property string conversationId: ""
    property string conversationName: ""
    property bool compact: false
    // Set by App: the window is visible and active, so arriving messages are being read.
    property bool focused: false

    signal backRequested()
    signal conversationRead(string id)

    property var messages: []
    property bool truncated: false
    property bool loading: false
    property string typingPeer: ""

    onConversationIdChanged: {
        root.typingPeer = "";
        if (root.conversationId === "") {
            root.messages = [];
            root.truncated = false;
            return;
        }
        root.openThread(root.conversationId);
    }

    function openThread(conv) {
        if (!conv || !root.client.connected) return;
        root.loading = true;
        root.messages = [];
        root.client.send("history", { conversation: conv }, function(r) {
            if (root.conversationId !== conv) return;
            root.loading = false;
            if (r.ok && r.data) {
                root.truncated = r.data.truncated === true;
                var list = Array.isArray(r.data.messages) ? r.data.messages : [];
                root.mergeMessages(list, true);
            }
        });
        root.client.send("read", { conversation: conv }, function() {
            root.conversationRead(conv);
        });
    }

    function mergeMessages(incoming, dropPending) {
        var base = [];
        for (var i = 0; i < root.messages.length; i++) {
            var cur = root.messages[i];
            if (dropPending && cur.pending) continue;
            base.push(cur);
        }
        var byId = ({});
        for (var j = 0; j < base.length; j++) {
            byId[base[j].id] = base[j];
        }
        for (var k = 0; k < incoming.length; k++) {
            var m = incoming[k];
            if (!m || !m.id) continue;
            byId[m.id] = m;
        }
        var merged = [];
        for (var id in byId) {
            merged.push(byId[id]);
        }
        merged.sort(function(a, b) {
            var dt = (Number(a.timestamp_ms) || 0) - (Number(b.timestamp_ms) || 0);
            if (dt !== 0) return dt;
            return String(a.id).localeCompare(String(b.id));
        });
        root.messages = merged;
        Qt.callLater(function() {
            if (msgList.count > 0) {
                msgList.positionViewAtEnd();
            }
        });
    }

    function focusComposer() {
        composer.focusInput();
    }

    function sendFromShortcut() {
        composer.submit();
    }

    function formatTimestamp(ms) {
        var n = Number(ms) || 0;
        if (n <= 0) return "";
        var dt = new Date(n);
        var loc = Qt.locale(I18n.formatLocale || "");
        return dt.toLocaleTimeString(loc, Locale.ShortFormat);
    }

    Connections {
        target: root.client

        function onHistoryArrived(conv, msgs) {
            if (conv !== root.conversationId) return;
            root.mergeMessages(Array.isArray(msgs) ? msgs : [], true);
            composer.statusNote = "";
        }

        function onMessageArrived(conv, msg, notify) {
            if (conv !== root.conversationId || !msg) return;
            root.mergeMessages([msg], false);
            if (root.focused && !msg.from_me && !msg.pending) {
                root.client.send("read", { conversation: conv }, function() {
                    root.conversationRead(conv);
                });
            }
        }

        function onTyping(conversation, name, started) {
            if (conversation !== root.conversationId) return;
            root.typingPeer = started ? String(name || I18n.t("gmessages.someone", "Someone")) : "";
        }

        function onStateChanged() {
            if (root.client.state === "ready" && root.conversationId !== "") {
                root.openThread(root.conversationId);
            }
        }
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Empty state when no conversation is selected
        Item {
            visible: root.conversationId === ""
            Layout.fillWidth: true
            Layout.fillHeight: true

            Text {
                anchors.centerIn: parent
                textFormat: Text.PlainText
                text: I18n.t("gmessages.select_conversation", "Select a conversation")
                color: Commons.Color.muted
                font.family: root.theme.family
                font.pixelSize: root.theme.body
            }
        }

        // Thread header
        Rectangle {
            visible: root.conversationId !== ""
            Layout.fillWidth: true
            Layout.preferredHeight: root.theme.ctl + root.theme.padY
            color: "transparent"

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: root.theme.padX
                anchors.rightMargin: root.theme.padX
                spacing: root.theme.gap

                Rectangle {
                    visible: root.compact
                    implicitWidth: root.theme.ctl - Style.space(6)
                    implicitHeight: root.theme.ctl - Style.space(6)
                    radius: root.theme.radius
                    color: "transparent"

                    Text {
                        anchors.centerIn: parent
                        textFormat: Text.PlainText
                        text: "\u{F004D}" // nf-md-arrow_left
                        color: Commons.Color.foreground
                        font.family: root.theme.family
                        font.pixelSize: root.theme.body
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.backRequested()
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    spacing: Style.space(2)

                    Text {
                        Layout.fillWidth: true
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        text: root.conversationName !== ""
                            ? root.conversationName
                            : I18n.t("gmessages.conversation", "Conversation")
                        color: Commons.Color.foreground
                        font.family: root.theme.family
                        font.pixelSize: root.theme.subtitle
                        font.bold: true
                    }

                    Text {
                        visible: root.typingPeer !== ""
                        Layout.fillWidth: true
                        textFormat: Text.PlainText
                        elide: Text.ElideRight
                        text: root.typingPeer + " " + I18n.t("gmessages.is_typing", "is typing…")
                        color: Commons.Color.muted
                        font.family: root.theme.family
                        font.pixelSize: root.theme.caption
                    }
                }
            }
        }

        Rectangle {
            visible: root.conversationId !== ""
            Layout.fillWidth: true
            height: 1
            color: Commons.Color.popups.border
        }

        // Message list
        Item {
            visible: root.conversationId !== ""
            Layout.fillWidth: true
            Layout.fillHeight: true

            ListView {
                id: msgList
                anchors.fill: parent
                anchors.leftMargin: root.theme.padX
                anchors.rightMargin: root.theme.padX
                anchors.topMargin: root.theme.padY
                anchors.bottomMargin: root.theme.padY
                clip: true
                spacing: Style.space(8)
                model: root.messages

                delegate: Item {
                    id: bubbleRow
                    required property var modelData

                    width: msgList.width
                    height: bubbleRect.implicitHeight

                    readonly property bool fromMe: modelData.from_me === true
                    readonly property bool isDeleted: modelData.deleted === true
                    readonly property bool isFailed: modelData.failed === true
                    readonly property bool isPending: modelData.pending === true

                    Rectangle {
                        id: bubbleRect
                        anchors.right: bubbleRow.fromMe ? parent.right : undefined
                        anchors.left: bubbleRow.fromMe ? undefined : parent.left
                        width: Math.min(parent.width * 0.78, Math.max(Style.space(96), bubbleContent.implicitWidth + root.theme.padX * 2))
                        implicitHeight: bubbleContent.implicitHeight + root.theme.padY * 2
                        radius: Style.cornerRadius

                        color: {
                            if (bubbleRow.isDeleted) return "transparent";
                            if (bubbleRow.isFailed) return Util.alpha(Commons.Color.urgent, 0.16);
                            if (bubbleRow.fromMe) return Util.alpha(Commons.Color.accent, bubbleRow.isPending ? 0.55 : 0.9);
                            return Commons.Color.popups.background;
                        }
                        border.width: (bubbleRow.isDeleted || bubbleRow.isFailed || !bubbleRow.fromMe) ? 1 : 0
                        border.color: {
                            if (bubbleRow.isFailed) return Commons.Color.urgent;
                            return Commons.Color.popups.border;
                        }
                        opacity: bubbleRow.isDeleted ? 0.65 : (bubbleRow.isPending ? 0.8 : 1.0)

                        readonly property color textColor: {
                            if (bubbleRow.isDeleted) return Commons.Color.muted;
                            if (bubbleRow.isFailed) return Commons.Color.foreground;
                            if (bubbleRow.fromMe) return Commons.Color.background;
                            return Commons.Color.foreground;
                        }

                        ColumnLayout {
                            id: bubbleContent
                            anchors.fill: parent
                            anchors.leftMargin: root.theme.padX
                            anchors.rightMargin: root.theme.padX
                            anchors.topMargin: root.theme.padY
                            anchors.bottomMargin: root.theme.padY
                            spacing: Style.space(4)

                            Text {
                                visible: !bubbleRow.fromMe && String(bubbleRow.modelData.sender || "") !== ""
                                Layout.fillWidth: true
                                textFormat: Text.PlainText
                                elide: Text.ElideRight
                                text: String(bubbleRow.modelData.sender || "")
                                color: bubbleRect.textColor
                                opacity: 0.85
                                font.family: root.theme.family
                                font.pixelSize: root.theme.caption
                                font.bold: true
                            }

                            Text {
                                Layout.fillWidth: true
                                textFormat: Text.PlainText
                                wrapMode: Text.Wrap
                                text: bubbleRow.isDeleted
                                    ? I18n.t("gmessages.message_deleted", "Message deleted")
                                    : String(bubbleRow.modelData.text || "")
                                color: bubbleRect.textColor
                                font.family: root.theme.family
                                font.pixelSize: root.theme.body
                                font.italic: bubbleRow.isDeleted
                            }

                            Repeater {
                                model: Array.isArray(bubbleRow.modelData.attachments) ? bubbleRow.modelData.attachments : []

                                Text {
                                    required property var modelData
                                    Layout.fillWidth: true
                                    textFormat: Text.PlainText
                                    elide: Text.ElideRight
                                    text: "\u{F0066} " + String(modelData.name || modelData.mime || I18n.t("gmessages.attachment", "Attachment"))
                                    color: bubbleRect.textColor
                                    opacity: 0.85
                                    font.family: root.theme.family
                                    font.pixelSize: root.theme.bodySmall
                                }
                            }

                            RowLayout {
                                Layout.fillWidth: true
                                spacing: Style.space(6)

                                Item { Layout.fillWidth: true }

                                Text {
                                    visible: bubbleRow.isPending
                                    textFormat: Text.PlainText
                                    text: I18n.t("gmessages.sending_pending", "sending…")
                                    color: bubbleRect.textColor
                                    opacity: 0.85
                                    font.family: root.theme.family
                                    font.pixelSize: root.theme.caption
                                    font.italic: true
                                }

                                Text {
                                    visible: bubbleRow.isFailed
                                    textFormat: Text.PlainText
                                    text: I18n.t("gmessages.message_failed", "Failed to send")
                                    color: Commons.Color.urgent
                                    font.family: root.theme.family
                                    font.pixelSize: root.theme.caption
                                    font.bold: true
                                }

                                Text {
                                    visible: !bubbleRow.isPending
                                    textFormat: Text.PlainText
                                    text: root.formatTimestamp(bubbleRow.modelData.timestamp_ms)
                                    color: bubbleRect.textColor
                                    opacity: 0.75
                                    font.family: root.theme.family
                                    font.pixelSize: root.theme.caption
                                }
                            }
                        }
                    }
                }
            }

            Text {
                anchors.centerIn: parent
                visible: root.messages.length === 0
                textFormat: Text.PlainText
                text: root.loading
                    ? I18n.t("gmessages.loading_messages", "Loading messages…")
                    : I18n.t("gmessages.no_messages", "No messages yet")
                color: Commons.Color.muted
                font.family: root.theme.family
                font.pixelSize: root.theme.body
            }
        }

        Rectangle {
            visible: root.conversationId !== ""
            Layout.fillWidth: true
            height: 1
            color: Commons.Color.popups.border
        }

        Composer {
            id: composer
            visible: root.conversationId !== ""
            Layout.fillWidth: true
            client: root.client
            theme: root.theme
            conversationId: root.conversationId
        }
    }
}
