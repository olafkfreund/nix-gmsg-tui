pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import QtQuick.Dialogs
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
    property string replyToId: ""

    property bool sending: false
    readonly property bool editing: inputField.activeFocus
    property string statusNote: ""
    property string errorNote: ""

    implicitHeight: col.implicitHeight + root.theme.padY * 2

    // Drafts stay with their conversation, so text never moves to another recipient.
    property var drafts: ({})
    property string draftFor: ""

    onConversationIdChanged: {
        root.statusNote = "";
        root.errorNote = "";
        root.drafts[root.draftFor] = inputField.text;
        inputField.text = root.drafts[root.conversationId] || "";
        root.draftFor = root.conversationId;
    }

    function focusInput() {
        inputField.forceActiveFocus();
    }

    function hasFocus() {
        return inputField.activeFocus;
    }

    // Picked files stay with their conversation too: conversation id -> [{ key, path, name }].
    property var files: ({})
    property int nextKey: 1
    property string pickFor: ""
    readonly property var currentFiles: root.files[root.conversationId] || []
    readonly property bool canAttach: root.client.features.indexOf("attachments") >= 0
    readonly property bool canSend: !root.sending && root.conversationId !== ""
        && (String(inputField.text || "").trim() !== "" || root.currentFiles.length > 0)

    function setFiles(conv, list) {
        var f = Object.assign({}, root.files);
        f[conv] = list;
        root.files = f;
    }

    function addPicked(conv, urls) {
        var list = (root.files[conv] || []).slice();
        var dropped = false;
        for (var i = 0; i < urls.length; i++) {
            var u = String(urls[i]);
            var path;
            if (u.indexOf("file:///") === 0) path = u.slice(7);
            else if (u.indexOf("file://localhost/") === 0) path = u.slice(16);
            else { dropped = true; continue; }
            try { path = decodeURIComponent(path); } catch (e) { dropped = true; continue; }
            if (list.length >= 10) { dropped = true; continue; }
            list.push({ key: root.nextKey++, path: path, name: path.split("/").pop() });
        }
        root.setFiles(conv, list);
        if (dropped && conv === root.conversationId) {
            root.errorNote = I18n.t("gmessages.attach_dropped", "Some files were not added (10 at most, local files only).");
        }
    }

    function removeFile(conv, key) {
        root.setFiles(conv, (root.files[conv] || []).filter(f => f.key !== key));
    }

    function submit() {
        var conv = root.conversationId;
        var raw = String(inputField.text || "");
        var body = raw.trim();
        var reply = root.replyToId;
        var sent = root.currentFiles.slice();
        if (root.sending || conv === "" || (body === "" && sent.length === 0)) return;
        if (sent.length > 0 && !root.canAttach) {
            root.errorNote = I18n.t("gmessages.attach_unsupported", "The background service can't send files; update it.");
            return;
        }

        root.sending = true;
        root.statusNote = "";
        root.errorNote = "";
        var params = { conversation: conv, text: body };
        if (reply !== "") {
            params.reply_to = reply;
        }
        if (sent.length > 0) {
            params.attachments = sent.map(f => f.path);
        }

        root.client.send("send", params, function(r) {
            root.sending = false;
            var open = root.conversationId === conv;
            if (r.ok && r.data) {
                var keys = sent.map(f => f.key);
                root.setFiles(conv, (root.files[conv] || []).filter(f => keys.indexOf(f.key) < 0));
                if (open) {
                    if (inputField.text === raw) inputField.text = "";
                    if (root.replyToId === reply) root.replyToId = "";
                    root.statusNote = r.data.status === "accepted" ? I18n.t("gmessages.sending_pending", "sending…") : "";
                } else if (root.drafts[conv] === raw) {
                    delete root.drafts[conv];
                }
                return;
            }
            if (!open) return;
            var code = r.error && r.error.code ? String(r.error.code) : "internal";
            if (code === "send_uncertain" || code === "timeout" || code === "disconnected") {
                root.errorNote = I18n.t("gmessages.send_uncertain", "may not have been sent");
            } else {
                root.errorNote = Errors.formatError(I18n, r.error);
            }
        });
    }

    FileDialog {
        id: picker
        fileMode: FileDialog.OpenFiles
        onAccepted: root.addPicked(root.pickFor, selectedFiles)
    }

    ColumnLayout {
        id: col
        anchors.fill: parent
        anchors.leftMargin: root.theme.padX
        anchors.rightMargin: root.theme.padX
        anchors.topMargin: root.theme.padY
        anchors.bottomMargin: root.theme.padY
        spacing: Style.space(6)

        Text {
            visible: root.errorNote !== "" || root.statusNote !== ""
            Layout.fillWidth: true
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: root.errorNote !== "" ? root.errorNote : root.statusNote
            color: root.errorNote !== "" ? Commons.Color.urgent : Commons.Color.muted
            font.family: root.theme.family
            font.pixelSize: root.theme.caption
        }

        Flow {
            visible: root.currentFiles.length > 0
            Layout.fillWidth: true
            spacing: root.theme.gap

            Repeater {
                model: root.currentFiles

                Rectangle {
                    id: chip
                    required property var modelData
                    required property int index
                    implicitWidth: chipRow.implicitWidth + Style.space(12)
                    implicitHeight: chipRow.implicitHeight + Style.space(6)
                    radius: root.theme.radius
                    color: "transparent"
                    border.width: 1
                    border.color: Commons.Color.muted

                    RowLayout {
                        id: chipRow
                        anchors.centerIn: parent
                        spacing: Style.space(6)

                        Text {
                            Layout.maximumWidth: 180
                            textFormat: Text.PlainText
                            elide: Text.ElideMiddle
                            text: "\u{F0066} " + chip.modelData.name
                            color: Commons.Color.foreground
                            font.family: root.theme.family
                            font.pixelSize: root.theme.caption
                        }

                        Text {
                            textFormat: Text.PlainText
                            text: "\u2715"
                            color: Commons.Color.muted
                            font.family: root.theme.family
                            font.pixelSize: root.theme.caption

                            MouseArea {
                                anchors.fill: parent
                                cursorShape: Qt.PointingHandCursor
                                onClicked: root.removeFile(root.conversationId, chip.modelData.key)
                            }
                        }
                    }
                }
            }
        }

        RowLayout {
            Layout.fillWidth: true
            spacing: root.theme.gap

            Rectangle {
                visible: root.canAttach
                Layout.alignment: Qt.AlignBottom
                implicitWidth: attachLabel.implicitWidth + root.theme.padX * 2
                implicitHeight: root.theme.ctl
                radius: root.theme.radius
                color: "transparent"
                border.width: 1
                border.color: Commons.Color.muted
                opacity: (!root.sending && root.conversationId !== "" && root.currentFiles.length < 10) ? 1.0 : 0.5

                Text {
                    id: attachLabel
                    anchors.centerIn: parent
                    textFormat: Text.PlainText
                    text: "\u{F0066}"
                    color: Commons.Color.foreground
                    font.family: root.theme.family
                    font.pixelSize: root.theme.bodySmall
                }

                MouseArea {
                    anchors.fill: parent
                    enabled: !root.sending && root.conversationId !== "" && root.currentFiles.length < 10
                    cursorShape: Qt.PointingHandCursor
                    onClicked: {
                        root.pickFor = root.conversationId;
                        picker.open();
                    }
                }
            }

            TextField {
                id: inputField
                Layout.fillWidth: true
                // Not off while sending: a disabled field drops focus. submit() guards resends.
                enabled: root.conversationId !== ""
                placeholderText: I18n.t("gmessages.composer_placeholder", "Write a message… (Ctrl+Enter to send)")
                wrapMode: TextInput.Wrap

                Keys.onPressed: function(event) {
                    if ((event.key === Qt.Key_Return || event.key === Qt.Key_Enter) && (event.modifiers & Qt.ControlModifier)) {
                        root.submit();
                        event.accepted = true;
                    }
                }
            }

            Rectangle {
                Layout.alignment: Qt.AlignBottom
                implicitWidth: sendLabel.implicitWidth + root.theme.padX * 2
                implicitHeight: root.theme.ctl
                radius: root.theme.radius
                color: Commons.Color.accent
                opacity: root.canSend ? 1.0 : 0.5

                Text {
                    id: sendLabel
                    anchors.centerIn: parent
                    textFormat: Text.PlainText
                    text: root.sending
                        ? I18n.t("gmessages.sending_pending", "sending…")
                        : I18n.t("gmessages.send", "Send")
                    color: Commons.Color.background
                    font.family: root.theme.family
                    font.pixelSize: root.theme.bodySmall
                    font.bold: true
                }

                MouseArea {
                    anchors.fill: parent
                    enabled: root.canSend
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.submit()
                }
            }
        }
    }
}
