package com.drapco.agent

import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.io.InputStream
import java.net.HttpURLConnection
import java.net.URL
import java.net.URLEncoder

/** Komputer anulował sesję (HTTP 410). */
class SmartEiCancelled : Exception("cancelled")

/** Odpowiedź HTTP z kodem błędu (parowanie: 403 zły PIN, 409 zajęte, 423 zablokowane). */
class SmartEiHttpError(val code: Int, message: String) : IOException(message)

/**
 * Klient mostka Smart EI na komputerze (HTTP w sieci lokalnej albo 127.0.0.1 przez `adb reverse`).
 * Bez bibliotek: HttpURLConnection + org.json. Po parowaniu każde zapytanie niesie
 * jednorazowy token sesji w nagłówku X-SmartEI-Token.
 */
class SmartEiClient(val host: String, val port: Int) {
    @Volatile
    var token: String? = null

    private fun url(path: String) = URL("http://$host:$port$path")

    private fun open(path: String, method: String, timeoutMs: Int = 60_000): HttpURLConnection {
        val c = url(path).openConnection() as HttpURLConnection
        c.requestMethod = method
        c.connectTimeout = 8_000
        c.readTimeout = timeoutMs
        c.useCaches = false
        c.instanceFollowRedirects = false
        c.setRequestProperty("Connection", "close")
        token?.let { c.setRequestProperty("X-SmartEI-Token", it) }
        return c
    }

    private fun readBody(c: HttpURLConnection): JSONObject {
        val code = c.responseCode
        val stream = if (code in 200..299) c.inputStream else c.errorStream
        val txt = stream?.use { it.readBytes().toString(Charsets.UTF_8) }.orEmpty()
        c.disconnect()
        if (code == 410) throw SmartEiCancelled()
        if (code !in 200..299) {
            val msg = runCatching { JSONObject(txt).optString("error") }.getOrNull().orEmpty()
            throw SmartEiHttpError(code, msg.ifEmpty { "HTTP $code" })
        }
        return if (txt.isBlank()) JSONObject() else JSONObject(txt)
    }

    fun get(path: String): JSONObject = readBody(open(path, "GET"))

    fun post(path: String, body: JSONObject = JSONObject()): JSONObject {
        val bytes = body.toString().toByteArray(Charsets.UTF_8)
        val c = open(path, "POST")
        c.doOutput = true
        c.setFixedLengthStreamingMode(bytes.size)
        c.setRequestProperty("Content-Type", "application/json; charset=utf-8")
        c.outputStream.use { it.write(bytes) }
        return readBody(c)
    }

    /** Parowanie PINem -> token. Zwraca odpowiedź (role, adb). */
    fun pair(pin: String, device: JSONObject): JSONObject {
        token = null
        val r = post("/pair", JSONObject().put("pin", pin).put("device", device))
        token = r.getString("token")
        return r
    }

    /** Wysyła plik strumieniowo (stała długość - bez ładowania do pamięci). */
    fun upload(rel: String, size: Long, input: InputStream, onBytes: (Long) -> Unit = {}): JSONObject {
        val c = open("/export/file?path=${enc(rel)}&size=$size", "PUT", 120_000)
        c.doOutput = true
        c.setFixedLengthStreamingMode(size)
        c.setRequestProperty("Content-Type", "application/octet-stream")
        c.outputStream.use { out ->
            val buf = ByteArray(256 * 1024)
            var left = size
            while (left > 0) {
                val n = input.read(buf, 0, minOf(buf.size.toLong(), left).toInt())
                if (n < 0) throw IOException("file shorter than declared")
                out.write(buf, 0, n)
                left -= n
                onBytes(n.toLong())
            }
        }
        return readBody(c)
    }

    fun uploadFile(rel: String, f: File, onBytes: (Long) -> Unit = {}): Boolean {
        val size = f.length()
        if (has(rel, size)) return false
        f.inputStream().use { upload(rel, size, it, onBytes) }
        return true
    }

    /** Czy komputer ma już ten plik (wznowienie przerwanej kopii). */
    fun has(rel: String, size: Long): Boolean =
        get("/export/has?path=${enc(rel)}&size=$size").optBoolean("have", false)

    /** Pobiera plik z planu importu do `dest` (przez .part + kontrola rozmiaru). */
    fun download(rel: String, size: Long, dest: File, onBytes: (Long) -> Unit = {}) {
        val c = open("/import/file?path=${enc(rel)}", "GET", 120_000)
        val code = c.responseCode
        if (code == 410) throw SmartEiCancelled()
        if (code != 200) {
            c.disconnect()
            throw SmartEiHttpError(code, "HTTP $code: $rel")
        }
        dest.parentFile?.mkdirs()
        val part = File(dest.path + ".part")
        var got = 0L
        c.inputStream.use { input ->
            FileOutputStream(part).use { out ->
                val buf = ByteArray(256 * 1024)
                while (true) {
                    val n = input.read(buf)
                    if (n < 0) break
                    out.write(buf, 0, n)
                    got += n
                    onBytes(n.toLong())
                }
                out.fd.sync()
            }
        }
        c.disconnect()
        if (size >= 0 && got != size) {
            part.delete()
            throw IOException("$rel: $got / $size B")
        }
        if (dest.exists()) dest.delete()
        if (!part.renameTo(dest)) {
            part.copyTo(dest, overwrite = true)
            part.delete()
        }
    }

    companion object {
        fun enc(s: String): String = URLEncoder.encode(s, "UTF-8").replace("+", "%20")

        /** "192.168.1.10:50123" -> (host, port); null gdy niepoprawne. */
        fun parseAddress(raw: String): Pair<String, Int>? {
            val t = raw.trim().removePrefix("http://").trimEnd('/')
            val host = t.substringBeforeLast(':', "")
            val port = t.substringAfterLast(':', "").toIntOrNull() ?: return null
            val ok = host.isNotEmpty() && port in 1..65535 &&
                host.all { it.isDigit() || it == '.' } && host.split('.').size == 4 &&
                host.split('.').all { p -> p.toIntOrNull()?.let { it in 0..255 } == true }
            return if (ok) host to port else null
        }
    }
}
