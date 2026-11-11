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

    property string emoji: ""
    property string qrSvg: ""
    property string manualUrl: ""
    property string pairMessage: ""
    property string actionError: ""
    property bool busy: false

    Connections {
        target: root.client
        function onPairing(data) {
            root.busy = false;
            if (!data) return;
            if (data.emoji !== undefined) root.emoji = String(data.emoji || "");
            if (data.qr_svg !== undefined) root.qrSvg = String(data.qr_svg || "");
            if (data.manual_url !== undefined) root.manualUrl = String(data.manual_url || "");
            if (data.message !== undefined) root.pairMessage = String(data.message || "");
        }
        function onStateChanged() {
            if (root.client.state === "login" || root.client.state === "ready") {
                root.busy = false;
            }
            if (root.client.state !== "pairing") {
                root.emoji = "";
                root.qrSvg = "";
                root.manualUrl = "";
            }
        }
    }

    function startPairing(method) {
        root.actionError = "";
        root.pairMessage = "";
        root.emoji = "";
        root.qrSvg = "";
        root.manualUrl = "";
        root.busy = true;
        root.client.send("pair.start", { method: method }, function(r) {
            root.busy = false;
            if (!r.ok) {
                root.actionError = Errors.formatError(I18n, r.error);
            }
        });
    }

    function cancelPairing() {
        root.actionError = "";
        root.client.send("pair.cancel", {}, function(r) {
            root.busy = false;
            if (!r.ok) {
                root.actionError = Errors.formatError(I18n, r.error);
            }
        });
    }

    function retryKey() {
        root.actionError = "";
        root.busy = true;
        root.client.send("key.retry", {}, function(r) {
            root.busy = false;
            if (!r.ok) {
                root.actionError = Errors.formatError(I18n, r.error);
            }
        });
    }

    readonly property bool isLockedState: root.client.state === "locked" || root.client.state === "keyring_missing"
    readonly property bool canStartPair: root.client.connected && !root.client.mismatch && (root.client.state === "login" || root.client.state === "expired" || root.client.state === "pairing")

    readonly property string stateText: {
        if (!root.client.connected) {
            return Errors.format(I18n, "disconnected", "Disconnected from the background service.");
        }
        if (root.client.mismatch) {
            return Errors.format(I18n, "protocol_mismatch", "Daemon protocol mismatch. Please restart the service.");
        }
        if (root.client.state === "locked") {
            return I18n.t("gmessages.unlock_keyring", "Unlock your keyring");
        }
        var base = Errors.format(I18n, root.client.state, root.client.state);
        if (root.client.detail) {
            let det = Errors.format(I18n, String(root.client.detail), String(root.client.detail));
            if (det && det !== base) return base + " (" + det + ")";
        }
        return base;
    }

    ColumnLayout {
        anchors.centerIn: parent
        width: Math.min(parent.width - root.theme.padX * 2, Style.space(460))
        spacing: root.theme.section

        Text {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: I18n.t("gmessages.app_name", "Messages")
            color: Commons.Color.foreground
            font.family: root.theme.family
            font.pixelSize: root.theme.title
            font.bold: true
        }

        Text {
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: root.stateText
            color: Commons.Color.foreground
            opacity: root.theme.dim
            font.family: root.theme.family
            font.pixelSize: root.theme.body
        }

        Text {
            visible: root.pairMessage !== "" || root.actionError !== ""
            Layout.fillWidth: true
            horizontalAlignment: Text.AlignHCenter
            textFormat: Text.PlainText
            wrapMode: Text.WordWrap
            text: root.actionError !== "" ? root.actionError : Errors.format(I18n, root.pairMessage, root.pairMessage)
            color: Commons.Color.urgent
            font.family: root.theme.family
            font.pixelSize: root.theme.bodySmall
        }

        // Big emoji verification display
        Rectangle {
            visible: root.emoji !== ""
            Layout.alignment: Qt.AlignHCenter
            radius: root.theme.radius
            color: Commons.Color.popups.background
            border.width: 1
            border.color: Commons.Color.popups.border
            implicitWidth: emojiText.implicitWidth + root.theme.padX * 2
            implicitHeight: emojiText.implicitHeight + root.theme.padY * 2

            Text {
                id: emojiText
                anchors.centerIn: parent
                textFormat: Text.PlainText
                text: root.emoji
                color: Commons.Color.foreground
                font.family: root.theme.family
                font.pixelSize: Math.round(root.theme.title * 2.5)
            }
        }

        // QR Code SVG display
        Rectangle {
            visible: root.qrSvg !== ""
            Layout.alignment: Qt.AlignHCenter
            width: Style.space(240)
            height: Style.space(240)
            radius: root.theme.radius
            color: "#FFFFFF"
            border.width: 1
            border.color: Commons.Color.popups.border

            Image {
                anchors.fill: parent
                anchors.margins: Style.space(12)
                fillMode: Image.PreserveAspectFit
                source: root.qrSvg !== "" ? ("data:image/svg+xml;utf8," + encodeURIComponent(root.qrSvg)) : ""
            }
        }

        // Manual pairing URL link
        Rectangle {
            visible: root.manualUrl !== ""
            Layout.alignment: Qt.AlignHCenter
            Layout.fillWidth: true
            implicitHeight: urlText.implicitHeight + root.theme.padY * 2
            radius: root.theme.radius
            color: Commons.Color.popups.background
            border.width: 1
            border.color: Commons.Color.accent

            Text {
                id: urlText
                anchors.fill: parent
                anchors.margins: root.theme.padY
                horizontalAlignment: Text.AlignHCenter
                verticalAlignment: Text.AlignVCenter
                textFormat: Text.PlainText
                wrapMode: Text.WrapAnywhere
                text: root.manualUrl
                color: Commons.Color.accent
                font.family: root.theme.family
                font.pixelSize: root.theme.bodySmall
                font.underline: true
            }

            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: {
                    if (root.manualUrl !== "") {
                        Qt.openUrlExternally(root.manualUrl);
                    }
                }
            }
        }

        // Locked keyring retry button
        Rectangle {
            visible: root.isLockedState
            Layout.alignment: Qt.AlignHCenter
            implicitWidth: Math.max(Style.space(140), retryLabel.implicitWidth + root.theme.padX * 2)
            implicitHeight: root.theme.ctl
            radius: root.theme.radius
            color: Commons.Color.accent

            Text {
                id: retryLabel
                anchors.centerIn: parent
                textFormat: Text.PlainText
                text: I18n.t("gmessages.retry", "Retry")
                color: Commons.Color.background
                font.family: root.theme.family
                font.pixelSize: root.theme.body
                font.bold: true
            }

            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: root.retryKey()
            }
        }

        // Three pairing buttons: Sign in with Google, QR code, Manual
        RowLayout {
            visible: root.canStartPair
            Layout.alignment: Qt.AlignHCenter
            spacing: root.theme.gap

            Repeater {
                model: [
                    { method: "google", label: I18n.t("gmessages.pair_google", "Sign in with Google") },
                    { method: "qr", label: I18n.t("gmessages.pair_qr", "QR code") },
                    { method: "manual", label: I18n.t("gmessages.pair_manual", "Manual") }
                ]

                Rectangle {
                    required property var modelData
                    implicitWidth: btnText.implicitWidth + root.theme.padX * 2
                    implicitHeight: root.theme.ctl
                    radius: root.theme.radius
                    color: modelData.method === "google" ? Commons.Color.accent : Commons.Color.popups.background
                    border.width: 1
                    border.color: modelData.method === "google" ? Commons.Color.accent : Commons.Color.popups.border
                    opacity: root.busy ? 0.6 : 1.0

                    Text {
                        id: btnText
                        anchors.centerIn: parent
                        textFormat: Text.PlainText
                        text: parent.modelData.label
                        color: parent.modelData.method === "google" ? Commons.Color.background : Commons.Color.foreground
                        font.family: root.theme.family
                        font.pixelSize: root.theme.body
                        font.bold: parent.modelData.method === "google"
                    }

                    MouseArea {
                        anchors.fill: parent
                        enabled: !root.busy
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.startPairing(parent.modelData.method)
                    }
                }
            }
        }

        // Cancel pairing button while in pairing state
        Rectangle {
            visible: root.client.state === "pairing"
            Layout.alignment: Qt.AlignHCenter
            implicitWidth: cancelText.implicitWidth + root.theme.padX * 2
            implicitHeight: root.theme.ctl
            radius: root.theme.radius
            color: "transparent"
            border.width: 1
            border.color: Commons.Color.popups.border

            Text {
                id: cancelText
                anchors.centerIn: parent
                textFormat: Text.PlainText
                text: I18n.t("gmessages.cancel", "Cancel")
                color: Commons.Color.foreground
                font.family: root.theme.family
                font.pixelSize: root.theme.bodySmall
            }

            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: root.cancelPairing()
            }
        }
    }
}
