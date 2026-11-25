package com.drapco.agent

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.graphics.Color
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Environment
import android.provider.Settings
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.style.ForegroundColorSpan
import android.view.Gravity
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ProgressBar
import android.widget.ScrollView
import android.widget.TextView
import org.json.JSONObject
import java.io.File
import java.util.concurrent.Executors

/**
 * Ekran agenta. Dwa tryby (wybiera komputer):
 *
 * KOPIA (domyślny): `am start -n com.drapco.agent/.MainActivity --es session <id>`
 *  1. systemowe okna READ_CONTACTS / READ_SMS,
 *  2. przycisk "Wykonaj kopię" -> Download/Backup/<id>/ (contacts.vcf, sms.xml, done.json),
 *  3. komputer pobiera pliki, sprząta (CleanupReceiver) i odinstalowuje agenta.
 *
 * PRZYWRACANIE: `... --es session <id> --es mode restore`
 *  Komputer wgrał wcześniej Download/Restore/<id>/contacts.vcf i sms.xml.
 *  - Kontakty: systemowy import aplikacji Kontakty (ACTION_VIEW text/x-vcard);
 *    gdy jej brak - import bezpośredni (WRITE_CONTACTS).
 *  - SMS: agent na chwilę zostaje domyślną aplikacją SMS (tylko ona może pisać
 *    do bazy wiadomości), wstawia wiadomości przez ContentResolver, a potem
 *    prosi o przywrócenie poprzedniej aplikacji SMS.
 *  - "Gotowe" -> restore_done.json; komputer sprząta i odinstalowuje agenta.
 *
 * Wszystkie teksty pochodzą z res/values(-xx)/strings.xml (20 języków, EN domyślny).
 * UI budowane w kodzie (bez AndroidX/layoutów) - mały, samodzielny APK.
 */
class MainActivity : Activity() {

    private lateinit var titleView: TextView
    private lateinit var sessionView: TextView
    private lateinit var introView: TextView
    private lateinit var status: TextView
    private lateinit var report: TextView
    private lateinit var button: Button
    private lateinit var progress: ProgressBar
    private lateinit var restorePanel: LinearLayout
    private lateinit var btnContacts: Button
    private lateinit var btnSms: Button
    private lateinit var btnDefaultBack: Button
    private lateinit var btnDone: Button

    private var session: String = ""
    private var restoreMode = false
    /** Smart EI: pliki kopii w prywatnym folderze agenta (bez znaczników w Download/). */
    private var localDir: File? = null
    private val worker = Executors.newSingleThreadExecutor()

    @Volatile
    private var running = false

    // --- stan przywracania
    private var files: RestoreFiles? = null
    private var contactsCards = 0
    private var smsCounts: SmsRestorer.Counts? = null
    private var contactsMethod = "none"
    private var contactsImported = -1
    private var smsResult: SmsRestorer.Result? = null
    private var defaultTaken = false
    private var defaultRestored = false
    private var doneWarned = false
    private var finished = false
    private val reportLines = LinkedHashMap<String, Pair<String, Int>>()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        readIntent(intent)
        showOverLockScreen()
        start()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        if (running) return
        setIntent(intent)
        readIntent(intent)
        start()
    }

    private fun readIntent(i: Intent?) {
        session = sessionFrom(i)
        restoreMode = i?.getStringExtra("mode") == "restore"
        localDir = i?.getStringExtra("localDir")?.let { File(it) }?.takeIf { d ->
            // tylko własne foldery prywatne agenta - nic z zewnątrz
            val c = runCatching { d.canonicalPath }.getOrDefault("")
            c.startsWith(cacheDir.canonicalPath + File.separator) || c.startsWith(filesDir.canonicalPath + File.separator)
        }
    }

    private fun restoreFiles(): RestoreFiles = localDir?.let { RestoreFiles(it) } ?: RestoreFiles.forSession(session)

    private fun start() {
        files = null
        smsResult = null
        contactsMethod = "none"
        contactsImported = -1
        defaultTaken = false
        defaultRestored = false
        doneWarned = false
        finished = false
        reportLines.clear()
        // Smart EI: proces mógł zostać zabity i wznowiony przez system po zakończeniu -> raport od razu
        if (restoreMode && localDir != null) {
            RestoreState.localReport(this, localDir!!.path)?.let { rep ->
                setResult(RESULT_OK, Intent().putExtra("report", rep))
                finish()
                return
            }
        }
        setContentView(buildUi())
        if (restoreMode) {
            loadState()
            initRestore(askForAccess = true)
        } else {
            updateIdleStatus()
            requestMissingPermissions()
        }
    }

    override fun onResume() {
        super.onResume()
        if (running) return
        if (!restoreMode) {
            updateIdleStatus()
            return
        }
        // powrót z ustawień: dostęp do plików / zmiana domyślnej aplikacji SMS
        if (files == null && !finished && restoreFiles().readable()) initRestore(askForAccess = false)
        if (defaultTaken && !defaultRestored && !DefaultSms.isDefault(this)) {
            defaultRestored = true
            addReport("default", tr("rs_default_back_ok"), OK)
            setStatus(tr("rs_finish_hint"), OK)
        }
        updateRestoreButtons()
    }

    override fun onDestroy() {
        worker.shutdown()
        super.onDestroy()
    }

    // ------------------------------------------------------------------ //
    // Uprawnienia
    // ------------------------------------------------------------------ //

    private fun neededPermissions(): List<String> {
        val list = mutableListOf(Manifest.permission.READ_CONTACTS, Manifest.permission.READ_SMS)
        if (Build.VERSION.SDK_INT <= 28) list += Manifest.permission.WRITE_EXTERNAL_STORAGE
        return list
    }

    private fun isGranted(p: String): Boolean =
        Build.VERSION.SDK_INT < 23 || checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED

    private fun missingPermissions(): List<String> = neededPermissions().filterNot { isGranted(it) }

    private fun requestMissingPermissions() {
        val missing = missingPermissions()
        if (Build.VERSION.SDK_INT >= 23 && missing.isNotEmpty()) {
            requestPermissions(missing.toTypedArray(), REQ_PERMS)
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        when (requestCode) {
            REQ_PERMS -> updateIdleStatus()
            REQ_FILES -> if (restoreFiles().readable()) initRestore(askForAccess = false)
            REQ_CONTACTS -> if (isGranted(Manifest.permission.WRITE_CONTACTS)) {
                importContactsDirect()
            } else {
                setStatus(tr("rs_need_contacts_perm"), WARN)
                if (permanentlyDenied(Manifest.permission.WRITE_CONTACTS)) openAppSettings()
            }
            REQ_SMS_READ -> restoreSmsWork() // bez READ_SMS działa, tylko bez pomijania duplikatów
        }
    }

    /** Odmowa "na stałe" (Nie pytaj ponownie) -> systemowe okno już się nie pokaże. */
    private fun permanentlyDenied(p: String): Boolean =
        Build.VERSION.SDK_INT >= 23 && !isGranted(p) && !shouldShowRequestPermissionRationale(p)

    private fun openAppSettings() {
        tryStart(Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", packageName, null)))
    }

    private fun tryStart(i: Intent): Boolean = try {
        startActivity(i)
        true
    } catch (_: Throwable) {
        false
    }

    // ------------------------------------------------------------------ //
    // Kopia
    // ------------------------------------------------------------------ //

    private fun onBackupClicked() {
        if (running) return
        val missing = missingPermissions()
        val storageMissing = Build.VERSION.SDK_INT <= 28 && Manifest.permission.WRITE_EXTERNAL_STORAGE in missing
        val nothingReadable = Manifest.permission.READ_CONTACTS in missing && Manifest.permission.READ_SMS in missing
        if (storageMissing || nothingReadable) {
            if (missing.all { permanentlyDenied(it) } && missing.isNotEmpty()) {
                setStatus(tr("st_perm_blocked"), ERR)
                openAppSettings()
            } else {
                setStatus(tr("st_grant_dialogs"), WARN)
                requestMissingPermissions()
            }
            return
        }

        running = true
        button.isEnabled = false
        button.alpha = 0.5f
        progress.visibility = View.VISIBLE
        setStatus(tr("st_working"), GOLD)

        val currentSession = session
        worker.execute {
            try {
                val result = Exporter(applicationContext, currentSession, progress = { msg ->
                    runOnUiThread { setStatus(msg, GOLD) }
                }).run()
                Markers.writeDone(applicationContext, currentSession, result)
                runOnUiThread { onDone(result) }
            } catch (t: Throwable) {
                val msg = t.message ?: t.javaClass.simpleName
                try {
                    Markers.writeError(applicationContext, currentSession, msg)
                } catch (_: Throwable) {
                    // brak możliwości zapisu znacznika - komputer zgłosi przekroczenie czasu
                }
                runOnUiThread { onFailed(msg) }
            }
        }
    }

    private fun onDone(r: Exporter.Result) {
        running = false
        progress.visibility = View.GONE
        val parts = mutableListOf<String>()
        parts += if (r.contactsDenied) tr("part_contacts_denied") else tr("part_contacts_fmt", r.contacts)
        parts += if (r.smsDenied) tr("part_sms_denied") else tr("part_sms_fmt", r.sms)
        setStatus(tr("st_done_fmt", parts.joinToString(", ")), OK)
        button.text = tr("btn_backup_done")
    }

    private fun onFailed(msg: String) {
        running = false
        progress.visibility = View.GONE
        button.isEnabled = true
        button.alpha = 1f
        setStatus(tr("err_backup_fmt", msg), ERR)
    }

    private fun updateIdleStatus() {
        if (restoreMode) return
        val missing = missingPermissions()
        when {
            missing.isEmpty() -> setStatus(tr("st_perms_ok"), OK)
            Manifest.permission.READ_CONTACTS in missing && Manifest.permission.READ_SMS in missing ->
                setStatus(tr("st_need_both"), WARN)
            Manifest.permission.READ_SMS in missing -> setStatus(tr("st_no_sms"), WARN)
            Manifest.permission.READ_CONTACTS in missing -> setStatus(tr("st_no_contacts"), WARN)
            else -> setStatus(tr("st_no_storage"), WARN)
        }
    }

    // ------------------------------------------------------------------ //
    // Przywracanie
    // ------------------------------------------------------------------ //

    private fun initRestore(askForAccess: Boolean) {
        val f = restoreFiles()
        if (!f.readable()) {
            files = null
            setStatus(tr("rs_need_files"), WARN)
            if (askForAccess) requestFileAccess()
            updateRestoreButtons()
            return
        }
        files = f
        if (f.contacts == null && f.sms == null) {
            setStatus(tr("rs_none"), ERR)
            updateRestoreButtons()
            return
        }
        busy(true)
        worker.execute {
            val cards = f.contacts?.let { runCatching { Vcf.count(it) }.getOrDefault(0) } ?: 0
            val counts = f.sms?.let { runCatching { SmsRestorer(applicationContext) { _, _ -> }.count(it) } }
            runOnUiThread {
                busy(false)
                contactsCards = cards
                smsCounts = counts?.getOrNull()
                val parts = mutableListOf<String>()
                if (f.contacts != null) parts += tr("part_contacts_fmt", cards)
                if (f.sms != null) parts += tr("part_sms_fmt", smsCounts?.sms ?: 0)
                setStatus(tr("rs_files_fmt", parts.joinToString(", ")) + "\n" + tr("rs_finish_hint"), OK)
                counts?.exceptionOrNull()?.let { addReport("sms", tr("rs_error_fmt", errText(it)), ERR) }
                updateRestoreButtons()
            }
        }
    }

    private fun requestFileAccess() {
        if (Build.VERSION.SDK_INT >= 30) {
            if (!Environment.isExternalStorageManager()) {
                val pkg = Uri.parse("package:$packageName")
                if (!tryStart(Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION, pkg))) {
                    tryStart(Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION))
                }
            }
        } else if (Build.VERSION.SDK_INT >= 23) {
            val perms = mutableListOf(Manifest.permission.READ_EXTERNAL_STORAGE)
            if (Build.VERSION.SDK_INT <= 28) perms += Manifest.permission.WRITE_EXTERNAL_STORAGE
            val missing = perms.filterNot { isGranted(it) }
            if (missing.isNotEmpty()) requestPermissions(missing.toTypedArray(), REQ_FILES)
        }
    }

    // --- kontakty

    private fun onRestoreContacts() {
        val src = files?.contacts ?: return
        if (running || finished) return
        busy(true)
        worker.execute {
            val staged = runCatching {
                val dir = VcfProvider.dir(applicationContext)
                dir.mkdirs()
                val dst = File(dir, "contacts.vcf")
                src.copyTo(dst, overwrite = true)
                dst
            }
            runOnUiThread {
                busy(false)
                val dst = staged.getOrElse {
                    addReport("contacts", tr("rs_error_fmt", errText(it)), ERR)
                    return@runOnUiThread
                }
                val view = Intent(Intent.ACTION_VIEW)
                    .setDataAndType(VcfProvider.uriFor(this, dst), VcfProvider.MIME)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                try {
                    startActivity(view)
                    contactsMethod = "intent"
                    saveState()
                    addReport("contacts", tr("rs_contacts_started_fmt", contactsCards), GOLD)
                    setStatus(tr("rs_finish_hint"), OK)
                } catch (_: ActivityNotFoundException) {
                    importContactsDirect()
                } catch (_: SecurityException) {
                    importContactsDirect()
                }
            }
        }
    }

    /** Brak aplikacji obsługującej .vcf - import własny (WRITE_CONTACTS). */
    private fun importContactsDirect() {
        val src = files?.contacts ?: return
        if (!isGranted(Manifest.permission.WRITE_CONTACTS)) {
            setStatus(tr("rs_need_contacts_perm"), WARN)
            if (Build.VERSION.SDK_INT >= 23) {
                requestPermissions(
                    arrayOf(Manifest.permission.WRITE_CONTACTS, Manifest.permission.READ_CONTACTS),
                    REQ_CONTACTS
                )
            }
            return
        }
        busy(true)
        worker.execute {
            val r = runCatching {
                VcfImporter(applicationContext) { d, t ->
                    runOnUiThread { setStatus(tr("rs_contacts_importing_fmt", d, t), GOLD) }
                }.import(src)
            }
            runOnUiThread {
                busy(false)
                r.onSuccess { n ->
                    contactsMethod = "direct"
                    contactsImported = n
                    saveState()
                    addReport("contacts", tr("rs_contacts_done_fmt", n), OK)
                    setStatus(tr("rs_finish_hint"), OK)
                }.onFailure { addReport("contacts", tr("rs_error_fmt", errText(it)), ERR) }
                updateRestoreButtons()
            }
        }
    }

    // --- SMS

    private fun onRestoreSms() {
        if (files?.sms == null || running || finished) return
        if (DefaultSms.isDefault(this)) {
            defaultTaken = true
            saveState()
            restoreSms()
            return
        }
        val req = DefaultSms.requestIntent(this)
        if (req == null) {
            addReport("sms", tr("rs_default_denied"), ERR)
            return
        }
        DefaultSms.rememberPrevious(this)
        setStatus(tr("rs_need_default"), GOLD)
        try {
            startActivityForResult(req, REQ_ROLE)
        } catch (_: Throwable) {
            defaultDenied()
        }
    }

    @Deprecated("Activity API (bez AndroidX)")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQ_ROLE) return
        if (DefaultSms.isDefault(this)) {
            defaultTaken = true
            saveState()
            restoreSms()
        } else {
            defaultDenied()
        }
    }

    private fun defaultDenied() {
        var msg = tr("rs_default_denied")
        if (Build.VERSION.SDK_INT >= 33) msg += "\n" + tr("rs_restricted_hint")
        setStatus(msg, ERR)
    }

    private fun restoreSms() {
        // READ_SMS do pomijania duplikatów (Android 10+ nadaje je razem z rolą SMS)
        if (Build.VERSION.SDK_INT >= 23 && !isGranted(Manifest.permission.READ_SMS)) {
            requestPermissions(arrayOf(Manifest.permission.READ_SMS), REQ_SMS_READ)
            return
        }
        restoreSmsWork()
    }

    private fun restoreSmsWork() {
        val src = files?.sms ?: return
        if (running) return
        val total = smsCounts?.sms ?: 0
        busy(true)
        setStatus(tr("rs_sms_progress_fmt", 0, total), GOLD)
        worker.execute {
            val r = runCatching {
                SmsRestorer(applicationContext) { d, t ->
                    runOnUiThread { setStatus(tr("rs_sms_progress_fmt", d, t), GOLD) }
                }.restore(src, total)
            }
            runOnUiThread {
                busy(false)
                r.onSuccess { res ->
                    smsResult = res
                    saveState()
                    addReport("sms", tr("rs_sms_done_fmt", res.inserted, res.skipped), OK)
                    if (res.mms > 0) addReport("mms", tr("rs_mms_skipped_fmt", res.mms), WARN)
                    if (DefaultSms.isDefault(this)) {
                        addReport("default", tr("rs_default_back"), WARN)
                        setStatus(tr("rs_default_back"), WARN)
                    }
                }.onFailure { addReport("sms", tr("rs_error_fmt", errText(it)), ERR) }
                updateRestoreButtons()
            }
        }
    }

    private fun onDefaultBack() {
        for (i in DefaultSms.restoreIntents(this)) if (tryStart(i)) return
    }

    // --- Gotowe

    private fun onRestoreDone() {
        if (running || finished) return
        if (localDir == null && defaultTaken && DefaultSms.isDefault(this) && !doneWarned) {
            doneWarned = true
            setStatus(tr("rs_still_default"), WARN)
            return
        }
        finished = true
        busy(true)
        val accessible = files != null
        val json = summary()
        if (localDir != null) {
            // Smart EI: raport wraca do ekranu Smart EI. Domyślną aplikację SMS przywracamy dopiero
            // PO potwierdzeniu zakończenia do komputera (utrata roli SMS = system zabija proces agenta).
            json.put("defaultSmsStillAgent", DefaultSms.isDefault(this))
            RestoreState.saveLocalReport(this, localDir!!.path, json.toString())
            setResult(RESULT_OK, Intent().putExtra("report", json.toString()))
            finish()
            return
        }
        val currentSession = session
        worker.execute {
            val r = runCatching {
                if (accessible) {
                    RestoreMarkers.writeDone(applicationContext, currentSession, json)
                } else {
                    RestoreMarkers.writeError(
                        applicationContext,
                        currentSession,
                        "Agent has no access to the restore files (files access not granted)"
                    )
                }
            }
            runOnUiThread {
                busy(false)
                r.onSuccess { setStatus(tr("rs_done_sent"), OK) }
                    .onFailure {
                        finished = false
                        setStatus(tr("rs_error_fmt", errText(it)), ERR)
                    }
                updateRestoreButtons()
            }
        }
    }

    /** Stan przywracania w SharedPreferences - przetrwa zabicie procesu przez system (np. po utracie roli SMS). */
    private fun saveState() {
        val sms = smsResult
        val o = JSONObject()
            .put("contactsMethod", contactsMethod)
            .put("contactsImported", contactsImported)
            .put("contactsCards", contactsCards)
            .put("defaultTaken", defaultTaken)
        if (sms != null) {
            o.put("sms", JSONObject().put("total", sms.total).put("inserted", sms.inserted)
                .put("skipped", sms.skipped).put("failed", sms.failed).put("mms", sms.mms))
        }
        RestoreState.save(this, session, o)
    }

    private fun loadState() {
        val o = RestoreState.load(this, session) ?: return
        contactsMethod = o.optString("contactsMethod", "none")
        contactsImported = o.optInt("contactsImported", -1)
        contactsCards = o.optInt("contactsCards", contactsCards)
        defaultTaken = o.optBoolean("defaultTaken")
        o.optJSONObject("sms")?.let {
            smsResult = SmsRestorer.Result(it.optInt("total"), it.optInt("inserted"), it.optInt("skipped"),
                it.optInt("failed"), it.optInt("mms"))
        }
        when (contactsMethod) {
            "intent" -> addReport("contacts", tr("rs_contacts_started_fmt", contactsCards), GOLD)
            "direct" -> addReport("contacts", tr("rs_contacts_done_fmt", contactsImported), OK)
        }
        smsResult?.let { addReport("sms", tr("rs_sms_done_fmt", it.inserted, it.skipped), OK) }
        if (defaultTaken && !DefaultSms.isDefault(this)) {
            defaultRestored = true
            addReport("default", tr("rs_default_back_ok"), OK)
        }
    }

    private fun summary(): JSONObject {
        val sms = smsResult
        return JSONObject()
            .put("contactsFile", files?.contacts != null)
            .put("contactsCards", contactsCards)
            .put("contactsMethod", contactsMethod)
            .put("contactsImported", contactsImported)
            .put("smsFile", files?.sms != null)
            .put("smsTotal", sms?.total ?: (smsCounts?.sms ?: 0))
            .put("smsInserted", sms?.inserted ?: 0)
            .put("smsSkipped", sms?.skipped ?: 0)
            .put("smsFailed", sms?.failed ?: 0)
            .put("mmsSkipped", sms?.mms ?: (smsCounts?.mms ?: 0))
            .put("smsRestored", sms != null)
            .put("defaultSmsChanged", defaultTaken)
            .put("defaultSmsRestored", !DefaultSms.isDefault(this))
            .put("previousSmsApp", DefaultSms.previous(this) ?: "")
            .put("incomingSmsSaved", IncomingStats.get(this, "incoming_sms_saved"))
            .put("incomingSmsLost", IncomingStats.get(this, "incoming_sms_lost"))
            .put("incomingMms", IncomingStats.get(this, "incoming_mms"))
    }

    private fun errText(t: Throwable): String = t.message ?: t.javaClass.simpleName

    // ------------------------------------------------------------------ //
    // UI
    // ------------------------------------------------------------------ //

    private fun busy(on: Boolean) {
        running = on
        progress.visibility = if (on) View.VISIBLE else View.GONE
        if (restoreMode) updateRestoreButtons()
    }

    private fun updateRestoreButtons() {
        if (!restoreMode) return
        val f = files
        fun Button.state(visible: Boolean, enabled: Boolean) {
            visibility = if (visible) View.VISIBLE else View.GONE
            isEnabled = enabled
            alpha = if (enabled) 1f else 0.45f
        }
        val idle = !running && !finished
        btnContacts.state(f?.contacts != null, idle)
        btnSms.state(f?.sms != null && smsCounts != null, idle && smsResult == null)
        btnDefaultBack.state(localDir == null && defaultTaken && !defaultRestored && smsResult != null, !finished)
        btnDone.state(true, idle)
    }

    private fun addReport(key: String, text: String, color: Int) {
        reportLines[key] = text to color
        val sb = SpannableStringBuilder()
        for ((t, c) in reportLines.values) {
            if (sb.isNotEmpty()) sb.append("\n")
            val start = sb.length
            sb.append("• ").append(t)
            sb.setSpan(ForegroundColorSpan(c), start, sb.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
        }
        report.text = sb
        report.visibility = View.VISIBLE
    }

    private fun setStatus(text: String, color: Int) {
        status.text = text
        status.setTextColor(color)
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

    private fun buildUi(): View {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(dp(28), dp(48), dp(28), dp(32))
            setBackgroundColor(BG)
        }

        val logo = TextView(this).apply {
            text = "D"
            textSize = 34f
            typeface = Typeface.DEFAULT_BOLD
            gravity = Gravity.CENTER
            setTextColor(GOLD)
            background = GradientDrawable().apply {
                cornerRadius = dp(14).toFloat()
                setColor(Color.BLACK)
                setStroke(dp(2), GOLD)
            }
        }
        root.addView(logo, LinearLayout.LayoutParams(dp(68), dp(68)).apply { bottomMargin = dp(20) })

        titleView = TextView(this).apply {
            text = tr(if (restoreMode) "title_restore" else "title_backup")
            textSize = 22f
            typeface = Typeface.DEFAULT_BOLD
            gravity = Gravity.CENTER
            setTextColor(GOLD)
        }
        root.addView(titleView)

        sessionView = TextView(this).apply {
            text = tr("session_fmt", session)
            textSize = 12f
            gravity = Gravity.CENTER
            setTextColor(MUTED)
            typeface = Typeface.MONOSPACE
        }
        root.addView(sessionView, LinearLayout.LayoutParams(-2, -2).apply { topMargin = dp(6) })

        introView = TextView(this).apply {
            text = tr(if (restoreMode) "intro_restore" else "intro_backup")
            textSize = 14f
            gravity = Gravity.CENTER
            setLineSpacing(0f, 1.15f)
            setTextColor(TEXT)
        }
        root.addView(introView, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(22) })

        button = styledButton(tr("btn_backup"), true) { onBackupClicked() }
        root.addView(button, LinearLayout.LayoutParams(-1, dp(60)).apply { topMargin = dp(30) })

        restorePanel = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        btnContacts = styledButton(tr("btn_restore_contacts"), false) { onRestoreContacts() }
        btnSms = styledButton(tr("btn_restore_sms"), false) { onRestoreSms() }
        btnDefaultBack = styledButton(tr("btn_default_sms_back"), false) { onDefaultBack() }
        btnDone = styledButton(tr("btn_done"), true) { onRestoreDone() }
        for (b in listOf(btnContacts, btnSms, btnDefaultBack, btnDone)) {
            restorePanel.addView(b, LinearLayout.LayoutParams(-1, dp(56)).apply { topMargin = dp(12) })
        }
        root.addView(restorePanel, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(18) })

        if (restoreMode) button.visibility = View.GONE else restorePanel.visibility = View.GONE

        progress = ProgressBar(this).apply {
            isIndeterminate = true
            visibility = View.GONE
        }
        root.addView(progress, LinearLayout.LayoutParams(dp(40), dp(40)).apply { topMargin = dp(22) })

        status = TextView(this).apply {
            textSize = 15f
            gravity = Gravity.CENTER
            setLineSpacing(0f, 1.15f)
            setOnClickListener { if (restoreMode && files == null) requestFileAccess() }
        }
        root.addView(status, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(18) })

        report = TextView(this).apply {
            textSize = 14f
            setLineSpacing(0f, 1.2f)
            visibility = View.GONE
            setPadding(dp(14), dp(12), dp(14), dp(12))
            background = GradientDrawable().apply {
                cornerRadius = dp(10).toFloat()
                setColor(Color.rgb(18, 18, 18))
                setStroke(dp(1), Color.rgb(60, 60, 60))
            }
        }
        root.addView(report, LinearLayout.LayoutParams(-1, -2).apply { topMargin = dp(16) })

        updateRestoreButtons()
        return ScrollView(this).apply {
            isFillViewport = true
            setBackgroundColor(BG)
            addView(root)
        }
    }

    /** Okno widoczne także na ekranie blokady i wybudzające ekran (telefon na biurku). */
    @Suppress("DEPRECATION")
    private fun showOverLockScreen() {
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        if (Build.VERSION.SDK_INT >= 27) {
            setShowWhenLocked(true)
            setTurnScreenOn(true)
        } else {
            window.addFlags(
                WindowManager.LayoutParams.FLAG_SHOW_WHEN_LOCKED or WindowManager.LayoutParams.FLAG_TURN_SCREEN_ON
            )
        }
    }

    companion object {
        private const val REQ_PERMS = 42
        private const val REQ_FILES = 43
        private const val REQ_CONTACTS = 44
        private const val REQ_SMS_READ = 45
        private const val REQ_ROLE = 46

        private val BG = Color.rgb(10, 10, 10)
        private val GOLD = Color.rgb(0xC9, 0xA2, 0x49)
        private val TEXT = Color.rgb(0xD8, 0xD8, 0xD8)
        private val MUTED = Color.rgb(0x80, 0x80, 0x80)
        private val OK = Color.rgb(0x7F, 0xD0, 0x8A)
        private val WARN = Color.rgb(0xD9, 0xB4, 0x66)
        private val ERR = Color.rgb(0xE0, 0x8A, 0x8A)

        /** Identyfikator sesji od komputera (tylko bezpieczne znaki - trafia do ścieżki). */
        fun sessionFrom(intent: Intent?): String {
            val raw = intent?.getStringExtra("session").orEmpty()
            val clean = raw.filter { it in 'a'..'z' || it in 'A'..'Z' || it in '0'..'9' || it == '-' || it == '_' }.take(64)
            return clean.ifEmpty { "manual-" + System.currentTimeMillis() }
        }
    }
}
