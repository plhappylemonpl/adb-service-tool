package com.drapco.agent

import android.app.PendingIntent
import android.app.UiModeManager
import android.app.WallpaperManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ApplicationInfo
import android.content.pm.PackageInstaller
import android.content.pm.PackageManager
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Canvas
import android.media.AudioManager
import android.os.Build
import android.os.Environment
import android.os.StatFs
import android.provider.Settings
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/** Dane urządzenia dla komputera (hello / manifest.json). */
object DeviceDesc {
    fun json(): JSONObject = JSONObject()
        .put("manufacturer", Build.MANUFACTURER.orEmpty())
        .put("brand", Build.BRAND.orEmpty())
        .put("model", Build.MODEL.orEmpty())
        .put("device", Build.DEVICE.orEmpty())
        .put("sdk", Build.VERSION.SDK_INT)
        .put("androidVersion", Build.VERSION.RELEASE.orEmpty())

    @Suppress("DEPRECATION")
    fun storageRoot(): File = Environment.getExternalStorageDirectory()

    fun freeBytes(): Long = runCatching { StatFs(storageRoot().path).availableBytes }.getOrDefault(0L)
}

/** Lista aplikacji użytkownika (+ zaktualizowanych systemowych) ze źródłem instalacji. */
object AppList {
    data class App(
        val pkg: String,
        val label: String,
        val versionName: String,
        val versionCode: Long,
        val installer: String?,
        val system: Boolean,
        val updatedSystem: Boolean,
        val apks: List<String>
    ) {
        val size: Long get() = apks.sumOf { File(it).length() }
        fun json(): JSONObject = JSONObject()
            .put("package", pkg).put("label", label).put("versionName", versionName).put("versionCode", versionCode)
            .put("installer", installer ?: JSONObject.NULL).put("system", system).put("updatedSystem", updatedSystem)
            .put("size", size).put("apks", JSONArray(apks))
    }

    @Suppress("DEPRECATION")
    fun installer(pm: PackageManager, pkg: String): String? = try {
        if (Build.VERSION.SDK_INT >= 30) pm.getInstallSourceInfo(pkg).installingPackageName
        else pm.getInstallerPackageName(pkg)
    } catch (_: Throwable) {
        null
    }

    @Suppress("DEPRECATION")
    fun collect(ctx: Context): List<App> {
        val pm = ctx.packageManager
        val out = mutableListOf<App>()
        for (ai in pm.getInstalledApplications(0)) {
            if (ai.packageName == ctx.packageName) continue
            val system = ai.flags and ApplicationInfo.FLAG_SYSTEM != 0
            val updated = ai.flags and ApplicationInfo.FLAG_UPDATED_SYSTEM_APP != 0
            if (system && !updated) continue
            val pi = runCatching { pm.getPackageInfo(ai.packageName, 0) }.getOrNull()
            val code = when {
                pi == null -> 0L
                Build.VERSION.SDK_INT >= 28 -> pi.longVersionCode
                else -> pi.versionCode.toLong()
            }
            val apks = mutableListOf<String>()
            ai.sourceDir?.let { apks += it }
            ai.splitSourceDirs?.let { apks += it }
            out += App(
                ai.packageName,
                runCatching { pm.getApplicationLabel(ai).toString() }.getOrDefault(ai.packageName),
                pi?.versionName.orEmpty(),
                code,
                installer(pm, ai.packageName),
                system,
                updated,
                apks
            )
        }
        return out.sortedBy { it.label.lowercase() }
    }

    fun isPlay(installer: String?): Boolean =
        installer == "com.android.vending" || installer == "com.google.android.feedback"

    @Suppress("DEPRECATION")
    fun installedPackages(ctx: Context): List<String> =
        runCatching { ctx.packageManager.getInstalledPackages(0).map { it.packageName } }.getOrDefault(emptyList())

    fun isInstalled(ctx: Context, pkg: String): Boolean =
        runCatching { ctx.packageManager.getPackageInfo(pkg, 0); true }.getOrDefault(false)
}

/** Tapeta: eksport (jeśli system pozwala) i ustawienie przy imporcie. */
object Wallpapers {
    data class Exported(val files: List<Pair<String, File>>, val note: String)

    private fun toPng(bmp: Bitmap, f: File) {
        FileOutputStream(f).use { bmp.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }

    fun export(ctx: Context, dir: File): Exported {
        dir.mkdirs()
        val wm = WallpaperManager.getInstance(ctx)
        val out = mutableListOf<Pair<String, File>>()
        var note = ""
        if (wm.wallpaperInfo != null) note = "live wallpaper"
        try {
            if (Build.VERSION.SDK_INT >= 24) {
                for ((flag, name) in listOf(WallpaperManager.FLAG_SYSTEM to "home.png", WallpaperManager.FLAG_LOCK to "lock.png")) {
                    val pfd = wm.getWallpaperFile(flag) ?: continue
                    pfd.use {
                        val bmp = BitmapFactory.decodeFileDescriptor(it.fileDescriptor) ?: return@use
                        val f = File(dir, name)
                        toPng(bmp, f)
                        bmp.recycle()
                        out += name to f
                    }
                }
            }
            if (out.isEmpty()) {
                @Suppress("DEPRECATION")
                val d = wm.drawable
                if (d != null) {
                    val w = d.intrinsicWidth.coerceIn(1, 8192)
                    val h = d.intrinsicHeight.coerceIn(1, 8192)
                    val bmp = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
                    d.setBounds(0, 0, w, h)
                    d.draw(Canvas(bmp))
                    val f = File(dir, "home.png")
                    toPng(bmp, f)
                    bmp.recycle()
                    out += "home.png" to f
                }
            }
        } catch (e: SecurityException) {
            // Android 13+: odczyt tapety tylko dla aplikacji systemowych (READ_WALLPAPER_INTERNAL)
            note = "android-${Build.VERSION.SDK_INT}: wallpaper not readable by apps"
        } catch (e: Throwable) {
            note = e.message ?: e.javaClass.simpleName
        }
        return Exported(out, note)
    }

    /** Ustawia tapetę; `lock` = ekran blokady (Android 7+). */
    fun set(ctx: Context, f: File, lock: Boolean) {
        val wm = WallpaperManager.getInstance(ctx)
        FileInputStream(f).use { input ->
            if (Build.VERSION.SDK_INT >= 24) {
                wm.setStream(input, null, true, if (lock) WallpaperManager.FLAG_LOCK else WallpaperManager.FLAG_SYSTEM)
            } else if (!lock) {
                wm.setStream(input)
            }
        }
    }
}

/** Przenośne ustawienia (te same klucze co settings.json po stronie komputera). */
object PortableSettingsIO {
    private val SYSTEM_INT = listOf("screen_off_timeout", "accelerometer_rotation", "haptic_feedback_enabled", "vibrate_when_ringing")
    private val STREAMS = listOf("ring" to AudioManager.STREAM_RING, "music" to AudioManager.STREAM_MUSIC,
        "alarm" to AudioManager.STREAM_ALARM, "notification" to AudioManager.STREAM_NOTIFICATION)

    fun read(ctx: Context): JSONObject {
        val cr = ctx.contentResolver
        val sys = JSONObject()
        for (k in SYSTEM_INT) runCatching { sys.put(k, Settings.System.getInt(cr, k)) }
        runCatching { sys.put("font_scale", Settings.System.getFloat(cr, Settings.System.FONT_SCALE).toDouble()) }
        runCatching { Settings.System.getString(cr, Settings.System.TIME_12_24)?.let { sys.put("time_12_24", it) } }
        val vols = JSONObject()
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        for ((name, s) in STREAMS) runCatching {
            vols.put(name, JSONObject().put("value", am.getStreamVolume(s)).put("max", am.getStreamMaxVolume(s)))
        }
        val o = JSONObject().put("system", sys).put("volumes", vols)
        val um = ctx.getSystemService(Context.UI_MODE_SERVICE) as UiModeManager
        o.put("uiNightMode", um.nightMode)
        return o
    }

    fun canWrite(ctx: Context): Boolean = Build.VERSION.SDK_INT < 23 || Settings.System.canWrite(ctx)

    /** Zwraca liczbę zastosowanych ustawień (tryb ciemny wymaga uprawnień systemowych - pomijany). */
    fun apply(ctx: Context, s: JSONObject): Int {
        var n = 0
        val cr = ctx.contentResolver
        val sys = s.optJSONObject("system") ?: JSONObject()
        if (canWrite(ctx)) {
            for (k in SYSTEM_INT) {
                val v = sys.optString(k, "").toIntOrNull() ?: continue
                if (runCatching { Settings.System.putInt(cr, k, v) }.getOrDefault(false)) n++
            }
            sys.optString("font_scale", "").toFloatOrNull()?.takeIf { it in 0.5f..2.0f }?.let {
                if (runCatching { Settings.System.putFloat(cr, Settings.System.FONT_SCALE, it) }.getOrDefault(false)) n++
            }
            sys.optString("time_12_24", "").takeIf { it == "12" || it == "24" }?.let {
                if (runCatching { Settings.System.putString(cr, Settings.System.TIME_12_24, it) }.getOrDefault(false)) n++
            }
        }
        val am = ctx.getSystemService(Context.AUDIO_SERVICE) as AudioManager
        val vols = s.optJSONObject("volumes") ?: JSONObject()
        for ((name, stream) in STREAMS) {
            val v = vols.optJSONObject(name) ?: continue
            val max = v.optInt("max", 0)
            if (max <= 0) continue
            val tmax = am.getStreamMaxVolume(stream)
            val scaled = Math.round(v.optInt("value", 0).toDouble() * tmax / max).toInt().coerceIn(0, tmax)
            if (runCatching { am.setStreamVolume(stream, scaled, 0) }.isSuccess) n++
        }
        return n
    }
}

/** Pliki kategorii (ścieżki względem pamięci wewnętrznej). */
object Walker {
    data class Entry(val file: File, val rel: String)

    fun list(root: File, relDir: String, exclude: Set<String>): List<Entry> {
        val base = File(root, relDir)
        if (!base.isDirectory) return emptyList()
        val out = mutableListOf<Entry>()
        val stack = ArrayDeque<Pair<File, String>>()
        stack.add(base to relDir)
        while (stack.isNotEmpty()) {
            val (d, rel) = stack.removeLast()
            val children = d.listFiles() ?: continue
            for (c in children) {
                if (d == base && relDir == "Download" && c.name in exclude) continue
                val r = "$rel/${c.name}"
                if (c.isDirectory) stack.add(c to r) else if (c.isFile && !c.name.endsWith(".part")) out += Entry(c, r)
            }
        }
        return out.sortedBy { it.rel }
    }

    fun size(root: File, relDir: String, exclude: Set<String>): Long = list(root, relDir, exclude).sumOf { it.file.length() }
}

/**
 * Instalacja własnych APK (base + splity) przez systemowy PackageInstaller.
 * Użytkownik zawsze potwierdza instalację w oknie systemu.
 */
class ApkInstaller(private val ctx: Context) {
    fun allowed(): Boolean = Build.VERSION.SDK_INT < 26 || ctx.packageManager.canRequestPackageInstalls()

    /** Blokuje do wyniku (max 10 min). Zwraca null = sukces, inaczej opis błędu. */
    fun install(files: List<File>): String? {
        val pi = ctx.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
        val id = pi.createSession(params)
        val session = pi.openSession(id)
        try {
            for (f in files) {
                FileInputStream(f).use { input ->
                    session.openWrite(f.name, 0, f.length()).use { out ->
                        input.copyTo(out, 256 * 1024)
                        session.fsync(out)
                    }
                }
            }
            val action = "${ctx.packageName}.SMARTEI_INSTALL.$id"
            val latch = CountDownLatch(1)
            var result: String? = "timeout"
            val receiver = object : BroadcastReceiver() {
                override fun onReceive(c: Context, intent: Intent) {
                    when (val st = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
                        PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                            @Suppress("DEPRECATION")
                            val confirm = intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT)
                            confirm?.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                            runCatching { c.startActivity(confirm) }
                        }
                        PackageInstaller.STATUS_SUCCESS -> {
                            result = null
                            latch.countDown()
                        }
                        else -> {
                            result = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: "status $st"
                            latch.countDown()
                        }
                    }
                }
            }
            if (Build.VERSION.SDK_INT >= 33) {
                ctx.registerReceiver(receiver, IntentFilter(action), Context.RECEIVER_NOT_EXPORTED)
            } else {
                ctx.registerReceiver(receiver, IntentFilter(action))
            }
            try {
                var flags = PendingIntent.FLAG_UPDATE_CURRENT
                if (Build.VERSION.SDK_INT >= 31) flags = flags or PendingIntent.FLAG_MUTABLE
                val pending = PendingIntent.getBroadcast(ctx, id, Intent(action).setPackage(ctx.packageName), flags)
                session.commit(pending.intentSender)
                latch.await(10, TimeUnit.MINUTES)
            } finally {
                runCatching { ctx.unregisterReceiver(receiver) }
            }
            return result
        } catch (t: Throwable) {
            runCatching { session.abandon() }
            return t.message ?: t.javaClass.simpleName
        } finally {
            runCatching { session.close() }
        }
    }
}
