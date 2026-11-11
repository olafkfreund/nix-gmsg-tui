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
    property string replyToId: ""

    property bool sending: false
    property string statusNote: ""
    property string errorNote: ""

    implicitHeight: col.implicitHeight + root.theme.padY * 2

    onConversationIdChanged: {
        root.statusNote = "";
        root.errorNote = "";
    }

    function focusInput() {
        inputField.forceActiveFocus();
    }

    function hasFocus() {
        return inputField.activeFocus;
    }

    function submit() {
        if (root.sending || root.conversationId === "") return;
        var body = String(inputField.text || "").trim();
        if (body === "") return;

        root.sending = true;
        root.statusNote = "";
        root.errorNote = "";
        var conv = root.conversationId;
        var params = { conversation: conv, text: body };
        if (root.replyToId !== "") {
            params.reply_to = root.replyToId;
        }

        root.client.send("send", params, function(r) {
            root.sending = false;
            if (r.ok && r.data) {
                if (root.conversationId === conv) {
                    inputField.text = "";
                    root.replyToId = "";
                }
                if (r.data.status === "accepted") {
                    root.statusNote = I18n.t("gmessages.sending_pending", "sending…");
                } else {
                    root.statusNote = "";
                }
                return;
            }
            var code = r.error && r.error.code ? String(r.error.code) : "internal";
            if (code === "send_uncertain") {
                root.errorNote = I18n.t("gmessages.send_uncertain", "may not have been sent");
            } else {
                root.errorNote = Errors.formatError(I18n, r.error);
            }
        });
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

        RowLayout {
            Layout.fillWidth: true
            spacing: root.theme.gap

            TextField {
                id: inputField
                Layout.fillWidth: true
                enabled: root.conversationId !== "" && !root.sending
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
                opacity: (root.sending || String(inputField.text || "").trim() === "") ? 0.5 : 1.0

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
                    enabled: !root.sending && root.conversationId !== ""
                    cursorShape: Qt.PointingHandCursor
                    onClicked: root.submit()
                }
            }
        }
    }
}
