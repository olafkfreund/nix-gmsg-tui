pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Errors.js" as Errors

Item {
    id: root

    required property var client
    required property var theme
    property string conversationId: ""
    property string conversationName: ""
    property string conversationAvatar: ""
    property string conversationColor: ""
    property bool conversationGroup: false
    property bool compact: false
    // Set by App: the window is visible and active, so arriving messages are being read.
    property bool focused: false
    readonly property bool editing: composer.editing
    // A message came in while the window was not focused: mark it read on return.
    property bool readOwed: false

    onFocusedChanged: {
        if (root.focused && root.readOwed && root.conversationId !== "") {
            root.readOwed = false;
            var conv = root.conversationId;
            root.client.send("read", { conversation: conv }, function() {
                root.conversationRead(conv);
            });
        }
    }

    signal backRequested()
    signal conversationRead(string id)
    signal confirmRequested(string title, string body, string actionLabel, var action)

    // The message under the pointer, for the Delete key.
    property string hoveredMessage: ""
    // A modal layer is open: the composer must not take keys.
    property bool modal: false

    // Messages that can be reacted to or deleted (the one rule for the buttons and the Delete key).
    function reactable(m) {
        return m.deleted !== true && m.failed !== true && m.pending !== true;
    }

    property var messages: []
    property bool truncated: false
    property bool loading: false
    property string typingPeer: ""

    onConversationIdChanged: {
        root.hoveredMessage = "";
        root.typingPeer = "";
        root.readOwed = false;
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
            if (root.ui["r:" + m.id] !== undefined) root.setUi("r:" + m.id, null);
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

    // State that outlives delegates (mergeMessages replaces the array on every event):
    // "m:<message>:<index>" -> {busy, err, saved}, "r:<message>" -> {busy} while a react is in flight.
    property var ui: ({})

    function setUi(key, patch) {
        var next = Object.assign({}, root.ui);
        if (patch === null) delete next[key];
        else next[key] = Object.assign({}, next[key] || ({}), patch);
        root.ui = next;
    }

    Component {
        id: savedClear
        Timer {
            property string key: ""
            interval: 4000
            running: true
            onTriggered: {
                root.setUi(key, { saved: "" });
                destroy();
            }
        }
    }

    function mediaRequest(method, message, index) {
        var key = "m:" + message + ":" + index;
        if ((root.ui[key] || ({})).busy) return;
        root.setUi(key, { busy: true, err: "", saved: "" });
        root.client.send(method, { message: message, index: index }, function(r) {
            if (!r.ok || !r.data) {
                root.setUi(key, { busy: false, err: Errors.formatError(I18n, r.error) });
                return;
            }
            root.setUi(key, { busy: false });
            var path = String(r.data.path || "");
            if (method === "media.open") {
                Qt.openUrlExternally("file://" + path);
            } else {
                root.setUi(key, { saved: path.substring(path.lastIndexOf("/") + 1) });
                savedClear.createObject(root, { key: key });
            }
        });
    }

    function react(msg, emoji) {
        var rk = "r:" + msg.id;
        if ((root.ui[rk] || ({})).busy) return;
        var list = Array.isArray(msg.reactions) ? msg.reactions : [];
        var same = false, any = false;
        for (var i = 0; i < list.length; i++) {
            if (list[i].mine) {
                any = true;
                if (list[i].emoji === emoji) same = true;
            }
        }
        var op = same ? "remove" : (any ? "replace" : "add");
        root.setUi(rk, { busy: true });
        root.client.send("react", { conversation: root.conversationId, message: msg.id, emoji: emoji, op: op }, function(r) {
            root.setUi(rk, null);
            if (!r.ok) {
                composer.statusNote = Errors.formatError(I18n, r.error);
            }
        });
    }

    function removeMessage(id) {
        var next = [];
        for (var i = 0; i < root.messages.length; i++) {
            if (root.messages[i].id !== id) next.push(root.messages[i]);
        }
        if (next.length !== root.messages.length) root.messages = next;
    }

    function askDeleteMessage(id) {
        var conv = root.conversationId;
        if (id === "" || conv === "" || (root.ui["d:" + id] || ({})).busy) return;
        root.confirmRequested(
            I18n.t("gmessages.delete_message_title", "Delete message?"),
            I18n.t("gmessages.delete_message_body", "This deletes the message for good."),
            I18n.t("gmessages.delete", "Delete"),
            function() {
                var dk = "d:" + id;
                root.setUi(dk, { busy: true });
                root.client.send("delete.message", { conversation: conv, message: id }, function(r) {
                    root.setUi(dk, null);
                    if (r.ok) {
                        if (root.conversationId === conv) root.removeMessage(id);
                    } else {
                        composer.statusNote = Errors.formatError(I18n, r.error);
                    }
                });
            });
    }

    // True when a message was under the pointer (the key is then handled here).
    function deleteHovered() {
        if (root.hoveredMessage === "" || !root.visible || root.conversationId === "") return false;
        for (var i = 0; i < root.messages.length; i++) {
            if (root.messages[i].id === root.hoveredMessage) {
                // Swallow the key: falling through would offer to delete the whole chat.
                if (!root.reactable(root.messages[i])) return true;
                root.askDeleteMessage(root.hoveredMessage);
                return true;
            }
        }
        return false;
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
            if (!msg.from_me && !msg.pending) {
                if (root.focused) {
                    root.client.send("read", { conversation: conv }, function() {
                        root.conversationRead(conv);
                    });
                } else {
                    root.readOwed = true;
                }
            }
        }

        function onGone(conversation, message) {
            if (conversation === root.conversationId && message !== "") root.removeMessage(message);
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

        function onResynced() {
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
            Layout.preferredHeight: Math.max(root.theme.ctl + root.theme.padY, Style.space(40) + root.theme.padY)
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

                Avatar {
                    Layout.alignment: Qt.AlignVCenter
                    theme: root.theme
                    size: Style.space(40)
                    path: root.conversationAvatar
                    color: root.conversationColor
                    name: root.conversationName
                    group: root.conversationGroup
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
                    height: rowCol.implicitHeight

                    readonly property bool fromMe: modelData.from_me === true
                    readonly property bool isDeleted: modelData.deleted === true
                    readonly property bool isFailed: modelData.failed === true
                    readonly property bool isPending: modelData.pending === true
                    readonly property bool reactable: root.reactable(modelData)
                    readonly property bool reactBusy: (root.ui["r:" + modelData.id] || ({})).busy === true
                    readonly property var reactions: Array.isArray(modelData.reactions) ? modelData.reactions : []

                    HoverHandler {
                        id: rowHover
                        onHoveredChanged: {
                            if (hovered) root.hoveredMessage = String(bubbleRow.modelData.id);
                            else if (root.hoveredMessage === String(bubbleRow.modelData.id)) root.hoveredMessage = "";
                        }
                    }

                    // The picker overlay would cover the Open/Save rows, so it hides while one is hovered.
                    property bool attHover: false

                    Column {
                        id: rowCol
                        width: parent.width
                        spacing: Style.space(4)

                        Item {
                            width: parent.width
                            height: bubbleRect.implicitHeight

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

                                ColumnLayout {
                                    id: att
                                    required property int index
                                    required property var modelData
                                    Layout.fillWidth: true
                                    spacing: Style.space(2)

                                    readonly property string key: "m:" + String(bubbleRow.modelData.id) + ":" + att.index
                                    readonly property var st: root.ui[att.key] || ({})

                                    HoverHandler { onHoveredChanged: bubbleRow.attHover = hovered }

                                    RowLayout {
                                        Layout.fillWidth: true
                                        spacing: Style.space(6)

                                        Text {
                                            Layout.fillWidth: true
                                            textFormat: Text.PlainText
                                            elide: Text.ElideRight
                                            text: "\u{F0066} " + String(att.modelData.name || att.modelData.mime || I18n.t("gmessages.attachment", "Attachment"))
                                            color: bubbleRect.textColor
                                            opacity: 0.85
                                            font.family: root.theme.family
                                            font.pixelSize: root.theme.bodySmall
                                        }

                                        Repeater {
                                            model: [
                                                { method: "media.open", icon: "\u{F03CC}", label: I18n.t("gmessages.open", "Open") },
                                                { method: "media.save", icon: "\u{F0193}", label: I18n.t("gmessages.save", "Save") }
                                            ]

                                            Rectangle {
                                                id: attBtn
                                                required property var modelData
                                                implicitWidth: btnText.implicitWidth + Style.space(10)
                                                implicitHeight: btnText.implicitHeight + Style.space(4)
                                                radius: Style.cornerRadius
                                                color: "transparent"
                                                border.width: 1
                                                border.color: bubbleRect.textColor
                                                opacity: att.st.busy ? 0.4 : 0.85

                                                Text {
                                                    id: btnText
                                                    anchors.centerIn: parent
                                                    textFormat: Text.PlainText
                                                    text: attBtn.modelData.icon + " " + attBtn.modelData.label
                                                    color: bubbleRect.textColor
                                                    font.family: root.theme.family
                                                    font.pixelSize: root.theme.caption
                                                }

                                                MouseArea {
                                                    anchors.fill: parent
                                                    enabled: !att.st.busy
                                                    cursorShape: Qt.PointingHandCursor
                                                    onClicked: root.mediaRequest(attBtn.modelData.method, String(bubbleRow.modelData.id), att.index)
                                                }
                                            }
                                        }
                                    }

                                    Text {
                                        visible: !!att.st.err
                                        Layout.fillWidth: true
                                        textFormat: Text.PlainText
                                        wrapMode: Text.Wrap
                                        text: String(att.st.err || "")
                                        color: Commons.Color.urgent
                                        font.family: root.theme.family
                                        font.pixelSize: root.theme.caption
                                    }

                                    Text {
                                        visible: !!att.st.saved
                                        Layout.fillWidth: true
                                        textFormat: Text.PlainText
                                        elide: Text.ElideMiddle
                                        text: I18n.t("gmessages.saved_to", "Saved to") + " " + String(att.st.saved || "")
                                        color: bubbleRect.textColor
                                        opacity: 0.85
                                        font.family: root.theme.family
                                        font.pixelSize: root.theme.caption
                                    }
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

                        // Reaction chips; only mine take clicks (they remove).
                        Flow {
                            visible: bubbleRow.reactions.length > 0
                            width: parent.width
                            spacing: Style.space(4)
                            layoutDirection: bubbleRow.fromMe ? Qt.RightToLeft : Qt.LeftToRight

                            Repeater {
                                model: bubbleRow.reactions

                                Rectangle {
                                    id: chip
                                    required property var modelData
                                    width: chipText.implicitWidth + Style.space(12)
                                    height: chipText.implicitHeight + Style.space(4)
                                    radius: height / 2
                                    color: Commons.Color.popups.background
                                    border.width: chip.modelData.mine ? 2 : 1
                                    border.color: chip.modelData.mine ? Commons.Color.foreground : Commons.Color.popups.border
                                    opacity: bubbleRow.reactBusy ? 0.5 : 1.0

                                    Text {
                                        id: chipText
                                        anchors.centerIn: parent
                                        textFormat: Text.PlainText
                                        text: String(chip.modelData.emoji) + (chip.modelData.count > 1 ? " " + chip.modelData.count : "")
                                        color: Commons.Color.foreground
                                        font.family: root.theme.family
                                        font.pixelSize: root.theme.bodySmall
                                    }

                                    MouseArea {
                                        anchors.fill: parent
                                        enabled: chip.modelData.mine === true && bubbleRow.reactable && !bubbleRow.reactBusy
                                        cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
                                        onClicked: root.react(bubbleRow.modelData, chip.modelData.emoji)
                                    }
                                }
                            }
                        }
                    }

                    // Picker: an overlay on the top edge of the bubble, outside the layout, so hovering never moves the list.
                    Item {
                        visible: bubbleRow.reactable && rowHover.hovered && !bubbleRow.attHover
                        z: 1
                        anchors.top: parent.top
                        width: parent.width
                        height: pickerRow.height

                        Row {
                            id: pickerRow
                            anchors.right: bubbleRow.fromMe ? parent.right : undefined
                            anchors.left: bubbleRow.fromMe ? undefined : parent.left
                            spacing: Style.space(4)
                            opacity: bubbleRow.reactBusy ? 0.5 : 1.0

                            Repeater {
                                model: ["\u{1F44D}", "\u2764\uFE0F", "\u{1F602}", "\u{1F62E}", "\u{1F622}", "\u{1F64F}"]

                                Rectangle {
                                    id: pick
                                    required property string modelData
                                    width: pickText.implicitWidth + Style.space(10)
                                    height: pickText.implicitHeight + Style.space(4)
                                    radius: height / 2
                                    color: Commons.Color.popups.background
                                    border.width: 1
                                    border.color: Commons.Color.popups.border

                                    Text {
                                        id: pickText
                                        anchors.centerIn: parent
                                        textFormat: Text.PlainText
                                        text: pick.modelData
                                        font.family: root.theme.family
                                        font.pixelSize: root.theme.body
                                    }

                                    MouseArea {
                                        anchors.fill: parent
                                        enabled: !bubbleRow.reactBusy
                                        cursorShape: Qt.PointingHandCursor
                                        onClicked: root.react(bubbleRow.modelData, pick.modelData)
                                    }
                                }
                            }

                            Rectangle {
                                width: delText.implicitWidth + Style.space(10)
                                height: delText.implicitHeight + Style.space(4)
                                radius: height / 2
                                color: Commons.Color.popups.background
                                border.width: 1
                                border.color: Commons.Color.urgent

                                Text {
                                    id: delText
                                    anchors.centerIn: parent
                                    textFormat: Text.PlainText
                                    text: I18n.t("gmessages.delete", "Delete")
                                    color: Commons.Color.urgent
                                    font.family: root.theme.family
                                    font.pixelSize: root.theme.caption
                                }

                                MouseArea {
                                    anchors.fill: parent
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: root.askDeleteMessage(String(bubbleRow.modelData.id))
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
            enabled: !root.modal
            visible: root.conversationId !== ""
            Layout.fillWidth: true
            client: root.client
            theme: root.theme
            conversationId: root.conversationId
        }
    }
}
