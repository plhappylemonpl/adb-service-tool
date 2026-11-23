# Komponenty z manifestu (R8 poza Gradle nie czyta manifestu - trzymamy je jawnie).
-keep class com.drapco.agent.MainActivity { *; }
-keep class com.drapco.agent.CleanupReceiver { *; }
-keep class com.drapco.agent.VcfProvider { *; }
-keep class com.drapco.agent.SmsDeliverReceiver { *; }
-keep class com.drapco.agent.MmsDeliverReceiver { *; }
-keep class com.drapco.agent.ComposeActivity { *; }
-keep class com.drapco.agent.RespondService { *; }
# Czytelne stosy błędów (APK i tak ma kilkadziesiąt KB).
-dontobfuscate
-dontwarn org.jetbrains.annotations.**
