.pragma library

var messages = {
    // Error codes
    "not_paired": "Not paired with a phone.",
    "locked": "Unlock your keyring",
    "phone_offline": "Phone appears to be offline.",
    "send_rejected": "Message was rejected by the phone.",
    "send_uncertain": "may not have been sent",
    "not_found": "Conversation or message not found.",
    "bad_request": "Invalid request.",
    "unknown_method": "Unsupported operation.",
    "protocol_mismatch": "Daemon protocol mismatch. Please restart the service.",
    "busy": "Another operation is already in progress.",
    "timeout": "Request timed out.",
    "internal": "Internal daemon error.",
    "disconnected": "Disconnected from the background service.",

    // State details & states
    "starting": "Starting service…",
    "keyring_missing": "Keyring service is unavailable.",
    "store_unreadable": "Message store could not be opened.",
    "login": "Sign in or pair your phone to get started.",
    "pairing": "Pairing with your phone…",
    "connecting": "Connecting to Google Messages…",
    "ready": "Connected",
    "expired": "Session expired. Please pair again.",
    "fatal": "The background service encountered a fatal error.",
    "empty_passphrase": "Store passphrase is empty.",
    "keyring_locked": "Unlock your keyring",
    "keyring_refused": "The keyring refused to store the key.",
    "unpaired_remotely": "Unpaired from your phone.",
    "cancelled": "Pairing cancelled."
};

function format(I18n, code, fallback) {
    var key = String(code || "internal");
    var en = messages[key] || fallback || key;
    if (I18n && typeof I18n.t === "function") {
        return I18n.t("gmessages." + key, en);
    }
    return en;
}

function formatError(I18n, err) {
    if (!err) return format(I18n, "internal", "Unknown error");
    if (typeof err === "string") return format(I18n, err, err);
    // The daemon's own texts for internal errors (too large, no Downloads folder…) are fit to show.
    if ((err.code || "internal") === "internal" && err.message) return String(err.message);
    return format(I18n, err.code || "internal", err.message || "Unknown error");
}
