package com.drapco.agent

import android.content.ContentValues
import android.content.Context
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import java.io.File
import java.io.FileOutputStream
import java.io.FilterOutputStream
import java.io.IOException
import java.io.OutputStream

/**
 * Zapis plików do publicznego `Download/<base>/<sesja>/` (base = Backup albo Restore),
 * który ADB czyta bez roota.
 *
 * - Android 10+ (API 29): MediaStore.Downloads - bez uprawnień do pamięci.
 *   Plik powstaje jako "pending" (niewidoczny), a po zapisie jest publikowany,
 *   więc komputer nigdy nie pobierze połowy pliku.
 * - Android 5-9: zwykły plik (WRITE_EXTERNAL_STORAGE), zapis do .part + rename.
 *
 * Każdy utworzony plik jest zapamiętywany (SharedPreferences), żeby
 * CleanupReceiver mógł go później usunąć razem z wpisem w MediaStore.
 */
object Storage {

    private const val PREFS = "backup_files"

    const val BACKUP = "Backup"
    const val RESTORE = "Restore"

    fun relativeDir(base: String, session: String): String = "${Environment.DIRECTORY_DOWNLOADS}/$base/$session/"

    /** Licznik bajtów (rozmiar trafia do done.json - komputer go weryfikuje). */
    class CountingStream(out: OutputStream) : FilterOutputStream(out) {
        var count: Long = 0
            private set

        override fun write(b: Int) {
            out.write(b)
            count++
        }

        override fun write(b: ByteArray, off: Int, len: Int) {
            out.write(b, off, len)
            count += len
        }
    }

    data class Written(val name: String, val bytes: Long)

    /**
     * Tworzy plik `name` w folderze sesji i wywołuje `body` z licznikiem bajtów.
     * Zwraca faktyczną nazwę (MediaStore może ją zmienić) i rozmiar.
     */
    fun <T> write(
        ctx: Context,
        session: String,
        name: String,
        mime: String,
        base: String = BACKUP,
        body: (CountingStream) -> T
    ): Pair<Written, T> {
        return if (Build.VERSION.SDK_INT >= 29) writeMediaStore(ctx, base, session, name, mime, body)
        else writeLegacy(ctx, base, session, name, body)
    }

    private fun <T> writeMediaStore(
        ctx: Context,
        base: String,
        session: String,
        name: String,
        mime: String,
        body: (CountingStream) -> T
    ): Pair<Written, T> {
        val resolver = ctx.contentResolver
        val values = ContentValues().apply {
            put(MediaStore.MediaColumns.DISPLAY_NAME, name)
            put(MediaStore.MediaColumns.MIME_TYPE, mime)
            put(MediaStore.MediaColumns.RELATIVE_PATH, relativeDir(base, session))
            put(MediaStore.MediaColumns.IS_PENDING, 1)
        }
        val uri = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
            ?: throw IOException("MediaStore rejected $name")
        remember(ctx, session, uri.toString())
        try {
            val out = resolver.openOutputStream(uri, "w") ?: throw IOException("Cannot open $name for writing")
            val counting = CountingStream(out.buffered(64 * 1024))
            val result = counting.use { body(it) }
            val publish = ContentValues().apply { put(MediaStore.MediaColumns.IS_PENDING, 0) }
            resolver.update(uri, publish, null, null)
            return Pair(Written(displayName(ctx, uri) ?: name, counting.count), result)
        } catch (t: Throwable) {
            try {
                resolver.delete(uri, null, null)
            } catch (_: Throwable) {
            }
            throw t
        }
    }

    @Suppress("DEPRECATION")
    private fun <T> writeLegacy(
        ctx: Context,
        base: String,
        session: String,
        name: String,
        body: (CountingStream) -> T
    ): Pair<Written, T> {
        val dir = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "$base/$session")
        if (!dir.isDirectory && !dir.mkdirs()) throw IOException("Cannot create folder ${dir.path}")
        val target = File(dir, name)
        val part = File(dir, ".$name.part")
        val counting = CountingStream(FileOutputStream(part).buffered(64 * 1024))
        val result = counting.use { body(it) }
        if (!part.renameTo(target)) throw IOException("Cannot write ${target.path}")
        remember(ctx, session, target.absolutePath)
        return Pair(Written(name, counting.count), result)
    }

    private fun displayName(ctx: Context, uri: Uri): String? =
        ctx.contentResolver.query(uri, arrayOf(MediaStore.MediaColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) c.getString(0) else null
        }

    private fun remember(ctx: Context, session: String, item: String) {
        val prefs = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val set = HashSet(prefs.getStringSet(session, emptySet()) ?: emptySet())
        set += item
        prefs.edit().putStringSet(session, set).commit()
    }

    /** Usuwa wszystkie pliki sesji (wpisy MediaStore + pliki). Zwraca liczbę usuniętych. */
    fun deleteSession(ctx: Context, session: String): Int {
        val prefs = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        val items = prefs.getStringSet(session, emptySet()) ?: emptySet()
        var n = 0
        for (item in items) {
            try {
                if (item.startsWith("content://")) {
                    n += ctx.contentResolver.delete(Uri.parse(item), null, null)
                } else if (File(item).delete()) {
                    n++
                }
            } catch (_: Throwable) {
                // plik mógł zostać już usunięty przez komputer
            }
        }
        if (Build.VERSION.SDK_INT < 29) {
            @Suppress("DEPRECATION")
            for (base in arrayOf(BACKUP, RESTORE)) {
                val dir = File(Environment.getExternalStoragePublicDirectory(Environment.DIRECTORY_DOWNLOADS), "$base/$session")
                // w Restore/ leżą też pliki wgrane przez komputer - usuwamy tylko własne znaczniki
                if (base == BACKUP) dir.listFiles()?.forEach { it.delete() }
                dir.delete()
            }
        }
        prefs.edit().remove(session).commit()
        return n
    }

    fun write(out: OutputStream, text: String) = out.write(text.toByteArray(Charsets.UTF_8))
}
