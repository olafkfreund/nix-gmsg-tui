pragma ComponentBehavior: Bound
// qmllint disable missing-property
import QtQuick
import QtQuick.Layouts
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Errors.js" as Errors

// Modal contact picker: filter the phone's contacts, or type a number to start a chat with it.
Item {
    id: root

    required property var client
    required property var theme
    property bool open: false

    property var contacts: []
    property bool loading: false
    property bool starting: false
    property string note: ""
    property int currentIndex: 0
    property int gen: 0
    readonly property string typed: filterField.text.trim()
    readonly property bool isNumber: /^\+?[0-9 ()-]{3,}$/.test(root.typed)

    signal closeRequested()
    signal started(string conversation)

    visible: root.open

    // First row may be "start a chat with <number>"; the rest are matching contacts.
    readonly property var rows: {
        var q = root.typed.toLowerCase();
        var res = [];
        if (root.isNumber) res.push({ name: "", number: root.typed, direct: true });
        for (var i = 0; i < root.contacts.length; i++) {
            var c = root.contacts[i];
            if (q === "" || String(c.name || "").toLowerCase().indexOf(q) >= 0 || String(c.number || "").indexOf(q) >= 0) {
                res.push({ name: String(c.name || ""), number: String(c.number || ""), direct: false });
            }
        }
        return res;
    }

    Keys.onTabPressed: function(event) { filterField.forceActiveFocus(); event.accepted = true; }
    Keys.onBacktabPressed: function(event) { filterField.forceActiveFocus(); event.accepted = true; }

    onRowsChanged: root.currentIndex = Math.min(root.currentIndex, Math.max(0, root.rows.length - 1))

    onOpenChanged: {
        if (root.open) {
            filterField.text = "";
            root.note = "";
            root.contacts = [];
            root.currentIndex = 0;
            root.starting = false;
            root.fetch(0, [], ++root.gen);
            filterField.forceActiveFocus();
        }
    }

    function fetch(offset, acc, gen) {
        root.loading = true;
        root.client.send("contacts", { offset: offset, limit: 200 }, function(r) {
            if (gen !== root.gen) return;
            if (!r.ok || !r.data) {
                root.loading = false;
                root.note = Errors.formatError(I18n, r.error);
                return;
            }
            var items = Array.isArray(r.data.items) ? r.data.items : [];
            var merged = acc.concat(items);
            root.contacts = merged;
            if (items.length === 200 && merged.length < (Number(r.data.total) || 0)) {
                root.fetch(offset + items.length, merged, gen);
            } else {
                root.loading = false;
            }
        });
    }

    function move(delta) {
        root.currentIndex = Math.max(0, Math.min(root.rows.length - 1, root.currentIndex + delta));
        list.positionViewAtIndex(root.currentIndex, ListView.Contain);
    }

    function start(number) {
        if (root.starting || String(number) === "") return;
        root.starting = true;
        root.note = "";
        var gen = root.gen;
        root.client.send("chat.start", { numbers: [String(number)] }, function(r) {
            if (gen !== root.gen || !root.open) return;
            root.starting = false;
            if (!r.ok || !r.data || !r.data.conversation) {
                root.note = Errors.formatError(I18n, r.error);
                return;
            }
            root.started(String(r.data.conversation));
        });
    }

    function activate() {
        if (root.currentIndex >= 0 && root.currentIndex < root.rows.length) {
            root.start(root.rows[root.currentIndex].number);
        }
    }

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
        height: Math.min(parent.height - root.theme.padY * 2, Style.space(480))
        radius: root.theme.radius
        color: Commons.Color.popups.background
        border.width: 1
        border.color: Commons.Color.popups.border

        MouseArea {
            anchors.fill: parent
        }

        ColumnLayout {
            anchors.fill: parent
            anchors.margins: root.theme.padX
            spacing: root.theme.gap

            Text {
                Layout.fillWidth: true
                textFormat: Text.PlainText
                text: I18n.t("gmessages.new_chat", "New chat")
                color: Commons.Color.foreground
                font.family: root.theme.family
                font.pixelSize: root.theme.subtitle
                font.bold: true
            }

            TextField {
                id: filterField
                Layout.fillWidth: true
                placeholderText: I18n.t("gmessages.new_chat_placeholder", "Search contacts or type a number…")
                Keys.onEscapePressed: function(event) {
                    root.closeRequested();
                    event.accepted = true;
                }
                Keys.onDownPressed: function(event) {
                    root.move(1);
                    event.accepted = true;
                }
                // Only focusable item in the modal: keep focus here.
                Keys.onTabPressed: function(event) { event.accepted = true; }
                Keys.onBacktabPressed: function(event) { event.accepted = true; }
                Keys.onUpPressed: function(event) {
                    root.move(-1);
                    event.accepted = true;
                }
                Keys.onReturnPressed: function(event) {
                    root.activate();
                    event.accepted = true;
                }
                Keys.onEnterPressed: function(event) {
                    root.activate();
                    event.accepted = true;
                }
            }

            Text {
                visible: root.note !== ""
                Layout.fillWidth: true
                textFormat: Text.PlainText
                wrapMode: Text.Wrap
                text: root.note
                color: Commons.Color.urgent
                font.family: root.theme.family
                font.pixelSize: root.theme.caption
            }

            Item {
                Layout.fillWidth: true
                Layout.fillHeight: true

                ListView {
                    id: list
                    anchors.fill: parent
                    clip: true
                    model: root.rows
                    opacity: root.starting ? 0.5 : 1.0

                    delegate: Rectangle {
                        id: row
                        required property var modelData
                        required property int index
                        width: list.width
                        height: Style.space(44)
                        color: row.index === root.currentIndex ? Util.alpha(Commons.Color.accent, 0.18) : "transparent"

                        ColumnLayout {
                            anchors.fill: parent
                            anchors.leftMargin: root.theme.padX / 2
                            anchors.rightMargin: root.theme.padX / 2
                            spacing: 0

                            Item { Layout.fillHeight: true }

                            Text {
                                Layout.fillWidth: true
                                textFormat: Text.PlainText
                                elide: Text.ElideRight
                                text: row.modelData.direct
                                    ? I18n.t("gmessages.start_chat_with", "Start a chat with %1").replace("%1", row.modelData.number)
                                    : (row.modelData.name !== "" ? row.modelData.name : row.modelData.number)
                                color: Commons.Color.foreground
                                font.family: root.theme.family
                                font.pixelSize: root.theme.body
                            }

                            Text {
                                visible: !row.modelData.direct && row.modelData.name !== ""
                                Layout.fillWidth: true
                                textFormat: Text.PlainText
                                elide: Text.ElideRight
                                text: row.modelData.number
                                color: Commons.Color.muted
                                font.family: root.theme.family
                                font.pixelSize: root.theme.caption
                            }

                            Item { Layout.fillHeight: true }
                        }

                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: {
                                root.currentIndex = row.index;
                                root.activate();
                            }
                        }
                    }
                }

                Text {
                    anchors.centerIn: parent
                    visible: root.rows.length === 0
                    textFormat: Text.PlainText
                    text: root.loading
                        ? I18n.t("gmessages.loading_contacts", "Loading contacts…")
                        : I18n.t("gmessages.no_contacts", "No contacts")
                    color: Commons.Color.muted
                    font.family: root.theme.family
                    font.pixelSize: root.theme.body
                }
            }
        }
    }
}
