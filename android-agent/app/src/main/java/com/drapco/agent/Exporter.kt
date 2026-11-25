package com.drapco.agent

import android.Manifest
import android.content.ContentResolver
import android.content.Context
import android.content.pm.PackageManager
import android.database.Cursor
import android.net.Uri
import android.os.Build
import android.provider.ContactsContract
import android.provider.ContactsContract.CommonDataKinds
import java.io.BufferedWriter
import java.io.OutputStream
import java.io.OutputStreamWriter
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * Eksport danych (wywoływany wyłącznie z przycisku "Wykonaj kopię").
 *  - contacts.vcf : vCard generowany przez system (ContactsContract.CONTENT_VCARD_URI),
 *                   awaryjnie własny vCard 3.0 z tabeli Data,
 *  - sms.xml      : format zgodny z "SMS Backup & Restore" (da się go przywrócić).
 */
class Exporter(
    private val ctx: Context,
    private val session: String,
    private val progress: (String) -> Unit,
    /** Smart EI: zapis do własnego folderu (domyślnie Download/Backup/<sesja>/ przez Storage). */
    private val writer: ((String, String, (OutputStream) -> Int) -> Pair<Storage.Written, Int>)? = null,
    private val wantContacts: Boolean = true,
    private val wantSms: Boolean = true
) {
    data class Result(
        val contacts: Int,
        val sms: Int,
        val contactsDenied: Boolean,
        val smsDenied: Boolean,
        val files: List<Storage.Written>
    )

    private val cr: ContentResolver = ctx.contentResolver

    private fun granted(p: String): Boolean =
        Build.VERSION.SDK_INT < 23 || ctx.checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED

    fun run(): Result {
        val canContacts = wantContacts && granted(Manifest.permission.READ_CONTACTS)
        val canSms = wantSms && granted(Manifest.permission.READ_SMS)
        if (!canContacts && !canSms) {
            throw SecurityException(ctx.tr("err_no_perms"))
        }
        val files = mutableListOf<Storage.Written>()
        var contacts = 0
        var sms = 0
        if (canContacts) {
            val (w, n) = write("contacts.vcf", "text/x-vcard") { exportContacts(it) }
            files += w
            contacts = n
        }
        if (canSms) {
            val (w, n) = write("sms.xml", "text/xml") { exportSms(it, canContacts) }
            files += w
            sms = n
        }
        return Result(contacts, sms, wantContacts && !canContacts, wantSms && !canSms, files)
    }

    private fun write(name: String, mime: String, block: (OutputStream) -> Int): Pair<Storage.Written, Int> =
        writer?.invoke(name, mime, block) ?: Storage.write(ctx, session, name, mime) { block(it) }

    // ------------------------------------------------------------------ //
    // Kontakty
    // ------------------------------------------------------------------ //

    private fun exportContacts(out: OutputStream): Int {
        val proj = arrayOf(
            ContactsContract.Contacts._ID,
            ContactsContract.Contacts.LOOKUP_KEY,
            ContactsContract.Contacts.DISPLAY_NAME
        )
        var n = 0
        cr.query(ContactsContract.Contacts.CONTENT_URI, proj, null, null, ContactsContract.Contacts._ID)?.use { c ->
            val total = c.count
            while (c.moveToNext()) {
                val id = c.getLong(0)
                val key = c.getString(1)
                val name = c.getString(2)
                val systemCard = key != null && copySystemVcard(key, out)
                if (!systemCard) Storage.write(out, manualVcard(id, name))
                n++
                if (n % 25 == 0 || n == total) progress(ctx.tr("progress_contacts_fmt", n, total))
            }
        }
        return n
    }

    /** vCard wygenerowany przez dostawcę kontaktów (pełny: zdjęcie, adresy, grupy...). */
    private fun copySystemVcard(lookupKey: String, out: OutputStream): Boolean {
        val uri = Uri.withAppendedPath(ContactsContract.Contacts.CONTENT_VCARD_URI, lookupKey)
        return try {
            val afd = cr.openAssetFileDescriptor(uri, "r") ?: return false
            var copied = 0L
            afd.use { a ->
                a.createInputStream().use { input ->
                    val buf = ByteArray(16 * 1024)
                    var last: Byte = '\n'.code.toByte()
                    while (true) {
                        val r = input.read(buf)
                        if (r <= 0) break
                        out.write(buf, 0, r)
                        copied += r
                        last = buf[r - 1]
                    }
                    if (copied > 0 && last != '\n'.code.toByte()) Storage.write(out, "\r\n")
                }
            }
            copied > 0
        } catch (_: Exception) {
            false
        }
    }

    /** Awaryjny vCard 3.0 z tabeli Data (gdy producent zepsuł CONTENT_VCARD_URI). */
    private fun manualVcard(contactId: Long, displayName: String?): String {
        val sb = StringBuilder("BEGIN:VCARD\r\nVERSION:3.0\r\n")
        var hasName = false
        val proj = arrayOf(
            ContactsContract.Data.MIMETYPE,
            ContactsContract.Data.DATA1,
            ContactsContract.Data.DATA2,
            ContactsContract.Data.DATA3,
            ContactsContract.Data.DATA4,
            ContactsContract.Data.DATA5,
            ContactsContract.Data.DATA6
        )
        cr.query(
            ContactsContract.Data.CONTENT_URI, proj,
            "${ContactsContract.Data.CONTACT_ID}=?", arrayOf(contactId.toString()), null
        )?.use { c ->
            while (c.moveToNext()) {
                val mime = c.getString(0) ?: continue
                val d1 = c.getString(1)
                when (mime) {
                    CommonDataKinds.StructuredName.CONTENT_ITEM_TYPE -> {
                        val given = c.getString(2).orEmpty()
                        val family = c.getString(3).orEmpty()
                        val prefix = c.getString(4).orEmpty()
                        val middle = c.getString(5).orEmpty()
                        val suffix = c.getString(6).orEmpty()
                        sb.append("N:${esc(family)};${esc(given)};${esc(middle)};${esc(prefix)};${esc(suffix)}\r\n")
                        sb.append("FN:${esc(d1 ?: displayName.orEmpty())}\r\n")
                        hasName = true
                    }
                    CommonDataKinds.Phone.CONTENT_ITEM_TYPE -> if (!d1.isNullOrBlank()) {
                        val type = when (c.getInt(2)) {
                            CommonDataKinds.Phone.TYPE_MOBILE -> "CELL"
                            CommonDataKinds.Phone.TYPE_HOME -> "HOME"
                            CommonDataKinds.Phone.TYPE_WORK -> "WORK"
                            else -> "VOICE"
                        }
                        sb.append("TEL;TYPE=$type:${esc(d1)}\r\n")
                    }
                    CommonDataKinds.Email.CONTENT_ITEM_TYPE -> if (!d1.isNullOrBlank()) sb.append("EMAIL:${esc(d1)}\r\n")
                    CommonDataKinds.Organization.CONTENT_ITEM_TYPE -> if (!d1.isNullOrBlank()) sb.append("ORG:${esc(d1)}\r\n")
                    CommonDataKinds.Note.CONTENT_ITEM_TYPE -> if (!d1.isNullOrBlank()) sb.append("NOTE:${esc(d1)}\r\n")
                    CommonDataKinds.StructuredPostal.CONTENT_ITEM_TYPE -> if (!d1.isNullOrBlank()) {
                        sb.append("ADR:;;${esc(d1)};;;;\r\n")
                    }
                    CommonDataKinds.Event.CONTENT_ITEM_TYPE -> if (!d1.isNullOrBlank() && c.getInt(2) == CommonDataKinds.Event.TYPE_BIRTHDAY) {
                        sb.append("BDAY:${esc(d1)}\r\n")
                    }
                }
            }
        }
        if (!hasName) {
            sb.append("N:;${esc(displayName.orEmpty())};;;\r\nFN:${esc(displayName.orEmpty())}\r\n")
        }
        sb.append("END:VCARD\r\n")
        return sb.toString()
    }

    private fun esc(s: String): String =
        s.replace("\\", "\\\\").replace("\n", "\\n").replace("\r", "").replace(",", "\\,").replace(";", "\\;")

    // ------------------------------------------------------------------ //
    // SMS
    // ------------------------------------------------------------------ //

    private fun exportSms(out: OutputStream, canResolveNames: Boolean): Int {
        val uri = Uri.parse("content://sms")
        val w = BufferedWriter(OutputStreamWriter(out, Charsets.UTF_8), 64 * 1024)
        val dateFmt = SimpleDateFormat("d MMM yyyy HH:mm:ss", Locale.getDefault())
        val names = HashMap<String, String>()
        var n = 0
        val cursor = cr.query(uri, null, null, null, "date ASC")
            ?: throw IllegalStateException("SMS provider not available on this device")
        cursor.use { c ->
            val total = c.count
            w.write("<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\n")
            w.write("<!-- Drapco ADB Service Tool - kopia SMS (format zgodny z SMS Backup & Restore) -->\n")
            w.write(
                "<smses count=\"$total\" backup_set=\"${xml(session)}\" backup_date=\"${System.currentTimeMillis()}\" " +
                    "type=\"full\">\n"
            )
            while (c.moveToNext()) {
                val address = c.str("address")
                val date = c.long("date")
                val contact = if (canResolveNames && !address.isNullOrBlank()) {
                    names.getOrPut(address) { lookupName(address) ?: "(Unknown)" }
                } else {
                    "(Unknown)"
                }
                w.write("  <sms")
                attr(w, "protocol", c.str("protocol") ?: "0")
                attr(w, "address", address ?: "")
                attr(w, "date", date.toString())
                attr(w, "type", c.str("type") ?: "1")
                attr(w, "subject", c.str("subject") ?: "null")
                attr(w, "body", c.str("body") ?: "")
                attr(w, "toa", "null")
                attr(w, "sc_toa", "null")
                attr(w, "service_center", c.str("service_center") ?: "null")
                attr(w, "read", c.str("read") ?: "1")
                attr(w, "status", c.str("status") ?: "-1")
                attr(w, "locked", c.str("locked") ?: "0")
                attr(w, "date_sent", c.str("date_sent") ?: "0")
                attr(w, "sub_id", c.str("sub_id") ?: "-1")
                attr(w, "readable_date", dateFmt.format(Date(date)))
                attr(w, "contact_name", contact)
                w.write(" />\n")
                n++
                if (n % 200 == 0 || n == total) progress(ctx.tr("progress_sms_fmt", n, total))
            }
            w.write("</smses>\n")
        }
        w.flush()
        return n
    }

    private fun lookupName(number: String): String? = try {
        val uri = Uri.withAppendedPath(ContactsContract.PhoneLookup.CONTENT_FILTER_URI, Uri.encode(number))
        cr.query(uri, arrayOf(ContactsContract.PhoneLookup.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }
    } catch (_: Exception) {
        null
    }

    private fun Cursor.str(col: String): String? {
        val i = getColumnIndex(col)
        return if (i >= 0 && !isNull(i)) getString(i) else null
    }

    private fun Cursor.long(col: String): Long {
        val i = getColumnIndex(col)
        return if (i >= 0 && !isNull(i)) getLong(i) else 0L
    }

    private fun attr(w: BufferedWriter, name: String, value: String) {
        w.write(" ")
        w.write(name)
        w.write("=\"")
        w.write(xml(value))
        w.write("\"")
    }

    companion object {
        /** Escapowanie atrybutu XML (+ usunięcie znaków niedozwolonych w XML 1.0). */
        fun xml(s: String): String {
            val sb = StringBuilder(s.length + 16)
            for (ch in s) {
                when {
                    ch == '&' -> sb.append("&amp;")
                    ch == '<' -> sb.append("&lt;")
                    ch == '>' -> sb.append("&gt;")
                    ch == '"' -> sb.append("&quot;")
                    ch == '\'' -> sb.append("&apos;")
                    ch == '\n' -> sb.append("&#10;")
                    ch == '\r' -> sb.append("&#13;")
                    ch == '\t' -> sb.append("&#9;")
                    ch < ' ' || ch == '\uFFFE' || ch == '\uFFFF' -> {}
                    else -> sb.append(ch)
                }
            }
            return sb.toString()
        }
    }
}
