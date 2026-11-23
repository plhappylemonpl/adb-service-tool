package com.drapco.agent

import android.content.Context

/**
 * Teksty z zasobów (res/values(-xx)/strings.xml) po nazwie.
 *
 * Kompilacja bez Gradle (build-sdkless.sh) kompiluje Kotlin PRZED zasobami
 * (apktool/aapt2), więc klasa R nie istnieje w czasie kompilacji - stąd
 * odwołanie przez nazwę. Brakujący klucz zwraca samą nazwę (widać w UI, nie crash).
 */
fun Context.tr(key: String, vararg args: Any): String {
    val id = resources.getIdentifier(key, "string", packageName)
    if (id == 0) return key
    return if (args.isEmpty()) getString(id) else getString(id, *args)
}
