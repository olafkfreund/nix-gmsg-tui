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
    property string selectedId: ""
    property string selectedName: ""
    property string filterText: ""
    property bool filterActive: false
    readonly property bool editing: filterField.activeFocus

    signal conversationSelected(string id, string name)
    signal conversationRemoved(string id)
    signal conversationHidden(string id)
    signal newChatRequested()
    signal confirmRequested(string title, string body, string actionLabel, var action)

    // "inbox" or "archive" (the daemon's folder name); a page that comes back for another folder or generation is dropped.
    property string folder: "inbox"
    property int gen: 0
    property string status: ""
    // Rows with an archive/restore/delete in flight: id -> {}. Cleared by the row's event, a failure or 10 s.
    property var busyRows: ({})
    property int attempts: 0

    property var allItems: []
    // Ids removed or moved away since the last reload; a later page must not bring them back.
    property var goneIds: ({})
    // Conversation id to picture path; replaced whole on each change.
    property var avatars: ({})
    readonly property var selectedItem: {
        for (var i = 0; i < root.allItems.length; i++) {
            if (root.allItems[i].id === root.selectedId) return root.allItems[i];
        }
        return null;
    }
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

    function noteAvatars(items) {
        var next = Object.assign({}, root.avatars);
        var changed = false;
        for (var i = 0; i < items.length; i++) {
            var c = items[i];
            if (c && c.id && c.avatar && next[c.id] !== c.avatar) {
                next[c.id] = c.avatar;
                changed = true;
            }
        }
        if (changed) root.avatars = next;
    }

    function reload() {
        if (!root.client.connected || root.client.state !== "ready") return;
        root.loading = true;
        root.goneIds = ({});
        root.gen += 1;
        root.fetchPage(0, [], root.gen);
    }

    function fetchPage(offset, acc, gen) {
        root.client.send("conversations", { folder: root.folder, offset: offset, limit: 200 }, function(r) {
            if (gen !== root.gen) return;
            if (!r.ok || !r.data) {
                root.loading = false;
                return;
            }
            var items = Array.isArray(r.data.items) ? r.data.items : [];
            var total = Number(r.data.total) || 0;
            root.totalCount = total;
            root.noteAvatars(items);
            var merged = acc.concat(items).filter(function(c) { return !(c.id in root.goneIds); });
            merged.sort(function(a, b) { return (Number(b.last_ms) || 0) - (Number(a.last_ms) || 0); });
            root.allItems = merged;
            if (offset + items.length < total && items.length === 200) {
                root.fetchPage(offset + items.length, merged, gen);
            } else {
                root.loading = false;
                root.resolveSelectedName();
            }
        });
    }

    function upsertConversation(c) {
        if (!c || !c.id) return;
        root.noteAvatars([c]);
        root.setBusy(c.id, false);
        var here = !c.hidden && (c.folder || "inbox") === root.folder;
        if (here) delete root.goneIds[c.id]; else root.goneIds[c.id] = true;
        if (c.hidden && c.id === root.selectedId) root.conversationHidden(c.id);
        var next = [];
        var found = false;
        for (var i = 0; i < root.allItems.length; i++) {
            var cur = root.allItems[i];
            if (cur.id === c.id) {
                found = true;
                if (here) {
                    // A conversation event may come without a snippet; keep the last one known.
                    var keep = {};
                    if (!c.snippet) keep.snippet = cur.snippet || "";
                    if (!c.color) keep.color = cur.color || "";
                    next.push(Object.assign({}, c, keep));
                }
            } else {
                next.push(cur);
            }
        }
        if (!found && here) {
            next.push(c);
        }
        next.sort(function(a, b) { return (Number(b.last_ms) || 0) - (Number(a.last_ms) || 0); });
        root.allItems = next;
        if (c.id === root.selectedId && c.name && String(c.name) !== root.selectedName) {
            root.selectedName = String(c.name);
            root.conversationSelected(root.selectedId, root.selectedName);
        }
    }

    function setBusy(id, on) {
        if (!on && !(id in root.busyRows)) return;
        var next = Object.assign({}, root.busyRows);
        if (on) next[id] = { attempt: root.attempts }; else delete next[id];
        root.busyRows = next;
    }

    Component {
        id: busyClear
        Timer {
            property string key: ""
            property int attempt: 0
            interval: 10000
            running: true
            onTriggered: {
                // Only the attempt that started it may un-dim the row.
                if ((root.busyRows[key] || ({})).attempt === attempt) root.setBusy(key, false);
                destroy();
            }
        }
    }

    function removeConversation(id) {
        root.setBusy(id, false);
        root.goneIds[id] = true;
        var next = [];
        for (var i = 0; i < root.allItems.length; i++) {
            if (root.allItems[i].id !== id) next.push(root.allItems[i]);
        }
        if (next.length !== root.allItems.length) root.allItems = next;
        if (id === root.selectedId) {
            root.selectedId = "";
            root.selectedName = "";
        }
        root.conversationRemoved(id);
    }

    function failed(id, r) {
        root.setBusy(id, false);
        root.status = Errors.formatError(I18n, r.error);
    }

    function archiveOrRestore(id) {
        if (id in root.busyRows) return;
        var method = root.folder === "inbox" ? "archive" : "restore";
        root.status = "";
        root.attempts += 1;
        root.setBusy(id, true);
        busyClear.createObject(root, { key: id, attempt: root.attempts });
        root.client.send(method, { conversation: id }, function(r) {
            if (!r.ok) root.failed(id, r);
        });
    }

    function archiveCurrent() {
        if (root.currentIndex >= 0 && root.currentIndex < root.filteredItems.length) {
            root.archiveOrRestore(root.filteredItems[root.currentIndex].id);
        }
    }

    function askDelete(id, name) {
        if (id in root.busyRows) return;
        root.confirmRequested(
            I18n.t("gmessages.delete_chat_title", "Delete conversation?"),
            I18n.t("gmessages.delete_chat_body", "This deletes the conversation for good, on your phone too.") + (name !== "" ? "\n" + name : ""),
            I18n.t("gmessages.delete", "Delete"),
            function() { root.deleteChat(id); });
    }

    function deleteChat(id) {
        root.status = "";
        root.attempts += 1;
        root.setBusy(id, true);
        busyClear.createObject(root, { key: id, attempt: root.attempts });
        root.client.send("delete.chat", { conversation: id }, function(r) {
            if (r.ok) root.removeConversation(id);
            else root.failed(id, r);
        });
    }

    function deleteCurrent() {
        if (root.currentIndex >= 0 && root.currentIndex < root.filteredItems.length) {
            var c = root.filteredItems[root.currentIndex];
            root.askDelete(c.id, String(c.name || ""));
        }
    }

    function setFolder(f) {
        if (f === root.folder) return;
        root.folder = f;
        root.status = "";
        root.allItems = [];
        root.totalCount = 0;
        root.reload();
    }

    function toggleFolder() {
        root.setFolder(root.folder === "inbox" ? "archive" : "inbox");
    }

    // Opened before the list loaded (from a notification): give the thread its name now.
    function resolveSelectedName() {
        if (root.selectedId === "" || root.selectedName !== "") return;
        for (var i = 0; i < root.allItems.length; i++) {
            if (root.allItems[i].id === root.selectedId && root.allItems[i].name) {
                root.selectedName = String(root.allItems[i].name);
                root.conversationSelected(root.selectedId, root.selectedName);
                return;
            }
        }
    }

    // A live or sent message moves its conversation up with a fresh snippet.
    function noteMessage(convId, msg, notify) {
        if (!msg || msg.deleted) return;
        var next = [];
        var found = false;
        for (var i = 0; i < root.allItems.length; i++) {
            var c = root.allItems[i];
            if (c.id === convId) {
                found = true;
                var t = Number(msg.timestamp_ms) || 0;
                next.push(Object.assign({}, c, {
                    snippet: String(msg.text || "").slice(0, 80),
                    last_ms: Math.max(Number(c.last_ms) || 0, t),
                    unread: c.unread || notify === true
                }));
            } else {
                next.push(c);
            }
        }
        if (!found) return;
        next.sort(function(a, b) { return (Number(b.last_ms) || 0) - (Number(a.last_ms) || 0); });
        root.allItems = next;
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
            if (root.client.state === "login") root.avatars = ({});
            if (root.client.state === "ready") {
                root.reload();
            }
        }
        function onResynced() {
            if (root.client.state === "ready") {
                root.reload();
            }
        }
        function onConversationChanged(c) {
            root.upsertConversation(c);
        }
        function onAvatarsArrived(items) {
            var next = Object.assign({}, root.avatars);
            for (var id in items) next[id] = items[id];
            root.avatars = next;
        }
        function onMessageArrived(conv, msg, notify) {
            root.noteMessage(conv, msg, notify);
        }
        function onGone(conv, message) {
            if (message === "") root.removeConversation(conv);
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
                        text: "+"
                        color: Commons.Color.foreground
                        opacity: root.theme.dim
                        font.family: root.theme.family
                        font.pixelSize: root.theme.subtitle
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.newChatRequested()
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

        RowLayout {
            Layout.fillWidth: true
            Layout.leftMargin: root.theme.padX
            Layout.rightMargin: root.theme.padX
            Layout.topMargin: Style.space(4)
            Layout.bottomMargin: Style.space(4)
            spacing: root.theme.gap

            Repeater {
                model: [
                    { folder: "inbox", label: I18n.t("gmessages.folder_inbox", "Inbox") },
                    { folder: "archive", label: I18n.t("gmessages.folder_archived", "Archived") }
                ]

                Rectangle {
                    id: tab
                    required property var modelData
                    readonly property bool current: tab.modelData.folder === root.folder
                    implicitWidth: tabText.implicitWidth + Style.space(16)
                    implicitHeight: tabText.implicitHeight + Style.space(6)
                    radius: root.theme.radius
                    color: tab.current ? Util.alpha(Commons.Color.accent, 0.18) : "transparent"

                    Text {
                        id: tabText
                        anchors.centerIn: parent
                        textFormat: Text.PlainText
                        text: tab.modelData.label
                        color: Commons.Color.foreground
                        opacity: tab.current ? 1.0 : root.theme.dim
                        font.family: root.theme.family
                        font.pixelSize: root.theme.bodySmall
                        font.bold: tab.current
                    }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: root.setFolder(tab.modelData.folder)
                    }
                }
            }

            Text {
                Layout.fillWidth: true
                textFormat: Text.PlainText
                elide: Text.ElideRight
                horizontalAlignment: Text.AlignRight
                text: root.status
                color: Commons.Color.urgent
                font.family: root.theme.family
                font.pixelSize: root.theme.caption
            }
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
                    readonly property bool isBusy: modelData.id in root.busyRows
                    opacity: isBusy ? 0.4 : 1.0
                    color: isSelected
                        ? Util.alpha(Commons.Color.accent, 0.18)
                        : (isHighlighted ? Util.alpha(Commons.Color.foreground, 0.08) : "transparent")

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: root.theme.padX
                        anchors.rightMargin: root.theme.padX
                        spacing: root.theme.gap

                        Avatar {
                            Layout.alignment: Qt.AlignVCenter
                            theme: root.theme
                            size: Style.space(32)
                            path: root.avatars[rowDelegate.modelData.id] || ""
                            color: String(rowDelegate.modelData.color || "")
                            name: String(rowDelegate.modelData.name || "")
                            group: rowDelegate.modelData.group === true
                        }

                        // Unread dot
                        Rectangle {
                            Layout.alignment: Qt.AlignVCenter
                            width: Style.space(8)
                            height: Style.space(8)
                            radius: width / 2
                            color: (rowDelegate.modelData.unread && root.folder === "inbox") ? Commons.Color.accent : "transparent"
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

                    HoverHandler { id: rowHover }

                    MouseArea {
                        anchors.fill: parent
                        cursorShape: Qt.PointingHandCursor
                        onClicked: {
                            root.currentIndex = rowDelegate.index;
                            root.selectConversation(rowDelegate.modelData.id, rowDelegate.modelData.name || "");
                        }
                    }

                    // Row actions, on hover and on the current row; above the row's MouseArea.
                    Row {
                        visible: (rowHover.hovered || rowDelegate.isHighlighted) && !rowDelegate.isBusy
                        anchors.right: parent.right
                        anchors.rightMargin: root.theme.padX
                        anchors.bottom: parent.bottom
                        anchors.bottomMargin: Style.space(4)
                        spacing: Style.space(4)

                        Repeater {
                            model: [
                                { act: "archive", label: root.folder === "inbox" ? I18n.t("gmessages.archive", "Archive") : I18n.t("gmessages.restore", "Restore"), urgent: false },
                                { act: "delete", label: I18n.t("gmessages.delete", "Delete"), urgent: true }
                            ]

                            Rectangle {
                                id: actBtn
                                required property var modelData
                                width: actText.implicitWidth + Style.space(10)
                                height: actText.implicitHeight + Style.space(4)
                                radius: root.theme.radius
                                color: Commons.Color.popups.background
                                border.width: 1
                                border.color: actBtn.modelData.urgent ? Commons.Color.urgent : Commons.Color.popups.border

                                Text {
                                    id: actText
                                    anchors.centerIn: parent
                                    textFormat: Text.PlainText
                                    text: actBtn.modelData.label
                                    color: actBtn.modelData.urgent ? Commons.Color.urgent : Commons.Color.foreground
                                    font.family: root.theme.family
                                    font.pixelSize: root.theme.caption
                                }

                                MouseArea {
                                    anchors.fill: parent
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        var c = rowDelegate.modelData;
                                        if (actBtn.modelData.act === "archive") root.archiveOrRestore(c.id);
                                        else root.askDelete(c.id, String(c.name || ""));
                                    }
                                }
                            }
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
                    : (root.folder === "archive"
                        ? I18n.t("gmessages.no_archived", "No archived conversations")
                        : I18n.t("gmessages.no_conversations", "No conversations"))
                color: Commons.Color.muted
                font.family: root.theme.family
                font.pixelSize: root.theme.body
            }
        }
    }
}
