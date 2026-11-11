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
    property string selectedId: ""
    property string selectedName: ""
    property string filterText: ""
    property bool filterActive: false

    signal conversationSelected(string id, string name)

    property var allItems: []
    property int totalCount: 0
    property bool loading: false

    readonly property var filteredItems: {
        var q = root.filterText.trim().toLowerCase();
        var res = [];
        for (var i = 0; i < root.allItems.length; i++) {
            var c = root.allItems[i];
            if (!c || c.hidden) continue;
            if (q !== "") {
                var n = String(c.name || "").toLowerCase();
                var s = String(c.snippet || "").toLowerCase();
                if (n.indexOf(q) < 0 && s.indexOf(q) < 0) continue;
            }
            res.push(c);
        }
        return res;
    }

    property int currentIndex: 0

    onFilteredItemsChanged: {
        if (root.selectedId !== "") {
            for (var i = 0; i < root.filteredItems.length; i++) {
                if (root.filteredItems[i].id === root.selectedId) {
                    root.currentIndex = i;
                    return;
                }
            }
        }
        if (root.currentIndex >= root.filteredItems.length) {
            root.currentIndex = Math.max(0, root.filteredItems.length - 1);
        }
    }

    function reload() {
        if (!root.client.connected || root.client.state !== "ready") return;
        root.loading = true;
        root.fetchPage(0, []);
    }

    function fetchPage(offset, acc) {
        root.client.send("conversations", { folder: "inbox", offset: offset, limit: 200 }, function(r) {
            if (!r.ok || !r.data) {
                root.loading = false;
                return;
            }
            var items = Array.isArray(r.data.items) ? r.data.items : [];
            var total = Number(r.data.total) || 0;
            root.totalCount = total;
            var merged = acc.concat(items);
            merged.sort(function(a, b) { return (Number(b.last_ms) || 0) - (Number(a.last_ms) || 0); });
            root.allItems = merged;
            if (offset + items.length < total && items.length === 200) {
                root.fetchPage(offset + items.length, merged);
            } else {
                root.loading = false;
            }
        });
    }

    function upsertConversation(c) {
        if (!c || !c.id) return;
        var next = [];
        var found = false;
        for (var i = 0; i < root.allItems.length; i++) {
            var cur = root.allItems[i];
            if (cur.id === c.id) {
                found = true;
                if (!c.hidden && (c.folder === "inbox" || !c.folder)) {
                    next.push(c);
                }
            } else {
                next.push(cur);
            }
        }
        if (!found && !c.hidden && (c.folder === "inbox" || !c.folder)) {
            next.push(c);
        }
        next.sort(function(a, b) { return (Number(b.last_ms) || 0) - (Number(a.last_ms) || 0); });
        root.allItems = next;
        if (c.id === root.selectedId && c.name) {
            root.selectedName = String(c.name);
        }
    }

    function markReadLocal(convId) {
        var next = [];
        for (var i = 0; i < root.allItems.length; i++) {
            var c = root.allItems[i];
            if (c.id === convId && c.unread) {
                next.push(Object.assign({}, c, { unread: false }));
            } else {
                next.push(c);
            }
        }
        root.allItems = next;
    }

    function moveSelection(delta) {
        var count = root.filteredItems.length;
        if (count === 0) return;
        var next = Math.max(0, Math.min(count - 1, root.currentIndex + delta));
        root.currentIndex = next;
        listView.positionViewAtIndex(next, ListView.Contain);
    }

    function activateCurrent() {
        if (root.currentIndex >= 0 && root.currentIndex < root.filteredItems.length) {
            var c = root.filteredItems[root.currentIndex];
            root.selectConversation(c.id, c.name || "");
        }
    }

    function selectConversation(id, name) {
        root.selectedId = String(id || "");
        var resolvedName = String(name || "");
        if (resolvedName === "") {
            for (var i = 0; i < root.allItems.length; i++) {
                if (root.allItems[i].id === root.selectedId) {
                    resolvedName = String(root.allItems[i].name || "");
                    break;
                }
            }
        }
        root.selectedName = resolvedName;
        for (var j = 0; j < root.filteredItems.length; j++) {
            if (root.filteredItems[j].id === root.selectedId) {
                root.currentIndex = j;
                listView.positionViewAtIndex(j, ListView.Contain);
                break;
            }
        }
        root.conversationSelected(root.selectedId, root.selectedName);
    }

    function openFilter() {
        root.filterActive = true;
        filterField.forceActiveFocus();
    }

    function clearFilter() {
        root.filterText = "";
        filterField.text = "";
        root.filterActive = false;
    }

    function formatTime(ms) {
        var n = Number(ms) || 0;
        if (n <= 0) return "";
        var dt = new Date(n);
        var now = new Date();
        var loc = Qt.locale(I18n.formatLocale || "");
        if (dt.getFullYear() === now.getFullYear() && dt.getMonth() === now.getMonth() && dt.getDate() === now.getDate()) {
            return dt.toLocaleTimeString(loc, Locale.ShortFormat);
        }
        return dt.toLocaleDateString(loc, Locale.ShortFormat);
    }

    Connections {
        target: root.client
        function onStateChanged() {
            if (root.client.state === "ready") {
                root.reload();
            }
        }
        function onConversationChanged(c) {
            root.upsertConversation(c);
        }
    }

    Component.onCompleted: {
        if (root.client.state === "ready") {
            root.reload();
        }
    }

    ColumnLayout {
        anchors.fill: parent
        spacing: 0

        // Header / Filter bar
        Rectangle {
            Layout.fillWidth: true
            Layout.preferredHeight: root.theme.ctl + root.theme.padY
            color: "transparent"

            RowLayout {
                anchors.fill: parent
                anchors.leftMargin: root.theme.padX
                anchors.rightMargin: root.theme.padX
                spacing: root.theme.gap

                Text {
                    visible: !root.filterActive && root.filterText === ""
                    Layout.fillWidth: true
                    textFormat: Text.PlainText
                    text: I18n.t("gmessages.conversations_title", "Conversations")
                    color: Commons.Color.foreground
                    font.family: root.theme.family
                    font.pixelSize: root.theme.subtitle
                    font.bold: true
                    elide: Text.ElideRight
                }

                TextField {
                    id: filterField
                    visible: root.filterActive || root.filterText !== ""
                    Layout.fillWidth: true
                    placeholderText: I18n.t("gmessages.filter_placeholder", "Filter conversations…")
                    onTextChanged: root.filterText = text
                    Keys.onEscapePressed: function(event) {
                        root.clearFilter();
                        event.accepted = true;
                    }
                    Keys.onReturnPressed: function(event) {
                        filterField.focus = false;
                        root.activateCurrent();
                        event.accepted = true;
                    }
                }

                Rectangle {
                    implicitWidth: root.theme.ctl - Style.space(6)
                    implicitHeight: root.theme.ctl - Style.space(6)
                    radius: root.theme.radius
                    color: "transparent"

                    Text {
                        anchors.centerIn: parent
                        textFormat: Text.PlainText
                        text: (root.filterActive || root.filterText !== "") ? "\u{F0156}" : "\u{F0349}"
                        color: Commons.Color.foreground
                        opacity: root.theme.dim
                        font.family: root.theme.family
                        font.pixelSize: root.theme.body
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            if (root.filterActive || root.filterText !== "") {
                                root.clearFilter();
                            } else {
                                root.openFilter();
                            }
                        }
                    }
                }
            }
        }

        Rectangle {
            Layout.fillWidth: true
            height: 1
            color: Commons.Color.popups.border
        }

        Item {
            Layout.fillWidth: true
            Layout.fillHeight: true

            ListView {
                id: listView
                anchors.fill: parent
                clip: true
                model: root.filteredItems
                currentIndex: root.currentIndex

                delegate: Rectangle {
                    id: rowDelegate
                    required property var modelData
                    required property int index

                    width: listView.width
                    height: Math.max(Style.space(64), rowCol.implicitHeight + root.theme.padY * 2)
                    readonly property bool isSelected: modelData.id === root.selectedId
                    readonly property bool isHighlighted: index === root.currentIndex
                    color: isSelected
                        ? Util.alpha(Commons.Color.accent, 0.18)
                        : (isHighlighted ? Util.alpha(Commons.Color.foreground, 0.08) : "transparent")

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: root.theme.padX
                        anchors.rightMargin: root.theme.padX
                        spacing: root.theme.gap

                        // Unread dot
                        Rectangle {
                            Layout.alignment: Qt.AlignVCenter
                            width: Style.space(8)
                            height: Style.space(8)
                            radius: width / 2
                            color: rowDelegate.modelData.unread ? Commons.Color.accent : "transparent"
                        }

                        ColumnLayout {
                            id: rowCol
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignVCenter
                            spacing: Style.space(4)

                            RowLayout {
                                Layout.fillWidth: true
                                spacing: root.theme.gap

                                Text {
                                    Layout.fillWidth: true
                                    textFormat: Text.PlainText
                                    elide: Text.ElideRight
                                    text: String(rowDelegate.modelData.name || I18n.t("gmessages.unknown_contact", "Unknown"))
                                    color: Commons.Color.foreground
                                    font.family: root.theme.family
                                    font.pixelSize: root.theme.body
                                    font.bold: rowDelegate.modelData.unread === true
                                }

                                Text {
                                    textFormat: Text.PlainText
                                    text: root.formatTime(rowDelegate.modelData.last_ms)
                                    color: Commons.Color.muted
                                    font.family: root.theme.family
                                    font.pixelSize: root.theme.caption
                                }
                            }

                            Text {
                                Layout.fillWidth: true
                                textFormat: Text.PlainText
                                elide: Text.ElideRight
                                maximumLineCount: 1
                                text: String(rowDelegate.modelData.snippet || "")
                                color: rowDelegate.modelData.unread ? Commons.Color.foreground : Commons.Color.muted
                                opacity: rowDelegate.modelData.unread ? 0.9 : root.theme.dim
                                font.family: root.theme.family
                                font.pixelSize: root.theme.bodySmall
                            }
                        }
                    }

                    Rectangle {
                        anchors.bottom: parent.bottom
                        anchors.left: parent.left
                        anchors.right: parent.right
                        height: 1
                        color: Util.alpha(Commons.Color.popups.border, 0.5)
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            root.currentIndex = rowDelegate.index;
                            root.selectConversation(rowDelegate.modelData.id, rowDelegate.modelData.name || "");
                        }
                    }
                }
            }

            Text {
                anchors.centerIn: parent
                visible: root.filteredItems.length === 0
                textFormat: Text.PlainText
                text: root.loading
                    ? I18n.t("gmessages.loading_conversations", "Loading conversations…")
                    : I18n.t("gmessages.no_conversations", "No conversations")
                color: Commons.Color.muted
                font.family: root.theme.family
                font.pixelSize: root.theme.body
            }
        }
    }
}
