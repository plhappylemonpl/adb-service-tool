package com.drapco.agent

import android.content.Context
import android.os.Build
import org.json.JSONArray
import org.json.JSONObject

/**
 * Znaczniki dla komputera (zapisywane OSTATNIE, po zamknięciu plików danych):
 *  - done.json  : kopia zakończona, lista plików z rozmiarami (komputer je weryfikuje),
 *  - error.json : kopia nieudana, czytelny komunikat.
 * Przywracanie (Download/Restore/<sesja>/):
 *  - restore_done.json  : użytkownik kliknął Gotowe - podsumowanie,
 *  - restore_error.json : przywracanie niemożliwe (np. brak dostępu do plików).
 */
object Markers {

    fun writeDone(ctx: Context, session: String, r: Exporter.Result) {
        val files = JSONArray()
        for (f in r.files) {
            files.put(JSONObject().put("name", f.name).put("size", f.bytes))
        }
        val json = JSONObject()
            .put("status", "ok")
            .put("session", session)
            .put("contacts", r.contacts)
            .put("sms", r.sms)
            .put("contactsDenied", r.contactsDenied)
            .put("smsDenied", r.smsDenied)
            .put("files", files)
            .put("device", "${Build.MANUFACTURER} ${Build.MODEL}")
            .put("sdk", Build.VERSION.SDK_INT)
            .put("agentVersion", BuildInfo.VERSION)
            .put("finishedAt", System.currentTimeMillis())
        Storage.write(ctx, session, "done.json", "application/json") { Storage.write(it, json.toString(2)) }
    }

    fun writeError(ctx: Context, session: String, message: String) {
        val json = JSONObject()
            .put("status", "error")
            .put("session", session)
            .put("error", message)
            .put("sdk", Build.VERSION.SDK_INT)
            .put("agentVersion", BuildInfo.VERSION)
        Storage.write(ctx, session, "error.json", "application/json") { Storage.write(it, json.toString(2)) }
    }
}

object RestoreMarkers {
    fun writeDone(ctx: Context, session: String, json: JSONObject) {
        json.put("status", "ok")
            .put("session", session)
            .put("device", "${Build.MANUFACTURER} ${Build.MODEL}")
            .put("sdk", Build.VERSION.SDK_INT)
            .put("agentVersion", BuildInfo.VERSION)
            .put("finishedAt", System.currentTimeMillis())
        Storage.write(ctx, session, "restore_done.json", "application/json", Storage.RESTORE) {
            Storage.write(it, json.toString(2))
        }
    }

    fun writeError(ctx: Context, session: String, message: String) {
        val json = JSONObject()
            .put("status", "error")
            .put("session", session)
            .put("error", message)
            .put("sdk", Build.VERSION.SDK_INT)
            .put("agentVersion", BuildInfo.VERSION)
        Storage.write(ctx, session, "restore_error.json", "application/json", Storage.RESTORE) {
            Storage.write(it, json.toString(2))
        }
    }
}

object BuildInfo {
    const val VERSION = "1.1"
}
