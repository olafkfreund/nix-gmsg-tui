pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import Quickshell.Widgets
import qs.Commons
import qs.Commons as Commons

Item {
    id: root

    required property var theme
    property string path: ""
    property string color: ""
    property string name: ""
    property bool group: false
    property real size: Style.space(32)

    implicitWidth: root.size
    implicitHeight: root.size

    readonly property bool hasColor: /^#[0-9a-f]{6}$/.test(root.color)
    readonly property color fill: root.hasColor ? root.color : Commons.Color.muted
    // Perceived brightness of the fill picks a dark or light text.
    readonly property bool lightFill: {
        if (!root.hasColor) return false;
        var r = parseInt(root.color.slice(1, 3), 16);
        var g = parseInt(root.color.slice(3, 5), 16);
        var b = parseInt(root.color.slice(5, 7), 16);
        return (0.299 * r + 0.587 * g + 0.114 * b) > 150;
    }
    readonly property color ink: root.hasColor
        ? (root.lightFill ? "#202124" : "#ffffff")
        : Commons.Color.foreground

    // First letter of the first two words, split on code points.
    readonly property string initials: {
        if (root.group) return "";
        var words = root.name.trim().split(/\s+/);
        var out = "";
        for (var i = 0; i < words.length && out.length < 2; i++) {
            var first = Array.from(words[i])[0] || "";
            if (/\p{L}/u.test(first)) out += first.toUpperCase();
        }
        return out;
    }
    readonly property bool showImage: !root.group && root.path !== "" && photo.status !== Image.Error

    Rectangle {
        anchors.fill: parent
        radius: root.size / 2
        color: root.fill
        visible: !root.showImage || photo.status !== Image.Ready

        Text {
            anchors.centerIn: parent
            textFormat: Text.PlainText
            text: root.initials !== "" ? root.initials : "\u{F0004}" // nf-md-account
            color: root.ink
            font.family: root.theme.family
            font.pixelSize: root.size * 0.4
            font.bold: root.initials !== ""
        }
    }

    ClippingRectangle {
        anchors.fill: parent
        radius: root.size / 2
        color: "transparent"
        visible: root.showImage

        Image {
            id: photo
            anchors.fill: parent
            source: root.group || root.path === "" ? "" : "file://" + root.path
            asynchronous: true
            fillMode: Image.PreserveAspectCrop
            sourceSize.width: root.size
            sourceSize.height: root.size
        }
    }
}
