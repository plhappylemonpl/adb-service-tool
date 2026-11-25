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
    const val VERSION = "1.2"
}

/**
 * Stan przywracania i sesji Smart EI w SharedPreferences. Android zabija proces agenta, gdy
 * odbiera mu rolę domyślnej aplikacji SMS, i od razu wznawia ostatnią aktywność w nowym
 * procesie - dzięki temu stanowi nie wraca ona do ekranu startowego ani nie zgłasza błędu.
 */
object RestoreState {
    private const val PREFS = "agent_state"
    private const val MAX_AGE_MS = 6L * 60 * 60 * 1000

    private fun prefs(ctx: Context) = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    private fun fresh(ts: Long) = System.currentTimeMillis() - ts in 0..MAX_AGE_MS

    fun save(ctx: Context, session: String, state: JSONObject) {
        prefs(ctx).edit().putString("restore_session", session)
            .putString("restore_state", state.put("ts", System.currentTimeMillis()).toString()).commit()
    }

    fun load(ctx: Context, session: String): JSONObject? {
        val p = prefs(ctx)
        if (p.getString("restore_session", null) != session) return null
        val o = runCatching { JSONObject(p.getString("restore_state", "{}")!!) }.getOrNull() ?: return null
        return o.takeIf { fresh(it.optLong("ts")) }
    }

    fun saveLocalReport(ctx: Context, dir: String, report: String) {
        prefs(ctx).edit().putString("local_dir", dir).putString("local_report", report)
            .putLong("local_ts", System.currentTimeMillis()).commit()
    }

    fun localReport(ctx: Context, dir: String): String? {
        val p = prefs(ctx)
        if (p.getString("local_dir", null) != dir || !fresh(p.getLong("local_ts", 0))) return null
        return p.getString("local_report", null)
    }

    fun clearLocalReport(ctx: Context) {
        prefs(ctx).edit().remove("local_dir").remove("local_report").commit()
    }

    /** Smart EI: sesja (host:port:PIN) zakończona - po wznowieniu procesu pokaż ekran końcowy. */
    fun markSmartEiDone(ctx: Context, key: String, text: String, adb: Boolean) {
        prefs(ctx).edit().putString("sei_done_key", key).putString("sei_done_text", text)
            .putBoolean("sei_done_adb", adb).putLong("sei_done_ts", System.currentTimeMillis()).commit()
    }

    /** (tekst, adb) gdy sesja `key` (albo dowolna, gdy key == null) zakończyła się niedawno. */
    fun smartEiDone(ctx: Context, key: String?): Pair<String, Boolean>? {
        val p = prefs(ctx)
        val k = p.getString("sei_done_key", null) ?: return null
        val ts = p.getLong("sei_done_ts", 0)
        val recent = if (key == null) System.currentTimeMillis() - ts in 0..30L * 60 * 1000 else fresh(ts)
        if ((key != null && k != key) || !recent) return null
        return (p.getString("sei_done_text", "") ?: "") to p.getBoolean("sei_done_adb", false)
    }

    fun clearSmartEi(ctx: Context) {
        prefs(ctx).edit().remove("sei_done_key").remove("sei_done_text").remove("sei_done_ts").commit()
    }
}
