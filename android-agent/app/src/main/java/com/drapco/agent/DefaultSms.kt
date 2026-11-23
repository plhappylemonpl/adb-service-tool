package com.drapco.agent

import android.app.role.RoleManager
import android.content.Context
import android.content.Intent
import android.os.Build
import android.provider.Settings
import android.provider.Telephony

/**
 * Domyślna aplikacja SMS - tylko ona może zapisywać do content://sms.
 *
 *  - Android 4.4-9: Telephony.Sms.Intents.ACTION_CHANGE_DEFAULT (systemowe okno "Zmienić aplikację SMS?"),
 *  - Android 10+  : RoleManager.ROLE_SMS (ACTION_CHANGE_DEFAULT jest tam ignorowane).
 *
 * Poprzednią aplikację zapamiętujemy, żeby po przywróceniu SMS-ów
 * pomóc użytkownikowi wrócić do niej (Android 10+ nie pozwala zrobić tego
 * programowo - otwieramy ustawienia aplikacji domyślnych). Komputer i tak
 * sprawdza to po zakończeniu i przywraca poprzednią aplikację przez ADB.
 */
object DefaultSms {
    private const val PREFS = "restore_state"
    private const val KEY_PREV = "previous_sms_app"

    fun current(ctx: Context): String? = try {
        Telephony.Sms.getDefaultSmsPackage(ctx)
    } catch (_: Throwable) {
        null
    }

    fun isDefault(ctx: Context): Boolean {
        if (Build.VERSION.SDK_INT >= 29) {
            val rm = ctx.getSystemService(RoleManager::class.java)
            if (rm != null) return rm.isRoleHeld(RoleManager.ROLE_SMS)
        }
        return current(ctx) == ctx.packageName
    }

    /** Zapamiętuje aktualną aplikację SMS (o ile to nie my). */
    fun rememberPrevious(ctx: Context) {
        val cur = current(ctx)
        if (!cur.isNullOrEmpty() && cur != ctx.packageName) {
            ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString(KEY_PREV, cur).commit()
        }
    }

    fun previous(ctx: Context): String? =
        ctx.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY_PREV, null)

    /** Okno systemowe "ustaw jako domyślną"; null = rola SMS niedostępna (np. tablet bez telefonii). */
    fun requestIntent(ctx: Context): Intent? {
        if (Build.VERSION.SDK_INT >= 29) {
            val rm = ctx.getSystemService(RoleManager::class.java) ?: return null
            if (!rm.isRoleAvailable(RoleManager.ROLE_SMS)) return null
            return rm.createRequestRoleIntent(RoleManager.ROLE_SMS)
        }
        return Intent(Telephony.Sms.Intents.ACTION_CHANGE_DEFAULT)
            .putExtra(Telephony.Sms.Intents.EXTRA_PACKAGE_NAME, ctx.packageName)
    }

    /** Powrót do poprzedniej aplikacji SMS. */
    fun restoreIntents(ctx: Context): List<Intent> {
        val list = mutableListOf<Intent>()
        val prev = previous(ctx)
        if (Build.VERSION.SDK_INT < 29 && !prev.isNullOrEmpty()) {
            list += Intent(Telephony.Sms.Intents.ACTION_CHANGE_DEFAULT)
                .putExtra(Telephony.Sms.Intents.EXTRA_PACKAGE_NAME, prev)
        }
        if (Build.VERSION.SDK_INT >= 24) list += Intent(Settings.ACTION_MANAGE_DEFAULT_APPS_SETTINGS)
        list += Intent(Settings.ACTION_WIRELESS_SETTINGS)
        list += Intent(Settings.ACTION_SETTINGS)
        return list
    }
}
