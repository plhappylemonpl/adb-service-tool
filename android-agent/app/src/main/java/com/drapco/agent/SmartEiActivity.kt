package com.drapco.agent

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.media.MediaScannerConnection
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.Settings
import android.text.InputType
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.ScrollView
import android.widget.TextView
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Smart EI (Smart Export Import) - agent po stronie telefonu.
 *
 * Uruchomienie:
 *  - ADB: komputer wywołuje `am start -n com.drapco.agent/.SmartEiActivity --es host 127.0.0.1
 *    --ei port <P> --es pin <PIN> --es role export|import --ez adb true` (połączenie przez `adb reverse`),
 *  - Wi-Fi: kod QR otwiera `drapco-smartei://pair?h=<IP>&p=<PORT>&k=<PIN>`, albo użytkownik
 *    wpisuje IP:port i PIN ręcznie (ikona agenta w menu aplikacji).
 *
 * Każdy eksport/import startuje dopiero po kliknięciu przycisku na telefonie (świadoma zgoda),
 * uprawnienia systemowe pyta Android. Komunikacja: HTTP z tokenem sesji (SmartEiClient).
 */
class SmartEiActivity : Activity() {

    private lateinit var status: TextView
    private lateinit var intro: TextView
    private lateinit var detail: TextView
    private lateinit var progress: ProgressBar
    private lateinit var form: LinearLayout
    private lateinit var address: EditText
    private lateinit var pin: EditText
    private lateinit var buttons: LinearLayout

    private val signals = LinkedBlockingQueue<String>()
    @Volatile
    private var worker: Thread? = null
    @Volatile
    private var restoreReport: String? = null
    private var adb = false
    /** Aktywna sesja (host:port:PIN) - po zakończeniu zapisywana w RestoreState. */
    @Volatile
    private var sessionKey = ""
    @Volatile
    private var client: SmartEiClient? = null
    @Volatile
    private var heartbeatOn = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= 27) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        }
        setContentView(buildUi())
        handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        if (worker?.isAlive != true) handle(intent)
    }

    private fun handle(i: Intent?) {
        val host = i?.getStringExtra("host")
        val p = i?.getIntExtra("port", 0) ?: 0
        val k = i?.getStringExtra("pin")
        if (host != null && p > 0 && k != null) {
            adb = i.getBooleanExtra("adb", false)
            // proces wznowiony przez system (np. po utracie roli SMS) z tym samym intentem -
            // sesja jest już zakończona: ekran końcowy zamiast ponownego łączenia
            RestoreState.smartEiDone(this, "$host:$p:$k")?.let { (text, wasAdb) ->
                adb = wasAdb
                done(text, OK)
                return
            }
            start(host, p, k)
            return
        }
        val data = i?.data
        if (data != null && data.scheme == "drapco-smartei") {
            val h = data.getQueryParameter("h").orEmpty()
            val port = data.getQueryParameter("p").orEmpty()
            val kk = data.getQueryParameter("k").orEmpty().filter { it.isDigit() }.take(6)
            address.setText("$h:$port")
            pin.setText(kk)
            val parsed = SmartEiClient.parseAddress("$h:$port")
            if (parsed != null && kk.length == 6) {
                if (RestoreState.smartEiDone(this, "${parsed.first}:${parsed.second}:$kk") != null) {
                    done(RestoreState.smartEiDone(this, null)?.first ?: tr("sei_done_import"), OK)
                    return
                }
                start(parsed.first, parsed.second, kk)
                return
            }
        }
        // otwarte z menu krótko po zakończonej sesji (np. po restarcie procesu) - ekran końcowy
        RestoreState.smartEiDone(this, null)?.let { (text, wasAdb) ->
            adb = wasAdb
            done(text, OK)
            showNewConnectionButton()
            return
        }
        showForm(null)
    }

    override fun onResume() {
        super.onResume()
        signals.offer("resume")
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        signals.offer("perms")
    }

    @Deprecated("Activity API (bez AndroidX)")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode == REQ_RESTORE) {
            restoreReport = data?.getStringExtra("report")
            signals.offer("restore")
        }
    }

    // ------------------------------------------------------------------ //
    // Wątek sesji
    // ------------------------------------------------------------------ //

    private fun start(host: String, port: Int, code: String) {
        if (worker?.isAlive == true) return
        form.visibility = View.GONE
        signals.clear()
        sessionKey = "$host:$port:$code"
        val t = Thread({ session(SmartEiClient(host, port), code) }, "smartei")
        worker = t
        t.start()
    }

    private fun session(c: SmartEiClient, code: String) {
        try {
            ui { busy(true); setStatus(tr("sei_connecting"), GOLD) }
            val r = c.pair(code, DeviceDesc.json())
            client = c
            startHeartbeat(c)
            adb = r.optBoolean("adb", adb)
            val role = r.optString("role")
            ui { setStatus(tr("sei_paired"), OK) }
            // Wi-Fi: pliki idą przez agenta - dostęp do pamięci przed policzeniem rozmiarów
            if (!adb) ensureStorage(write = role == "import")
            c.post("/hello", hello())
            ui { busy(false); setIntro(tr("sei_wait_review")); setStatus(tr("sei_limits"), MUTED) }
            val job = waitJob(c) { it.optString("state") == "go" }
            if (role == "export") exportFlow(c, job) else importFlow(c)
        } catch (_: SmartEiCancelled) {
            ui { done(tr("sei_cancelled"), WARN) }
        } catch (e: SmartEiHttpError) {
            val msg = when (e.code) {
                403 -> tr("sei_err_pin")
                423 -> tr("sei_err_locked")
                409 -> tr("sei_err_busy")
                else -> tr("sei_error_fmt", e.message ?: "")
            }
            ui { busy(false); showForm(msg) }
        } catch (e: IOException) {
            ui { busy(false); showForm(tr("sei_err_net_fmt", e.message ?: e.javaClass.simpleName)) }
        } catch (t: Throwable) {
            runCatching { c.post("/error", JSONObject().put("error", t.message ?: t.javaClass.simpleName)) }
            ui { done(tr("sei_error_fmt", t.message ?: t.javaClass.simpleName), ERR) }
        } finally {
            heartbeatOn = false
        }
    }

    /** Co 8 s sygnał życia - komputer po ~30 s ciszy (120 s w krokach z oknami systemu) zgłasza błąd. */
    private fun startHeartbeat(c: SmartEiClient) {
        heartbeatOn = true
        Thread({
            while (heartbeatOn) {
                runCatching { c.post("/ping") }.onFailure { if (it is SmartEiCancelled) heartbeatOn = false }
                try {
                    Thread.sleep(8000)
                } catch (_: InterruptedException) {
                    return@Thread
                }
            }
        }, "smartei-heartbeat").apply { isDaemon = true }.start()
    }

    /** Krok z oknami systemu / innej aplikacji (zgody, Kontakty, rola SMS, instalator): dłuższy limit ciszy na PC. */
    private fun <T> interactive(block: () -> T): T {
        runCatching { client?.post("/stage", JSONObject().put("stage", "interactive")) }
        try {
            return block()
        } finally {
            runCatching { client?.post("/stage", JSONObject().put("stage", "interactive_end")) }
        }
    }

    /** Zakończenie sesji: zapis stanu (przetrwa zabicie procesu), potem dopiero rola SMS. */
    private fun finishSession(c: SmartEiClient, text: String) {
        c.post("/done", JSONObject())
        heartbeatOn = false
        RestoreState.markSmartEiDone(this, sessionKey, text, adb)
        ui { done(text, OK) }
        // Wi-Fi: oddanie roli SMS dopiero TERAZ - system może zabić proces agenta, ale komputer
        // ma już potwierdzenie. W trybie ADB rolę przywraca komputer przy sprzątaniu.
        if (!adb && DefaultSms.isDefault(this)) {
            ui { for (i in DefaultSms.restoreIntents(this)) if (tryStart(i)) break }
        }
    }

    private fun hello(): JSONObject {
        val root = DeviceDesc.storageRoot()
        val sizes = JSONObject()
        if (storageReadable()) {
            for ((key, paths) in CATEGORIES) {
                sizes.put(key, paths.sumOf { runCatching { Walker.size(root, it, EXCLUDE) }.getOrDefault(0L) })
            }
        }
        val apps = runCatching { AppList.collect(applicationContext) }.getOrDefault(emptyList())
        sizes.put("apks", apps.filter { !AppList.isPlay(it.installer) }.sumOf { it.size })
        return JSONObject()
            .put("device", DeviceDesc.json())
            .put("installed", JSONArray(AppList.installedPackages(applicationContext)))
            .put("freeBytes", DeviceDesc.freeBytes())
            .put("sizes", sizes)
            .put("agentVersion", BuildInfo.VERSION)
    }

    private fun waitJob(c: SmartEiClient, until: (JSONObject) -> Boolean): JSONObject {
        while (true) {
            val j = c.get("/job")
            if (j.optString("state") == "cancel") throw SmartEiCancelled()
            if (until(j)) return j
            Thread.sleep(1500)
        }
    }

    // ------------------------------------------------------------------ //
    // Eksport
    // ------------------------------------------------------------------ //

    private fun exportFlow(c: SmartEiClient, job: JSONObject) {
        val m = job.optJSONObject("modules") ?: JSONObject()
        val contacts = m.optBoolean("contacts")
        val sms = m.optBoolean("sms")
        val wallpaper = m.optBoolean("wallpaper")
        val files = m.optJSONArray("files") ?: JSONArray()
        ui { setIntro(tr("sei_export_intro")) }
        ask("start" to tr("sei_btn_start_export"))
        ui { busy(true) }

        val perms = mutableListOf<String>()
        if (contacts) perms += Manifest.permission.READ_CONTACTS
        if (sms) perms += Manifest.permission.READ_SMS
        if (wallpaper && Build.VERSION.SDK_INT in 23..32) perms += Manifest.permission.READ_EXTERNAL_STORAGE
        requestPerms(perms)
        if (files.length() > 0 && !adb) ensureStorage(write = false)

        val work = freshDir("out")
        val notes = JSONArray()
        val prog = Progress(c)

        if (contacts || sms) {
            ui { setStatus(tr("sei_step_contacts"), GOLD) }
            try {
                val r = Exporter(applicationContext, "smartei", { msg -> ui { setStatus(msg, GOLD) } }, { name, _, block ->
                    val f = File(work, name)
                    val n = Storage.CountingStream(FileOutputStream(f)).use { block(it) }
                    Storage.Written(name, f.length()) to n
                }, contacts, sms).run()
                for (w in r.files) c.uploadFile("contacts_sms/${w.name}", File(work, w.name))
                if (contacts) module(c, "contacts", !r.contactsDenied, r.contacts.toLong(), if (r.contactsDenied) "permission denied" else "")
                if (sms) module(c, "sms", !r.smsDenied, r.sms.toLong(), if (r.smsDenied) "permission denied" else "")
            } catch (e: SecurityException) {
                if (contacts) module(c, "contacts", false, 0, "permission denied")
                if (sms) module(c, "sms", false, 0, "permission denied")
            }
        }

        if (wallpaper) {
            ui { setStatus(tr("sei_step_wallpaper"), GOLD) }
            val e = Wallpapers.export(applicationContext, File(work, "wallpaper"))
            for ((name, f) in e.files) c.uploadFile("wallpaper/$name", f)
            module(c, "wallpaper", e.files.isNotEmpty(), e.files.size.toLong(), e.note)
            if (e.files.isEmpty()) ui { addDetail(tr("sei_wallpaper_denied")) }
        }

        if (m.optBoolean("settings")) {
            ui { setStatus(tr("sei_step_settings"), GOLD) }
            c.post("/export/settings", PortableSettingsIO.read(applicationContext))
        }

        if (m.optBoolean("apps")) {
            ui { setStatus(tr("sei_step_apps"), GOLD) }
            val list = AppList.collect(applicationContext)
            val resp = c.post("/export/apps", JSONObject().put("apps", JSONArray(list.map { it.json() })))
            val want = resp.optJSONArray("apks") ?: JSONArray()
            val todo = (0 until want.length()).mapNotNull { i -> list.find { it.pkg == want.optString(i) } }
            todo.forEachIndexed { i, app ->
                ui { setStatus(tr("sei_apk_fmt", i + 1, todo.size, app.label), GOLD) }
                prog.set("apks", i.toLong(), todo.size.toLong(), 0, 0, app.label, force = true)
                for (p in app.apks) {
                    val f = File(p)
                    if (f.canRead()) runCatching { c.uploadFile("apks/${app.pkg}/${f.name}", f) }
                        .onFailure { if (it is SmartEiCancelled) throw it; notes.put("apk ${app.pkg}: ${it.message}") }
                }
            }
        }

        if (files.length() > 0 && storageReadable()) {
            val root = DeviceDesc.storageRoot()
            val entries = mutableListOf<Walker.Entry>()
            for (i in 0 until files.length()) {
                val paths = files.optJSONObject(i)?.optJSONArray("paths") ?: continue
                for (j in 0 until paths.length()) entries += Walker.list(root, paths.optString(j), EXCLUDE)
            }
            val total = entries.size.toLong()
            val bytesTotal = entries.sumOf { it.file.length() }
            var bytes = 0L
            var failed = 0
            entries.forEachIndexed { i, e ->
                prog.set("files", i.toLong(), total, bytes, bytesTotal, e.rel)
                if (i % 25 == 0) ui { setStatus(tr("sei_files_fmt", i, total.toInt()), GOLD) }
                try {
                    val sent = c.uploadFile("files/${e.rel}", e.file) { n ->
                        bytes += n
                        prog.set("files", i.toLong(), total, bytes, bytesTotal, e.rel)
                    }
                    if (!sent) bytes += e.file.length() // już jest na komputerze (wznowienie)
                } catch (x: SmartEiCancelled) {
                    throw x
                } catch (x: Throwable) {
                    failed++
                    if (failed <= 20) notes.put("${e.rel}: ${x.message}")
                }
            }
            prog.set("files", total, total, bytesTotal, bytesTotal, null, force = true)
            if (failed > 0) notes.put("files failed: $failed")
        }

        c.post("/done", JSONObject().put("notes", notes))
        heartbeatOn = false
        RestoreState.markSmartEiDone(this, sessionKey, tr("sei_done_export"), adb)
        ui { done(tr("sei_done_export"), OK) }
    }

    // ------------------------------------------------------------------ //
    // Import
    // ------------------------------------------------------------------ //

    private fun importFlow(c: SmartEiClient) {
        val plan = c.get("/import/plan")
        ui { setIntro(tr("sei_import_intro")) }
        ask("start" to tr("sei_btn_start_import"))
        ui { busy(true) }
        val work = freshDir("in")
        val prog = Progress(c)

        // --- ADB: najpierw komputer (pliki, ustawienia, APK); potem PC wybudza ekran i wyciąga
        // agenta na pierwszy plan - okna systemu (import VCF, rola SMS) nie startują z tła
        var fallback = JSONArray()
        if (plan.optBoolean("adb")) {
            ui { setStatus(tr("sei_wait_pc"), GOLD) }
            val job = waitJob(c) { it.optBoolean("pcDone") || it.optString("state") == "finish" }
            fallback = job.optJSONArray("agentInstall") ?: JSONArray()
        }

        // --- kontakty / SMS: istniejący ekran przywracania agenta (pliki w folderze prywatnym)
        val vcf = plan.optString("contacts").takeIf { plan.has("contacts") && !plan.isNull("contacts") }
        val xml = plan.optString("sms").takeIf { plan.has("sms") && !plan.isNull("sms") }
        if (vcf != null || xml != null) {
            ui { setStatus(tr("sei_step_contacts"), GOLD) }
            val dir = File(work, "restore").apply { mkdirs() }
            vcf?.let { c.download(it, -1, File(dir, "contacts.vcf")) }
            xml?.let { c.download(it, -1, File(dir, "sms.xml")) }
            restoreReport = null
            RestoreState.clearLocalReport(this)
            interactive {
                ui {
                    @Suppress("DEPRECATION")
                    startActivityForResult(
                        Intent(this, MainActivity::class.java)
                            .putExtra("mode", "restore")
                            .putExtra("session", "smartei-" + System.currentTimeMillis())
                            .putExtra("localDir", dir.path),
                        REQ_RESTORE
                    )
                }
                await(setOf("restore"), TimeUnit.HOURS.toMillis(2))
            }
            val r = runCatching { JSONObject(restoreReport ?: "{}") }.getOrDefault(JSONObject())
            if (vcf != null) {
                val method = r.optString("contactsMethod", "none")
                val n = r.optLong("contactsImported", -1).takeIf { it >= 0 } ?: r.optLong("contactsCards", 0)
                module(c, "contacts", method != "none" && method.isNotEmpty(), n, method)
            }
            if (xml != null) {
                module(c, "sms", r.optBoolean("smsRestored"), r.optLong("smsInserted"), "skipped ${r.optLong("smsSkipped")}")
            }
            dir.deleteRecursively()
        }

        // --- tapeta
        plan.optJSONObject("wallpaper")?.let { wp ->
            ui { setStatus(tr("sei_step_wallpaper"), GOLD) }
            var n = 0
            val errs = mutableListOf<String>()
            for ((key, lock) in listOf("home" to false, "lock" to true)) {
                if (wp.isNull(key)) continue
                val f = File(work, "$key.png")
                runCatching {
                    c.download(wp.getString(key), -1, f)
                    Wallpapers.set(applicationContext, f, lock)
                    n++
                }.onFailure { if (it is SmartEiCancelled) throw it; errs += (it.message ?: it.javaClass.simpleName) }
            }
            module(c, "wallpaper", n > 0, n.toLong(), errs.joinToString("; "))
            if (n > 0) ui { addDetail(tr("sei_wallpaper_set")) }
        }

        // --- ustawienia (Wi-Fi; w trybie ADB ustawia je komputer)
        plan.optJSONObject("settings")?.let { s ->
            ui { setStatus(tr("sei_step_settings"), GOLD) }
            if (!PortableSettingsIO.canWrite(this) && Build.VERSION.SDK_INT >= 23) {
                ui { setIntro(tr("sei_write_settings")) }
                if (ask("allow" to tr("sei_btn_allow"), "skip" to tr("sei_btn_skip")) == "allow") {
                    interactive {
                        ui { tryStart(Intent(Settings.ACTION_MANAGE_WRITE_SETTINGS, Uri.parse("package:$packageName"))) }
                        await(setOf("resume"), TimeUnit.MINUTES.toMillis(10))
                    }
                }
            }
            val n = PortableSettingsIO.apply(applicationContext, s)
            module(c, "settings", n > 0, n.toLong(), if (s.has("uiNightMode")) "ui_night_mode: system only" else "")
        }

        // --- pliki
        val files = plan.optJSONArray("files") ?: JSONArray()
        val root = DeviceDesc.storageRoot()
        if (plan.optBoolean("download") && files.length() > 0) {
            ensureStorage(write = true)
            val total = files.length().toLong()
            var bytesTotal = 0L
            for (i in 0 until files.length()) bytesTotal += files.optJSONObject(i)?.optLong("size") ?: 0
            var bytes = 0L
            var failed = 0
            for (i in 0 until files.length()) {
                val f = files.optJSONObject(i) ?: continue
                val target = File(root, f.optString("target"))
                val size = f.optLong("size", -1)
                if (!target.canonicalPath.startsWith(root.canonicalPath + File.separator)) continue
                if (i % 25 == 0) ui { setStatus(tr("sei_files_fmt", i, total.toInt()), GOLD) }
                prog.set("files", i.toLong(), total, bytes, bytesTotal, f.optString("target"))
                if (target.isFile && target.length() == size) {
                    bytes += size
                    continue
                }
                try {
                    c.download(f.getString("path"), size, target) { n -> bytes += n; prog.set("files", i.toLong(), total, bytes, bytesTotal, null) }
                } catch (x: SmartEiCancelled) {
                    throw x
                } catch (x: Throwable) {
                    failed++
                    if (failed <= 5) c.post("/log", JSONObject().put("level", "warn").put("message", "${f.optString("target")}: ${x.message}"))
                }
            }
            prog.set("files", total, total, bytesTotal, bytesTotal, null, force = true)
            module(c, "files", failed == 0, total - failed, if (failed > 0) "failed: $failed" else "")
        }

        // --- własne APK (Wi-Fi: z komputera; ADB: plan B z Download/SmartEI_APKs)
        val apks = plan.optJSONArray("apks") ?: JSONArray()
        if (apks.length() > 0) installApks(c, apks, work, fromComputer = true)
        if (fallback.length() > 0) installApks(c, fallback, work, fromComputer = false)

        // --- skanowanie multimediów (galeria widzi zdjęcia od razu)
        if (files.length() > 0) {
            val paths = (0 until files.length()).mapNotNull { files.optJSONObject(it)?.optString("target") }
                .map { File(root, it).path }
            paths.chunked(400).forEach { runCatching { MediaScannerConnection.scanFile(applicationContext, it.toTypedArray(), null, null) } }
        }

        // --- kolejka Sklepu Play
        c.post("/stage", JSONObject().put("stage", "queue"))
        val play = plan.optJSONArray("play") ?: JSONArray()
        if (play.length() > 0) queueLoop(c)

        // --- koniec
        waitJob(c) { it.optString("state") == "finish" || it.optString("state") == "done" }
        ui { busy(false); setIntro(tr("sei_finish_ready")) }
        ask("finish" to tr("sei_btn_finish"))
        work.deleteRecursively()
        finishSession(c, tr("sei_done_import"))
    }

    private fun installApks(c: SmartEiClient, list: JSONArray, work: File, fromComputer: Boolean) {
        val inst = ApkInstaller(applicationContext)
        if (!inst.allowed() && Build.VERSION.SDK_INT >= 26) {
            ui { setIntro(tr("sei_install_perm")) }
            if (ask("allow" to tr("sei_btn_allow"), "skip" to tr("sei_btn_skip")) == "allow") {
                interactive {
                    ui { tryStart(Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:$packageName"))) }
                    await(setOf("resume"), TimeUnit.MINUTES.toMillis(10))
                }
            }
            if (!inst.allowed()) {
                module(c, "apks", false, 0, "install unknown apps not allowed")
                return
            }
        }
        var ok = 0
        for (i in 0 until list.length()) {
            val a = list.optJSONObject(i) ?: continue
            val label = a.optString("label", a.optString("package"))
            ui { setStatus(tr("sei_installing_fmt", label), GOLD) }
            val fl = a.optJSONArray("files") ?: continue
            val dir = File(work, "apks/" + a.optString("package").filter { it.isLetterOrDigit() || it == '.' || it == '_' })
            val local = mutableListOf<File>()
            try {
                for (j in 0 until fl.length()) {
                    val f = fl.optJSONObject(j) ?: continue
                    val path = f.optString("path")
                    if (fromComputer) {
                        val dest = File(dir, path.substringAfterLast('/'))
                        c.download(path, f.optLong("size", -1), dest)
                        local += dest
                    } else {
                        local += File(path)
                    }
                }
                val err = interactive { inst.install(local) }
                if (err == null) ok++ else c.post("/log", JSONObject().put("level", "warn").put("message", "$label: $err"))
            } catch (x: SmartEiCancelled) {
                throw x
            } catch (x: Throwable) {
                c.post("/log", JSONObject().put("level", "warn").put("message", "$label: ${x.message}"))
            } finally {
                if (fromComputer) dir.deleteRecursively()
            }
        }
        module(c, "apks", ok == list.length(), ok.toLong(), "agent")
    }

    private fun queueLoop(c: SmartEiClient) {
        var opened = ""
        ui { busy(false); setIntro(tr("sei_queue_hint")) }
        showButtons(listOf(Triple("open", tr("sei_btn_open_play"), true), Triple("next", tr("sei_btn_next"), false),
            Triple("skip", tr("sei_btn_skip"), false)))
        while (true) {
            val q = c.get("/queue")
            if (!q.optBoolean("active")) {
                if (c.get("/job").optString("state") == "finish") break
                Thread.sleep(1500)
                continue
            }
            if (q.optBoolean("finished")) break
            val cur = q.optJSONObject("current")
            val pkg = cur?.optString("package").orEmpty()
            val label = cur?.optString("label").orEmpty()
            ui { setStatus(tr("sei_queue_title", q.optInt("position"), q.optInt("total")) + "\n" + label, GOLD) }
            if (!adb && pkg.isNotEmpty() && pkg != opened) {
                opened = pkg
                ui { openPlay(pkg) }
            }
            if (!adb && pkg.isNotEmpty() && AppList.isInstalled(applicationContext, pkg)) {
                c.post("/queue/installed", JSONObject().put("package", pkg))
                continue
            }
            when (signals.poll(2, TimeUnit.SECONDS)) {
                "btn:open" -> {
                    ui { openPlay(pkg) }
                    showButtons(listOf(Triple("open", tr("sei_btn_open_play"), true), Triple("next", tr("sei_btn_next"), false),
                        Triple("skip", tr("sei_btn_skip"), false)))
                }
                "btn:next" -> {
                    c.post("/queue/next")
                    showButtons(listOf(Triple("open", tr("sei_btn_open_play"), true), Triple("next", tr("sei_btn_next"), false),
                        Triple("skip", tr("sei_btn_skip"), false)))
                }
                "btn:skip" -> {
                    c.post("/queue/skip")
                    showButtons(listOf(Triple("open", tr("sei_btn_open_play"), true), Triple("next", tr("sei_btn_next"), false),
                        Triple("skip", tr("sei_btn_skip"), false)))
                }
            }
        }
        ui { buttons.removeAllViews() }
    }

    private fun openPlay(pkg: String) {
        val market = Intent(Intent.ACTION_VIEW, Uri.parse("market://details?id=$pkg")).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        if (!tryStart(Intent(market).setPackage("com.android.vending")) && !tryStart(market)) {
            tryStart(Intent(Intent.ACTION_VIEW, Uri.parse("https://play.google.com/store/apps/details?id=$pkg")))
        }
    }

    // ------------------------------------------------------------------ //
    // Pomocnicze (wątek sesji)
    // ------------------------------------------------------------------ //

    /** Postęp do komputera - najwyżej co 0,7 s. */
    private inner class Progress(private val c: SmartEiClient) {
        private var last = 0L
        fun set(phase: String, done: Long, total: Long, bytes: Long, bytesTotal: Long, current: String?, force: Boolean = false) {
            val now = System.currentTimeMillis()
            if (!force && now - last < 700) return
            last = now
            val o = JSONObject().put("phase", phase).put("done", done).put("total", total)
                .put("bytesDone", bytes).put("bytesTotal", bytesTotal)
            if (current != null) o.put("current", current)
            runCatching { c.post("/progress", o) }.onFailure { if (it is SmartEiCancelled) throw it }
        }
    }

    private fun module(c: SmartEiClient, name: String, ok: Boolean, items: Long, note: String) {
        c.post("/export/module", JSONObject().put("name", name).put("ok", ok).put("items", items).put("note", note))
    }

    private fun freshDir(name: String): File = File(cacheDir, "smartei/$name").apply {
        deleteRecursively()
        mkdirs()
    }

    private fun granted(p: String) = Build.VERSION.SDK_INT < 23 || checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED

    private fun requestPerms(list: List<String>) {
        val missing = list.filterNot { granted(it) }
        if (missing.isEmpty() || Build.VERSION.SDK_INT < 23) return
        interactive {
            ui { requestPermissions(missing.toTypedArray(), REQ_PERMS) }
            await(setOf("perms"), TimeUnit.MINUTES.toMillis(10))
        }
    }

    private fun storageReadable(): Boolean = when {
        Build.VERSION.SDK_INT >= 30 -> Environment.isExternalStorageManager()
        else -> granted(Manifest.permission.READ_EXTERNAL_STORAGE)
    }

    private fun storageWritable(): Boolean = when {
        Build.VERSION.SDK_INT >= 30 -> Environment.isExternalStorageManager()
        else -> granted(Manifest.permission.WRITE_EXTERNAL_STORAGE)
    }

    /** Dostęp do pamięci (Android 11+: "dostęp do wszystkich plików"); użytkownik może pominąć. */
    private fun ensureStorage(write: Boolean) {
        repeat(3) {
            if (if (write) storageWritable() else storageReadable()) return
            ui { busy(false); setIntro(tr("sei_files_access")) }
            if (ask("allow" to tr("sei_btn_files_access"), "skip" to tr("sei_btn_skip_files")) != "allow") return
            if (Build.VERSION.SDK_INT >= 30) {
                interactive {
                    ui {
                        if (!tryStart(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, Uri.parse("package:$packageName")))) {
                            tryStart(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                        }
                    }
                    await(setOf("resume"), TimeUnit.MINUTES.toMillis(10))
                }
            } else {
                requestPerms(listOf(Manifest.permission.READ_EXTERNAL_STORAGE, Manifest.permission.WRITE_EXTERNAL_STORAGE))
            }
        }
    }

    /** Czeka na jeden z sygnałów (przycisk, wynik uprawnień, powrót z ustawień). */
    private fun await(names: Set<String>, timeoutMs: Long): String? {
        val end = System.currentTimeMillis() + timeoutMs
        while (true) {
            val left = end - System.currentTimeMillis()
            if (left <= 0) return null
            val s = signals.poll(left, TimeUnit.MILLISECONDS) ?: return null
            if (s in names) return s
        }
    }

    /** Przyciski -> id klikniętego. */
    private fun ask(vararg opts: Pair<String, String>): String {
        signals.clear()
        showButtons(opts.mapIndexed { i, (id, label) -> Triple(id, label, i == 0) })
        val got = await(opts.map { "btn:" + it.first }.toSet(), TimeUnit.HOURS.toMillis(3)) ?: throw IOException("timeout")
        ui { buttons.removeAllViews() }
        return got.removePrefix("btn:")
    }

    private fun ui(block: () -> Unit) = runOnUiThread(block)

    private fun tryStart(i: Intent): Boolean = try {
        startActivity(i)
        true
    } catch (_: Throwable) {
        false
    }

    // ------------------------------------------------------------------ //
    // UI
    // ------------------------------------------------------------------ //

    private fun showForm(error: String?) {
        form.visibility = View.VISIBLE
        buttons.removeAllViews()
        setIntro(tr("sei_pair_intro"))
        if (error != null) setStatus(error, ERR) else setStatus("", MUTED)
    }

    private fun onConnectClicked() {
        val a = SmartEiClient.parseAddress(address.text.toString())
        val k = pin.text.toString().filter { it.isDigit() }
        when {
            a == null -> setStatus(tr("sei_err_address"), ERR)
            k.length != 6 -> setStatus(tr("sei_err_pin"), ERR)
            else -> {
                adb = false
                start(a.first, a.second, k)
            }
        }
    }

    private fun done(text: String, color: Int) {
        busy(false)
        form.visibility = View.GONE
        setStatus(text, color)
        buttons.removeAllViews()
        if (!adb && DefaultSms.isDefault(this)) {
            buttons.addView(styledButton(tr("btn_default_sms_back"), true) {
                for (i in DefaultSms.restoreIntents(this)) if (tryStart(i)) break
            }, LinearLayout.LayoutParams(-1, dp(56)).apply { topMargin = dp(12) })
            addDetail(tr("rs_default_back"))
        }
        if (!adb) {
            buttons.addView(styledButton(tr("sei_btn_uninstall"), false) {
                tryStart(Intent(Intent.ACTION_DELETE, Uri.parse("package:$packageName")))
            }, LinearLayout.LayoutParams(-1, dp(56)).apply { topMargin = dp(12) })
        }
    }

    private fun showNewConnectionButton() {
        buttons.addView(styledButton(tr("sei_btn_connect"), false) {
            RestoreState.clearSmartEi(this)
            detail.text = ""
            detail.visibility = View.GONE
            showForm(null)
        }, LinearLayout.LayoutParams(-1, dp(56)).apply { topMargin = dp(12) })
    }

    private fun showButtons(list: List<Triple<String, String, Boolean>>) = ui {
        buttons.removeAllViews()
        for ((id, label, filled) in list) {
            buttons.addView(styledButton(label, filled) { signals.offer("btn:$id") },
                LinearLayout.LayoutParams(-1, dp(56)).apply { topMargin = dp(12) })
        }
    }

    private fun busy(on: Boolean) {
        progress.visibility = if (on) View.VISIBLE else View.GONE
    }

    private fun setIntro(t: String) {
        intro.text = t
    }

    private fun setStatus(t: String, color: Int) {
        status.text = t
        status.setTextColor(color)
    }

    private fun addDetail(t: String) {
        detail.visibility = View.VISIBLE
        detail.text = if (detail.text.isNullOrEmpty()) "• $t" else "${detail.text}\n• $t"
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()

    private fun styledButton(label: String, filled: Boolean, onClick: () -> Unit): Button = Button(this).apply {
        text = label
        textSize = if (filled) 17f else 15f
        typeface = Typeface.DEFAULT_BOLD
        isAllCaps = false
        setTextColor(if (filled) Color.BLACK else GOLD)
        background = GradientDrawable().apply {
            cornerRadius = dp(12).toFloat()
            if (filled) setColor(GOLD) else {
                setColor(Color.rgb(20, 20, 20))
                setStroke(dp(2), GOLD)
            }
        }
        setOnClickListener { onClick() }
    }

    private fun field(hint: String, numeric: Boolean): EditText = EditText(this).apply {
        this.hint = hint
        setHintTextColor(MUTED)
        setTextColor(TEXT)
        textSize = 18f
        typeface = Typeface.MONOSPACE
        gravity = Gravity.CENTER
        isSingleLine = true
        inputType = if (numeric) InputType.TYPE_CLASS_NUMBER else InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
        setPadding(dp(12), dp(12), dp(12), dp(12))
        background = GradientDrawable().apply {
            cornerRadius = dp(10).toFloat()
            setColor(Color.rgb(18, 18, 18))
            setStroke(dp(1), Color.rgb(80, 80, 80))
        }
    }

    private fun buildUi(): View {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(28), dp(40), dp(28), dp(32))
            setBackgroundColor(BG)
        }
        val logo = TextView(this).apply {
            text = "EI"
            textSize = 26f
            typeface = Typeface.DEFAULT_BOLD
            gravity = Gravity.CENTER
            setTextColor(GOLD)
            background = GradientDrawable().apply {
                cornerRadius = dp(14).toFloat()
                setColor(Color.BLACK)
                setStroke(dp(2), GOLD)
            }
        }
        root.addView(logo, LinearLayout.LayoutParams(dp(64), dp(64)).apply { bottomMargin = dp(16) })
        root.addView(TextView(this).apply {
            text = tr("sei_title")
            textSize = 22f
            typeface = Typeface.DEFAULT_BOLD
            gravity = Gravity.CENTER
            setTextColor(GOLD)
        })
        root.addView(TextView(this).apply {
            text = tr("sei_subtitle")
            textSize = 13f
            gravity = Gravity.CENTER
            setTextColor(MUTED)
        })
        intro = TextView(this).apply {
            textSize = 14f
            gravity = Gravity.CENTER
            setLineSpacing(0f, 1.15f)
            setTextColor(TEXT)
        }
        root.addView(intro, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(20) })

        form = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        address = field(tr("sei_hint_address"), false)
        pin = field(tr("sei_hint_pin"), true)
        form.addView(address, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(18) })
        form.addView(pin, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(12) })
        form.addView(styledButton(tr("sei_btn_connect"), true) { onConnectClicked() },
            LinearLayout.LayoutParams(-1, dp(58)).apply { topMargin = dp(18) })
        form.visibility = View.GONE
        root.addView(form, LinearLayout.LayoutParams(-1, -2))

        buttons = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        root.addView(buttons, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(12) })

        progress = ProgressBar(this).apply {
            isIndeterminate = true
            visibility = View.GONE
        }
        root.addView(progress, LinearLayout.LayoutParams(dp(40), dp(40)).apply { topMargin = dp(20) })
        status = TextView(this).apply {
            textSize = 15f
            gravity = Gravity.CENTER
            setLineSpacing(0f, 1.15f)
        }
        root.addView(status, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(16) })
        detail = TextView(this).apply {
            textSize = 13f
            setTextColor(MUTED)
            visibility = View.GONE
        }
        root.addView(detail, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(12) })
        return ScrollView(this).apply {
            isFillViewport = true
            setBackgroundColor(BG)
            addView(root)
        }
    }

    companion object {
        private const val REQ_PERMS = 61
        private const val REQ_RESTORE = 62

        /** Te same kategorie co FILE_CATEGORIES w komputerze. */
        val CATEGORIES = listOf(
            "dcim" to listOf("DCIM"),
            "pictures" to listOf("Pictures"),
            "movies" to listOf("Movies"),
            "music" to listOf("Music"),
            "download" to listOf("Download"),
            "documents" to listOf("Documents"),
            "whatsapp" to listOf("Android/media/com.whatsapp/WhatsApp/Media", "WhatsApp/Media")
        )
        val EXCLUDE = setOf("Backup", "Restore", "SmartEI_APKs", "Agent.apk")

        private val BG = Color.rgb(10, 10, 10)
        private val GOLD = Color.rgb(0xC9, 0xA2, 0x49)
        private val TEXT = Color.rgb(0xD8, 0xD8, 0xD8)
        private val MUTED = Color.rgb(0x80, 0x80, 0x80)
        private val OK = Color.rgb(0x7F, 0xD0, 0x8A)
        private val WARN = Color.rgb(0xD9, 0xB4, 0x66)
        private val ERR = Color.rgb(0xE0, 0x8A, 0x8A)
    }
}
