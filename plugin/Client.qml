pragma ComponentBehavior: Bound
import QtQuick
import Quickshell
import Quickshell.Io

Item {
    id: root

    property string role: "app"
    readonly property string socketPath: Quickshell.env("XDG_RUNTIME_DIR") + "/gmessages-tui/daemon.sock"
    readonly property bool connected: sock.connected
    property string state: "starting"
    property var detail: null
    property int unread: 0
    property bool mismatch: false

    signal pairing(var data)
    signal conversationChanged(var c)
    signal historyArrived(string conv, var msgs)
    signal messageArrived(string conv, var msg, bool notify)
    signal missed(int count)
    signal typing(string conversation, string name, bool started)
    signal avatarsArrived(var items)
    // After every handshake: state is re-read, so views reload what they missed.
    signal resynced()

    property int seq: 0
    property var pending: ({})
    property int delay: 500

    Component {
        id: timeoutTimer
        Timer {
            property int reqId: -1
            repeat: false
            onTriggered: root.expire(reqId)
        }
    }

    function expire(id) {
        if (!(id in root.pending)) return;
        var entry = root.pending[id];
        delete root.pending[id];
        if (entry.timer) entry.timer.destroy();
        if (entry.cb) entry.cb({ id: id, ok: false, error: { code: "timeout", message: "timeout" } });
    }

    function clearPending(code) {
        var p = root.pending;
        root.pending = ({});
        for (var id in p) {
            var entry = p[id];
            if (entry.timer) entry.timer.destroy();
            if (entry.cb) entry.cb({ id: Number(id), ok: false, error: { code: code, message: code } });
        }
    }

    function send(method, params, cb) {
        if (!sock.connected) {
            if (cb) cb({ ok: false, error: { code: "disconnected", message: "disconnected" } });
            return;
        }
        if (root.mismatch && method !== "hello") {
            if (cb) cb({ ok: false, error: { code: "protocol_mismatch", message: "protocol_mismatch" } });
            return;
        }
        root.seq += 1;
        var id = root.seq;
        var ms = method === "send" ? 125000 : (method.indexOf("media.") === 0 ? 65000 : 20000);
        var t = timeoutTimer.createObject(root, { reqId: id, interval: ms, running: true });
        root.pending[id] = { cb: cb || null, timer: t };
        sock.write(JSON.stringify({ id: id, method: method, params: params || ({}) }) + "\n");
        sock.flush();
    }

    function sendHello() {
        var t = timeoutTimer.createObject(root, { reqId: 0, interval: 20000, running: true });
        root.pending[0] = {
            cb: function(r) {
                if (!r.ok && r.error && r.error.code === "protocol_mismatch") {
                    root.mismatch = true;
                }
                if (!r.ok) return;
                root.send("state", {}, function(s) {
                    if (s.ok && s.data) {
                        root.state = String(s.data.state);
                        root.detail = s.data.detail !== undefined ? s.data.detail : null;
                        root.unread = Number(s.data.unread) || 0;
                    }
                    root.resynced();
                });
            },
            timer: t
        };
        sock.write(JSON.stringify({ id: 0, method: "hello", params: { protocol: 1, role: root.role } }) + "\n");
        sock.flush();
    }

    function onEvent(ev, d) {
        d = d || ({});
        switch (ev) {
        case "hello":
            if (Number(d.protocol) !== 1) {
                root.mismatch = true;
                return;
            }
            root.mismatch = false;
            if (d.state !== undefined) root.state = String(d.state);
            root.detail = d.detail !== undefined ? d.detail : null;
            if (d.unread !== undefined) root.unread = Number(d.unread) || 0;
            break;
        case "state":
            if (d.state !== undefined) root.state = String(d.state);
            root.detail = d.detail !== undefined ? d.detail : null;
            break;
        case "pairing":
            root.pairing(d);
            break;
        case "conversation":
            if (d.conversation) root.conversationChanged(d.conversation);
            break;
        case "history":
            root.historyArrived(String(d.conversation || ""), Array.isArray(d.messages) ? d.messages : []);
            break;
        case "message":
            if (d.message) root.messageArrived(String(d.conversation || ""), d.message, d.notify === true);
            break;
        case "missed":
            root.missed(Number(d.count) || 0);
            break;
        case "unread":
            root.unread = Number(d.count) || 0;
            break;
        case "avatars":
            root.avatarsArrived(d.items || ({}));
            break;
        case "typing":
            root.typing(String(d.conversation || ""), String(d.name || ""), d.started === true);
            break;
        }
    }

    function onLine(line) {
        var msg;
        try { msg = JSON.parse(line); } catch (e) { return; }
        if (msg.event !== undefined) {
            root.onEvent(String(msg.event), msg.data);
            return;
        }
        if (msg.ok === false && msg.error && msg.error.code === "protocol_mismatch") {
            root.mismatch = true;
        }
        if (msg.id === undefined || msg.id === null || !(msg.id in root.pending)) return;
        var entry = root.pending[msg.id];
        delete root.pending[msg.id];
        if (entry.timer) entry.timer.destroy();
        if (entry.cb) entry.cb(msg);
    }

    Socket {
        id: sock
        path: root.socketPath
        parser: SplitParser {
            splitMarker: "\n"
            onRead: function(data) { root.onLine(data); }
        }
        onConnectionStateChanged: {
            if (connected) {
                root.delay = 500;
                retry.stop();
                root.mismatch = false;
                root.sendHello();
            } else {
                root.clearPending("disconnected");
                retry.interval = root.delay;
                root.delay = Math.min(10000, root.delay * 2);
                retry.restart();
            }
        }
    }

    Timer {
        id: retry
        repeat: false
        onTriggered: {
            sock.connected = false;
            sock.connected = true;
        }
    }

    Component.onCompleted: sock.connected = true
}
