pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons

// Modal confirmation: Cancel has focus first; Esc cancels, Enter presses the focused button.
Item {
    id: root

    required property var theme
    property bool open: false
    property string title: ""
    property string body: ""
    property string actionLabel: ""

    signal accepted()
    signal cancelled()

    visible: root.open

    onOpenChanged: {
        if (root.open) cancelBtn.forceActiveFocus();
    }

    Keys.onEscapePressed: function(event) {
        root.cancelled();
        event.accepted = true;
    }

    // Tab and Backtab cycle the two buttons and never leave the modal.
    Keys.onTabPressed: function(event) { cancelBtn.forceActiveFocus(); event.accepted = true; }
    Keys.onBacktabPressed: function(event) { okBtn.forceActiveFocus(); event.accepted = true; }

    component Btn: Rectangle {
        id: btn
        property string label: ""
        property color fill: "transparent"
        property color ink: Commons.Color.foreground
        signal pressed()

        activeFocusOnTab: true
        implicitWidth: lbl.implicitWidth + Style.space(24)
        implicitHeight: lbl.implicitHeight + Style.space(10)
        radius: root.theme.radius
        color: btn.fill
        border.width: btn.activeFocus ? 2 : 1
        border.color: btn.activeFocus ? Commons.Color.foreground : Commons.Color.popups.border

        Keys.onReturnPressed: function(event) { btn.pressed(); event.accepted = true; }
        Keys.onEnterPressed: function(event) { btn.pressed(); event.accepted = true; }
        Keys.onSpacePressed: function(event) { btn.pressed(); event.accepted = true; }

        Text {
            id: lbl
            anchors.centerIn: parent
            textFormat: Text.PlainText
            text: btn.label
            color: btn.ink
            font.family: root.theme.family
            font.pixelSize: root.theme.body
            font.bold: true
        }

        MouseArea {
            anchors.fill: parent
            cursorShape: Qt.PointingHandCursor
            onClicked: btn.pressed()
        }
    }

    Rectangle {
        anchors.fill: parent
        color: Util.alpha(Commons.Color.background, 0.72)

        MouseArea {
            anchors.fill: parent
            onClicked: root.cancelled()
        }
    }

    Rectangle {
        anchors.centerIn: parent
        width: Math.min(parent.width - root.theme.padX * 2, Style.space(400))
        implicitHeight: col.implicitHeight + root.theme.padY * 2
        radius: root.theme.radius
        color: Commons.Color.popups.background
        border.width: 1
        border.color: Commons.Color.popups.border

        MouseArea {
            anchors.fill: parent
        }

        ColumnLayout {
            id: col
            anchors.fill: parent
            anchors.margins: root.theme.padX
            spacing: root.theme.gap

            Text {
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                text: root.title
                color: Commons.Color.foreground
                font.family: root.theme.family
                font.pixelSize: root.theme.subtitle
                font.bold: true
            }

            Text {
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                text: root.body
                color: Commons.Color.foreground
                opacity: root.theme.dim
                font.family: root.theme.family
                font.pixelSize: root.theme.bodySmall
            }

            RowLayout {
                Layout.fillWidth: true
                spacing: root.theme.gap

                Item { Layout.fillWidth: true }

                Btn {
                    id: cancelBtn
                    label: I18n.t("gmessages.cancel", "Cancel")
                    KeyNavigation.right: okBtn
                    KeyNavigation.tab: okBtn
                    KeyNavigation.backtab: okBtn
                    onPressed: root.cancelled()
                }

                Btn {
                    id: okBtn
                    label: root.actionLabel
                    fill: Commons.Color.urgent
                    ink: Commons.Color.background
                    KeyNavigation.left: cancelBtn
                    KeyNavigation.tab: cancelBtn
                    KeyNavigation.backtab: cancelBtn
                    onPressed: root.accepted()
                }
            }
        }
    }
}
