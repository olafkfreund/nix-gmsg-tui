pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons

Item {
    id: root

    required property var theme
    property bool open: false

    signal closeRequested()

    visible: root.open

    Rectangle {
        anchors.fill: parent
        color: Util.alpha(Commons.Color.background, 0.72)

        MouseArea {
            anchors.fill: parent
            onClicked: root.closeRequested()
        }
    }

    Rectangle {
        anchors.centerIn: parent
        width: Math.min(parent.width - root.theme.padX * 2, Style.space(420))
        implicitHeight: contentCol.implicitHeight + root.theme.padY * 2
        radius: root.theme.radius
        color: Commons.Color.popups.background
        border.width: 1
        border.color: Commons.Color.popups.border

        MouseArea {
            anchors.fill: parent
        }

        ColumnLayout {
            id: contentCol
            anchors.fill: parent
            anchors.leftMargin: root.theme.padX
            anchors.rightMargin: root.theme.padX
            anchors.topMargin: root.theme.padY
            anchors.bottomMargin: root.theme.padY
            spacing: root.theme.gap

            RowLayout {
                Layout.fillWidth: true

                Text {
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    text: I18n.t("gmessages.help_title", "Keyboard Shortcuts")
                    color: Commons.Color.foreground
                    font.family: root.theme.family
                    font.pixelSize: root.theme.subtitle
                    font.bold: true
                }

                Text {
                    textFormat: Text.PlainText
                    text: "Esc / ?"
                    color: Commons.Color.muted
                    font.family: root.theme.family
                    font.pixelSize: root.theme.caption
                }
            }

            Rectangle {
                Layout.fillWidth: true
                height: 1
                color: Commons.Color.popups.border
            }

            Repeater {
                model: [
                    { keys: "j / k / ↓ / ↑", desc: I18n.t("gmessages.help_move", "Move selection in conversation list") },
                    { keys: "Enter", desc: I18n.t("gmessages.help_open", "Open selected conversation") },
                    { keys: "i", desc: I18n.t("gmessages.help_compose", "Focus message composer") },
                    { keys: "Ctrl+Return", desc: I18n.t("gmessages.help_send", "Send message") },
                    { keys: "/", desc: I18n.t("gmessages.help_filter", "Filter conversations") },
                    { keys: "n", desc: I18n.t("gmessages.help_new", "New chat") },
                    { keys: "a", desc: I18n.t("gmessages.help_folder", "Switch between Inbox and Archived") },
                    { keys: "e", desc: I18n.t("gmessages.help_archive", "Archive or restore conversation") },
                    { keys: "Delete / #", desc: I18n.t("gmessages.help_delete", "Delete conversation (or the message under the pointer)") },
                    { keys: "\u22EE", desc: I18n.t("gmessages.help_menu", "Menu: unpair phone, remove all data") },
                    { keys: "Esc", desc: I18n.t("gmessages.help_back", "Back to conversation list or hide window") },
                    { keys: "?", desc: I18n.t("gmessages.help_toggle", "Toggle keyboard shortcuts") },
                    { keys: "q", desc: I18n.t("gmessages.help_quit", "Hide window") }
                ]

                RowLayout {
                    id: helpRow
                    required property var modelData
                    Layout.fillWidth: true
                    spacing: root.theme.gap

                    Rectangle {
                        implicitWidth: Math.max(Style.space(110), keyLabel.implicitWidth + Style.space(12))
                        implicitHeight: keyLabel.implicitHeight + Style.space(6)
                        radius: Math.max(2, root.theme.radius / 2)
                        color: Util.alpha(Commons.Color.foreground, 0.08)
                        border.width: 1
                        border.color: Commons.Color.popups.border

                        Text {
                            id: keyLabel
                            anchors.centerIn: parent
                            textFormat: Text.PlainText
                            text: helpRow.modelData.keys
                            color: Commons.Color.foreground
                            font.family: root.theme.family
                            font.pixelSize: root.theme.caption
                            font.bold: true
                        }
                    }

                    Text {
                        Layout.fillWidth: true
                        textFormat: Text.PlainText
                        wrapMode: Text.WordWrap
                        text: helpRow.modelData.desc
                        color: Commons.Color.foreground
                        opacity: root.theme.dim
                        font.family: root.theme.family
                        font.pixelSize: root.theme.bodySmall
                    }
                }
            }
        }
    }
}
