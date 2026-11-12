pragma ComponentBehavior: Bound
// qmllint disable missing-property
// ^ qs.Commons / qs.Ui expose nested QtObjects (Style.spacing, Color.bar) the linter cannot type
import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui as Ui
import "Errors.js" as Errors

Ui.BarWidget {
    id: root
    moduleName: "olafkfreund.gmessages"

    property var shell: null
    property var manifest: null

    implicitWidth: row.implicitWidth
    implicitHeight: button.implicitHeight

    Client {
        id: client
        role: "bar"
    }

    readonly property string glyph: {
        if (!client.connected) return "\u{F036A}"; // nf-md-message_off_outline
        if (client.mismatch) return "\u{F0026}"; // nf-md-alert_circle_outline
        if (client.state === "locked" || client.state === "keyring_missing") return "\u{F033E}"; // nf-md-lock_outline
        if (client.state === "login" || client.state === "pairing" || client.state === "expired") return "\u{F0432}"; // nf-md-qrcode_scan
        if (client.unread > 0) return "\u{F0369}"; // nf-md-message_badge_outline
        return "\u{F0365}"; // nf-md-message_text_outline
    }

    readonly property string statusTip: {
        var title = I18n.t("gmessages.app_name", "Messages");
        if (!client.connected) {
            return title + ": " + Errors.format(I18n, "disconnected", "Disconnected from the background service.");
        }
        if (client.mismatch) {
            return title + ": " + Errors.format(I18n, "protocol_mismatch", "Daemon protocol mismatch.");
        }
        if (client.state === "locked" || client.state === "keyring_missing") {
            return title + ": " + Errors.format(I18n, client.state, "Unlock your keyring");
        }
        if (client.state === "login" || client.state === "pairing" || client.state === "expired") {
            return title + ": " + Errors.format(I18n, client.state, "Sign in required");
        }
        if (client.unread > 0) {
            return title + " · " + I18n.tn("gmessages.unread_count", client.unread, client.unread + " unread").replace("%n", String(client.unread));
        }
        return title;
    }

    Row {
        id: row
        height: parent.height
        spacing: Math.round(Style.space(4))

        Ui.BarIconButton {
            id: button
            bar: root.bar
            text: root.glyph
            active: client.connected && client.unread > 0
            dimmed: !client.connected || client.mismatch
            tooltipText: root.statusTip
            onPressed: function() {
                Quickshell.execDetached(["omarchy-shell", "olafkfreund.gmessages", "toggle"]);
            }
        }

        Rectangle {
            id: badge
            visible: client.connected && !client.mismatch && client.unread > 0
            anchors.verticalCenter: parent.verticalCenter
            height: Math.max(Style.space(16), badgeText.implicitHeight + Style.space(4))
            width: Math.max(height, badgeText.implicitWidth + Style.space(8))
            radius: height / 2
            color: Commons.Color.accent

            Text {
                id: badgeText
                anchors.centerIn: parent
                textFormat: Text.PlainText
                text: String(client.unread)
                color: Commons.Color.background
                font.family: Style.font.family
                font.pixelSize: Style.font.caption
                font.bold: true
            }

            MouseArea {
                anchors.fill: parent
                cursorShape: Qt.PointingHandCursor
                onClicked: Quickshell.execDetached(["omarchy-shell", "olafkfreund.gmessages", "toggle"])
            }
        }
    }
}
