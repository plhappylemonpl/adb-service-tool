package com.drapco.agent

import android.app.Activity
import android.app.Service
import android.content.BroadcastReceiver
import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.graphics.Color
import android.os.Build
import android.os.Bundle
import android.os.IBinder
import android.provider.Telephony
import android.view.Gravity
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView

/*
 * Komponenty WYMAGANE przez Androida, żeby aplikacja mogła zostać domyślną
 * aplikacją SMS (bez nich system nie pokaże agenta na liście).
 * Agent jest domyślny tylko na czas przywracania - ale w tym czasie nie może
 * zgubić żadnej przychodzącej wiadomości, więc odbiornik SMS zapisuje ją
 * do skrzynki odbiorczej (zwykle robi to domyślna aplikacja).
 */

/** Liczniki wiadomości odebranych w trakcie przywracania (trafiają do raportu). */
object IncomingStats {
    private const val PREFS = "restore_state"
    fun bump(ctx: Context, key: String) {
        val p = ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        p.edit().putInt(key, p.getInt(key, 0) + 1).commit()
    }

    fun get(ctx: Context, key: String): Int =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getInt(key, 0)
}

/** SMS_DELIVER - przychodzący SMS zapisywany do skrzynki odbiorczej. */
class SmsDeliverReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Telephony.Sms.Intents.SMS_DELIVER_ACTION) return
        val parts = try {
            Telephony.Sms.Intents.getMessagesFromIntent(intent)
        } catch (_: Throwable) {
            null
        }
        if (parts.isNullOrEmpty()) return
        val first = parts[0] ?: return
        val body = parts.joinToString("") { it?.displayMessageBody.orEmpty() }
        val v = ContentValues().apply {
            put(Telephony.Sms.ADDRESS, first.displayOriginatingAddress)
            put(Telephony.Sms.BODY, body)
            put(Telephony.Sms.DATE, System.currentTimeMillis())
            put(Telephony.Sms.DATE_SENT, first.timestampMillis)
            put(Telephony.Sms.READ, 0)
            put(Telephony.Sms.SEEN, 0)
            put(Telephony.Sms.TYPE, Telephony.Sms.MESSAGE_TYPE_INBOX)
            put(Telephony.Sms.PROTOCOL, first.protocolIdentifier)
            first.serviceCenterAddress?.let { put(Telephony.Sms.SERVICE_CENTER, it) }
            if (Build.VERSION.SDK_INT >= 22) {
                val sub = intent.getIntExtra("subscription", -1)
                if (sub >= 0) put(Telephony.Sms.SUBSCRIPTION_ID, sub)
            }
        }
        try {
            context.contentResolver.insert(Telephony.Sms.Inbox.CONTENT_URI, v)
            IncomingStats.bump(context, "incoming_sms_saved")
        } catch (_: Throwable) {
            IncomingStats.bump(context, "incoming_sms_lost")
        }
    }
}

/** WAP_PUSH_DELIVER (MMS) - agent nie pobiera MMS; liczymy je, żeby raport o tym powiedział. */
class MmsDeliverReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        IncomingStats.bump(context, "incoming_mms")
    }
}

/** "Odpowiedz wiadomością" na połączenie przychodzące - agent tego nie obsługuje. */
class RespondService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        stopSelf(startId)
        return START_NOT_STICKY
    }
}

/** Okno "napisz SMS" - informacja, że to tymczasowe narzędzie. */
class ComposeActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val d = resources.displayMetrics.density
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER
            setPadding((28 * d).toInt(), (48 * d).toInt(), (28 * d).toInt(), (32 * d).toInt())
            setBackgroundColor(Color.rgb(10, 10, 10))
        }
        root.addView(TextView(this).apply {
            text = tr("compose_info")
            textSize = 16f
            gravity = Gravity.CENTER
            setTextColor(Color.rgb(0xD9, 0xB4, 0x66))
        })
        root.addView(Button(this).apply {
            text = tr("btn_default_sms_back")
            isAllCaps = false
            setOnClickListener {
                for (i in DefaultSms.restoreIntents(this@ComposeActivity)) {
                    try {
                        startActivity(i)
                        break
                    } catch (_: Throwable) {
                    }
                }
                finish()
            }
        }, LinearLayout.LayoutParams(-1, -2).apply { topMargin = (24 * d).toInt() })
        setContentView(root)
    }
}
