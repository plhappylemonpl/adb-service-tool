package com.drapco.agent

import android.content.ContentProviderOperation
import android.content.ContentValues
import android.content.Context
import android.os.Environment
import android.provider.ContactsContract
import android.provider.ContactsContract.CommonDataKinds
import android.provider.Telephony
import android.util.Xml
import org.xmlpull.v1.XmlPullParser
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileInputStream
import java.nio.charset.Charset

/**
 * Pliki przywracania wgrane przez komputer: Download/Restore/<sesja>/contacts.vcf i sms.xml.
 * (Komputer nadaje agentowi dostęp do plików: appops MANAGE_EXTERNAL_STORAGE na Androidzie 11+,
 * READ_EXTERNAL_STORAGE na 6-10.)
 */
class RestoreFiles(val dir: File) {
    val contacts: File? = pick("contacts.vcf", ".vcf")
    val sms: File? = pick("sms.xml", ".xml")

    private fun pick(preferred: String, ext: String): File? {
        val exact = File(dir, preferred)
        if (exact.isFile && exact.canRead()) return exact
        return dir.listFiles()
            ?.filter { it.isFile && it.canRead() && it.name.lowercase().endsWith(ext) && !it.name.startsWith(".") }
            ?.minByOrNull { it.name }
    }

    /** Czy folder w ogóle da się czytać (brak dostępu = listFiles() == null). */
    fun readable(): Boolean = dir.isDirectory && dir.listFiles() != null

    companion object {
        @Suppress("DEPRECATION")
        fun forSession(session: String): RestoreFiles =
            RestoreFiles(
                File(
                    Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS),
                    "${Storage.RESTORE}/$session"
                )
            )
    }
}

// --------------------------------------------------------------------------- //
// vCard
// --------------------------------------------------------------------------- //

/** Minimalny parser vCard 2.1 / 3.0 / 4.0 (to, co eksportują Android, iOS, Outlook i nasz agent). */
object Vcf {
    data class Prop(val name: String, val params: List<String>, val value: String) {
        fun types(): List<String> = params.flatMap { p ->
            val v = if (p.contains('=')) {
                if (p.substringBefore('=').equals("TYPE", true)) p.substringAfter('=') else ""
            } else p
            v.split(',').map { it.trim().trim('"').uppercase() }.filter { it.isNotEmpty() }
        }
    }

    class Card(val props: List<Prop>) {
        fun all(name: String) = props.filter { it.name == name }
        fun first(name: String) = props.firstOrNull { it.name == name }
    }

    fun count(f: File): Int {
        var n = 0
        f.bufferedReader(Charsets.UTF_8).useLines { lines ->
            lines.forEach { if (it.trim().equals("BEGIN:VCARD", true)) n++ }
        }
        return n
    }

    fun parse(f: File): List<Card> {
        val cards = mutableListOf<Card>()
        var cur: MutableList<Prop>? = null
        for (line in unfold(f)) {
            val p = parseLine(line) ?: continue
            when {
                p.name == "BEGIN" && p.value.equals("VCARD", true) -> cur = mutableListOf()
                p.name == "END" && p.value.equals("VCARD", true) -> {
                    cur?.let { cards += Card(it) }
                    cur = null
                }
                else -> cur?.add(p)
            }
        }
        return cards
    }

    /** Łączy linie zawinięte (spacja/tab na początku) i miękkie łamania QUOTED-PRINTABLE ("=" na końcu). */
    private fun unfold(f: File): List<String> {
        val out = mutableListOf<String>()
        f.bufferedReader(Charsets.UTF_8).useLines { lines ->
            for (raw in lines) {
                val l = raw.trimEnd('\r')
                val last = out.lastOrNull()
                if (last != null && (l.startsWith(" ") || l.startsWith("\t"))) {
                    out[out.size - 1] = last + l.substring(1)
                } else if (last != null && isQp(last) && last.endsWith("=")) {
                    out[out.size - 1] = last.dropLast(1) + l
                } else if (l.isNotEmpty()) {
                    out += l
                }
            }
        }
        return out
    }

    private fun isQp(line: String): Boolean =
        line.substringBefore(':').uppercase().contains("QUOTED-PRINTABLE")

    private fun parseLine(line: String): Prop? {
        val colon = indexOfUnquoted(line, ':')
        if (colon <= 0) return null
        val head = line.substring(0, colon).split(';')
        var name = head[0].trim().uppercase()
        if (name.contains('.')) name = name.substringAfterLast('.') // item1.TEL
        val params = head.drop(1)
        var value = line.substring(colon + 1)
        val qp = params.any { it.uppercase().contains("QUOTED-PRINTABLE") }
        val charset = params.firstOrNull { it.uppercase().startsWith("CHARSET=") }?.substringAfter('=')
        if (qp) value = decodeQp(value, charset)
        return Prop(name, params, value)
    }

    private fun indexOfUnquoted(s: String, c: Char): Int {
        var q = false
        for (i in s.indices) {
            if (s[i] == '"') q = !q else if (s[i] == c && !q) return i
        }
        return -1
    }

    private fun decodeQp(v: String, charset: String?): String {
        val bytes = ByteArrayOutputStream()
        var i = 0
        while (i < v.length) {
            val ch = v[i]
            if (ch == '=' && i + 3 <= v.length) {
                val b = v.substring(i + 1, i + 3).toIntOrNull(16)
                if (b != null) {
                    bytes.write(b)
                    i += 3
                    continue
                }
            }
            bytes.write(ch.toString().toByteArray(Charsets.UTF_8))
            i++
        }
        val cs = try {
            Charset.forName(charset ?: "UTF-8")
        } catch (_: Throwable) {
            Charsets.UTF_8
        }
        return String(bytes.toByteArray(), cs)
    }

    /** Dzieli wartość po nieescapowanym ';' i usuwa escapowanie vCard 3.0. */
    fun components(v: String): List<String> {
        val parts = mutableListOf<String>()
        val sb = StringBuilder()
        var i = 0
        while (i < v.length) {
            val ch = v[i]
            if (ch == '\\' && i + 1 < v.length) {
                sb.append(ch).append(v[i + 1])
                i += 2
                continue
            }
            if (ch == ';') {
                parts += unescape(sb.toString())
                sb.setLength(0)
            } else sb.append(ch)
            i++
        }
        parts += unescape(sb.toString())
        return parts
    }

    fun unescape(v: String): String {
        val sb = StringBuilder()
        var i = 0
        while (i < v.length) {
            val ch = v[i]
            if (ch == '\\' && i + 1 < v.length) {
                when (val n = v[i + 1]) {
                    'n', 'N' -> sb.append('\n')
                    else -> sb.append(n)
                }
                i += 2
            } else {
                sb.append(ch)
                i++
            }
        }
        return sb.toString().trim()
    }
}

/**
 * Import kontaktów bezpośrednio do bazy (WRITE_CONTACTS) - używany tylko, gdy na
 * urządzeniu nie ma aplikacji obsługującej import .vcf (ACTION_VIEW text/x-vcard).
 * Kontakty trafiają na konto lokalne urządzenia.
 */
class VcfImporter(private val ctx: Context, private val progress: (Int, Int) -> Unit) {

    fun import(f: File): Int {
        val cards = Vcf.parse(f)
        val ops = ArrayList<ContentProviderOperation>()
        var done = 0
        var imported = 0
        for (card in cards) {
            val before = ops.size
            if (addCard(card, ops)) imported++ else while (ops.size > before) ops.removeAt(ops.size - 1)
            done++
            if (ops.size >= 300) {
                ctx.contentResolver.applyBatch(ContactsContract.AUTHORITY, ops)
                ops.clear()
            }
            if (done % 10 == 0 || done == cards.size) progress(done, cards.size)
        }
        if (ops.isNotEmpty()) ctx.contentResolver.applyBatch(ContactsContract.AUTHORITY, ops)
        return imported
    }

    private fun addCard(card: Vcf.Card, ops: ArrayList<ContentProviderOperation>): Boolean {
        val fn = card.first("FN")?.let { Vcf.unescape(it.value) }.orEmpty()
        val n = card.first("N")?.let { Vcf.components(it.value) } ?: emptyList()
        val phones = card.all("TEL").filter { it.value.isNotBlank() }
        val emails = card.all("EMAIL").filter { it.value.isNotBlank() }
        val display = fn.ifEmpty { listOf(n.getOrNull(3), n.getOrNull(1), n.getOrNull(2), n.getOrNull(0), n.getOrNull(4))
            .filter { !it.isNullOrBlank() }.joinToString(" ") }
        if (display.isEmpty() && phones.isEmpty() && emails.isEmpty()) return false

        val back = ops.size
        ops += ContentProviderOperation.newInsert(ContactsContract.RawContacts.CONTENT_URI)
            .withValue(ContactsContract.RawContacts.ACCOUNT_TYPE, null)
            .withValue(ContactsContract.RawContacts.ACCOUNT_NAME, null)
            .build()

        fun data(mime: String, vararg kv: Pair<String, Any?>) {
            val b = ContentProviderOperation.newInsert(ContactsContract.Data.CONTENT_URI)
                .withValueBackReference(ContactsContract.Data.RAW_CONTACT_ID, back)
                .withValue(ContactsContract.Data.MIMETYPE, mime)
            for ((k, v) in kv) if (v != null && v.toString().isNotEmpty()) b.withValue(k, v)
            ops += b.build()
        }

        data(
            CommonDataKinds.StructuredName.CONTENT_ITEM_TYPE,
            CommonDataKinds.StructuredName.DISPLAY_NAME to display.ifEmpty { null },
            CommonDataKinds.StructuredName.FAMILY_NAME to n.getOrNull(0),
            CommonDataKinds.StructuredName.GIVEN_NAME to n.getOrNull(1),
            CommonDataKinds.StructuredName.MIDDLE_NAME to n.getOrNull(2),
            CommonDataKinds.StructuredName.PREFIX to n.getOrNull(3),
            CommonDataKinds.StructuredName.SUFFIX to n.getOrNull(4)
        )
        for (p in phones) {
            val t = p.types()
            val type = when {
                "CELL" in t -> CommonDataKinds.Phone.TYPE_MOBILE
                "FAX" in t && "WORK" in t -> CommonDataKinds.Phone.TYPE_FAX_WORK
                "FAX" in t -> CommonDataKinds.Phone.TYPE_FAX_HOME
                "WORK" in t -> CommonDataKinds.Phone.TYPE_WORK
                "HOME" in t -> CommonDataKinds.Phone.TYPE_HOME
                "PAGER" in t -> CommonDataKinds.Phone.TYPE_PAGER
                else -> CommonDataKinds.Phone.TYPE_OTHER
            }
            data(
                CommonDataKinds.Phone.CONTENT_ITEM_TYPE,
                CommonDataKinds.Phone.NUMBER to Vcf.unescape(p.value).removePrefix("tel:"),
                CommonDataKinds.Phone.TYPE to type
            )
        }
        for (e in emails) {
            val t = e.types()
            val type = when {
                "WORK" in t -> CommonDataKinds.Email.TYPE_WORK
                "HOME" in t -> CommonDataKinds.Email.TYPE_HOME
                else -> CommonDataKinds.Email.TYPE_OTHER
            }
            data(
                CommonDataKinds.Email.CONTENT_ITEM_TYPE,
                CommonDataKinds.Email.ADDRESS to Vcf.unescape(e.value),
                CommonDataKinds.Email.TYPE to type
            )
        }
        val org = card.first("ORG")?.let { Vcf.components(it.value).filter { s -> s.isNotEmpty() }.joinToString(", ") }
        val title = card.first("TITLE")?.let { Vcf.unescape(it.value) }
        if (!org.isNullOrEmpty() || !title.isNullOrEmpty()) {
            data(
                CommonDataKinds.Organization.CONTENT_ITEM_TYPE,
                CommonDataKinds.Organization.COMPANY to org,
                CommonDataKinds.Organization.TITLE to title,
                CommonDataKinds.Organization.TYPE to CommonDataKinds.Organization.TYPE_WORK
            )
        }
        for (a in card.all("ADR")) {
            val c = Vcf.components(a.value) // pobox;ext;street;city;region;postcode;country
            if (c.all { it.isEmpty() }) continue
            data(
                CommonDataKinds.StructuredPostal.CONTENT_ITEM_TYPE,
                CommonDataKinds.StructuredPostal.POBOX to c.getOrNull(0),
                CommonDataKinds.StructuredPostal.STREET to listOfNotNull(c.getOrNull(2), c.getOrNull(1))
                    .filter { it.isNotEmpty() }.joinToString(" "),
                CommonDataKinds.StructuredPostal.CITY to c.getOrNull(3),
                CommonDataKinds.StructuredPostal.REGION to c.getOrNull(4),
                CommonDataKinds.StructuredPostal.POSTCODE to c.getOrNull(5),
                CommonDataKinds.StructuredPostal.COUNTRY to c.getOrNull(6),
                CommonDataKinds.StructuredPostal.TYPE to
                    if ("WORK" in a.types()) CommonDataKinds.StructuredPostal.TYPE_WORK
                    else CommonDataKinds.StructuredPostal.TYPE_HOME
            )
        }
        card.first("NOTE")?.let {
            data(CommonDataKinds.Note.CONTENT_ITEM_TYPE, CommonDataKinds.Note.NOTE to Vcf.unescape(it.value))
        }
        card.first("BDAY")?.let {
            data(
                CommonDataKinds.Event.CONTENT_ITEM_TYPE,
                CommonDataKinds.Event.START_DATE to Vcf.unescape(it.value),
                CommonDataKinds.Event.TYPE to CommonDataKinds.Event.TYPE_BIRTHDAY
            )
        }
        for (u in card.all("URL")) {
            data(CommonDataKinds.Website.CONTENT_ITEM_TYPE, CommonDataKinds.Website.URL to Vcf.unescape(u.value))
        }
        return true
    }
}

// --------------------------------------------------------------------------- //
// SMS
// --------------------------------------------------------------------------- //

/**
 * Przywracanie SMS z pliku w formacie "SMS Backup & Restore" (taki zapisuje nasz agent).
 * Wymaga, żeby agent był DOMYŚLNĄ aplikacją SMS (tylko ona może pisać do content://sms).
 * Duplikaty (ten sam typ, data, numer i treść) są pomijane - przywracanie można powtórzyć.
 * Wiadomości MMS są liczone i pomijane.
 */
class SmsRestorer(private val ctx: Context, private val progress: (Int, Int) -> Unit) {

    data class Counts(val sms: Int, val mms: Int)

    data class Result(val total: Int, val inserted: Int, val skipped: Int, val failed: Int, val mms: Int)

    fun count(f: File): Counts {
        var sms = 0
        var mms = 0
        parse(f) { tag, _ ->
            if (tag == "sms") sms++ else if (tag == "mms") mms++
        }
        return Counts(sms, mms)
    }

    fun restore(f: File, total: Int): Result {
        val existing = existingKeys()
        val ops = ArrayList<ContentProviderOperation>()
        var done = 0
        var inserted = 0
        var skipped = 0
        var failed = 0
        var mms = 0

        fun flush() {
            if (ops.isEmpty()) return
            val n = ops.size
            try {
                val res = ctx.contentResolver.applyBatch("sms", ops)
                val ok = res.count { it.uri != null }
                inserted += ok
                failed += n - ok
            } catch (_: Throwable) {
                // pojedynczo - jedna zła wiadomość nie blokuje reszty
                for (op in ops) {
                    try {
                        val r = ctx.contentResolver.applyBatch("sms", arrayListOf(op))
                        if (r.firstOrNull()?.uri != null) inserted++ else failed++
                    } catch (_: Throwable) {
                        failed++
                    }
                }
            }
            ops.clear()
        }

        parse(f) { tag, a ->
            if (tag == "mms") {
                mms++
                return@parse
            }
            if (tag != "sms") return@parse
            done++
            val v = values(a)
            if (v == null) {
                failed++
            } else {
                val key = key(v.getAsInteger(Telephony.Sms.TYPE), v.getAsLong(Telephony.Sms.DATE),
                    v.getAsString(Telephony.Sms.ADDRESS), v.getAsString(Telephony.Sms.BODY))
                if (!existing.add(key)) {
                    skipped++
                } else {
                    ops += ContentProviderOperation.newInsert(Telephony.Sms.CONTENT_URI).withValues(v).build()
                    if (ops.size >= 200) flush()
                }
            }
            if (done % 50 == 0 || done == total) progress(done, total)
        }
        flush()
        progress(done, maxOf(total, done))
        return Result(done, inserted, skipped, failed, mms)
    }

    private fun values(a: Map<String, String>): ContentValues? {
        fun s(k: String): String? = a[k]?.takeIf { it.isNotEmpty() && it != "null" }
        val address = s("address") ?: return null
        val date = s("date")?.toLongOrNull() ?: return null
        val type = s("type")?.toIntOrNull()?.takeIf { it in 1..6 } ?: Telephony.Sms.MESSAGE_TYPE_INBOX
        return ContentValues().apply {
            put(Telephony.Sms.ADDRESS, address)
            put(Telephony.Sms.BODY, a["body"].orEmpty())
            put(Telephony.Sms.DATE, date)
            s("date_sent")?.toLongOrNull()?.let { put(Telephony.Sms.DATE_SENT, it) }
            put(Telephony.Sms.TYPE, type)
            put(Telephony.Sms.READ, s("read")?.toIntOrNull() ?: 1)
            put(Telephony.Sms.SEEN, 1) // bez lawiny powiadomień o "nowych" wiadomościach
            s("status")?.toIntOrNull()?.let { put(Telephony.Sms.STATUS, it) }
            s("locked")?.toIntOrNull()?.let { put(Telephony.Sms.LOCKED, it) }
            s("protocol")?.toIntOrNull()?.let { put(Telephony.Sms.PROTOCOL, it) }
            s("subject")?.let { put(Telephony.Sms.SUBJECT, it) }
            s("service_center")?.let { put(Telephony.Sms.SERVICE_CENTER, it) }
        }
    }

    private fun key(type: Int?, date: Long?, address: String?, body: String?): String =
        "${type ?: 0}|${date ?: 0}|${address.orEmpty()}|${body.orEmpty().length}|${body.orEmpty().hashCode()}"

    /** Klucze wiadomości już obecnych w telefonie (brak READ_SMS = brak deduplikacji). */
    private fun existingKeys(): HashSet<String> {
        val set = HashSet<String>()
        try {
            ctx.contentResolver.query(
                Telephony.Sms.CONTENT_URI,
                arrayOf(Telephony.Sms.TYPE, Telephony.Sms.DATE, Telephony.Sms.ADDRESS, Telephony.Sms.BODY),
                null, null, null
            )?.use { c ->
                while (c.moveToNext()) {
                    set += key(c.getInt(0), c.getLong(1), c.getString(2), c.getString(3))
                }
            }
        } catch (_: Throwable) {
        }
        return set
    }

    /** Strumieniowe czytanie XML (plik może mieć dziesiątki MB). */
    private fun parse(f: File, onTag: (String, Map<String, String>) -> Unit) {
        FileInputStream(f).use { input ->
            val p = Xml.newPullParser()
            p.setFeature(XmlPullParser.FEATURE_PROCESS_NAMESPACES, false)
            p.setInput(input, null)
            var ev = p.eventType
            while (ev != XmlPullParser.END_DOCUMENT) {
                if (ev == XmlPullParser.START_TAG && (p.name == "sms" || p.name == "mms")) {
                    val m = HashMap<String, String>(p.attributeCount * 2)
                    if (p.name == "sms") for (i in 0 until p.attributeCount) m[p.getAttributeName(i)] = p.getAttributeValue(i)
                    onTag(p.name, m)
                    if (p.name == "mms") skipSubtree(p)
                }
                ev = p.next()
            }
        }
    }

    private fun skipSubtree(p: XmlPullParser) {
        var depth = 1
        while (depth > 0) {
            when (p.next()) {
                XmlPullParser.START_TAG -> depth++
                XmlPullParser.END_TAG -> depth--
                XmlPullParser.END_DOCUMENT -> return
            }
        }
    }
}
