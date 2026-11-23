package com.drapco.agent

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Sprzątanie po pobraniu kopii - wywoływane przez komputer:
 *   adb shell am broadcast -n com.drapco.agent/.CleanupReceiver --es session <id>
 *
 * Usuwa pliki sesji razem z wpisami MediaStore (aplikacja może kasować
 * wyłącznie pliki, które sama utworzyła). Wynik w danych broadcastu:
 * "deleted=N" (widoczne w wyjściu `am broadcast`).
 * Odbiornik chroniony uprawnieniem DUMP - wywoła go tylko powłoka ADB / system.
 */
class CleanupReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val session = MainActivity.sessionFrom(intent)
        val n = Storage.deleteSession(context, session)
        resultCode = 0
        resultData = "deleted=$n"
    }
}
