package com.drapco.agent

import android.content.ContentProvider
import android.content.ContentValues
import android.content.Context
import android.database.Cursor
import android.database.MatrixCursor
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.OpenableColumns
import java.io.File
import java.io.FileNotFoundException

/**
 * Udostępnia (tylko do odczytu) kopię pliku .vcf aplikacji Kontakty.
 * Od Androida 7 nie wolno przekazywać innym aplikacjom ścieżek file://,
 * a projekt nie używa AndroidX (FileProvider) - stąd własny, minimalny dostawca.
 *
 * Nieeksportowany: dostęp ma wyłącznie aplikacja, której nadamy jednorazowe
 * prawo odczytu (Intent.FLAG_GRANT_READ_URI_PERMISSION).
 */
class VcfProvider : ContentProvider() {

    override fun onCreate(): Boolean = true

    private fun fileFor(uri: Uri): File {
        val ctx = context ?: throw FileNotFoundException(uri.toString())
        val name = uri.lastPathSegment.orEmpty()
        if (!SAFE.matches(name)) throw FileNotFoundException(uri.toString())
        val f = File(dir(ctx), name)
        if (!f.isFile) throw FileNotFoundException(uri.toString())
        return f
    }

    override fun query(
        uri: Uri,
        projection: Array<out String>?,
        selection: String?,
        selectionArgs: Array<out String>?,
        sortOrder: String?
    ): Cursor {
        val f = fileFor(uri)
        val cols = projection ?: arrayOf(OpenableColumns.DISPLAY_NAME, OpenableColumns.SIZE)
        val row = cols.map { c ->
            when (c) {
                OpenableColumns.DISPLAY_NAME -> f.name
                OpenableColumns.SIZE -> f.length()
                else -> null
            }
        }
        return MatrixCursor(cols, 1).apply { addRow(row) }
    }

    override fun getType(uri: Uri): String = MIME

    override fun openFile(uri: Uri, mode: String): ParcelFileDescriptor {
        if (mode != "r") throw SecurityException("read-only")
        return ParcelFileDescriptor.open(fileFor(uri), ParcelFileDescriptor.MODE_READ_ONLY)
    }

    override fun insert(uri: Uri, values: ContentValues?): Uri? = throw UnsupportedOperationException()
    override fun delete(uri: Uri, selection: String?, selectionArgs: Array<out String>?): Int = 0
    override fun update(uri: Uri, values: ContentValues?, selection: String?, selectionArgs: Array<out String>?): Int = 0

    companion object {
        const val MIME = "text/x-vcard"
        private val SAFE = Regex("[A-Za-z0-9._-]{1,64}")

        fun dir(ctx: Context): File = File(ctx.cacheDir, "restore")

        fun uriFor(ctx: Context, f: File): Uri = Uri.parse("content://${ctx.packageName}.files/${f.name}")
    }
}
