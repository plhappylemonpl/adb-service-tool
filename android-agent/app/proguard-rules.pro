# Komponenty z manifestu (R8 poza Gradle nie czyta manifestu - trzymamy je jawnie).
# Ogólna reguła: KAŻDY komponent Androida z pakietu (nowe aktywności/odbiorniki nie mogą zniknąć -
# v1.2.0: brak reguły dla SmartEiActivity = ClassNotFoundException przy starcie). build-sdkless.sh
# dodatkowo sprawdza po R8, czy każda klasa z manifestu jest w classes.dex.
-keep public class com.drapco.agent.** extends android.app.Activity { *; }
-keep public class com.drapco.agent.** extends android.content.BroadcastReceiver { *; }
-keep public class com.drapco.agent.** extends android.app.Service { *; }
-keep public class com.drapco.agent.** extends android.content.ContentProvider { *; }
-keep class com.drapco.agent.MainActivity { *; }
-keep class com.drapco.agent.SmartEiActivity { *; }
-keep class com.drapco.agent.CleanupReceiver { *; }
-keep class com.drapco.agent.VcfProvider { *; }
-keep class com.drapco.agent.SmsDeliverReceiver { *; }
-keep class com.drapco.agent.MmsDeliverReceiver { *; }
-keep class com.drapco.agent.ComposeActivity { *; }
-keep class com.drapco.agent.RespondService { *; }
# Czytelne stosy błędów (APK i tak ma kilkadziesiąt KB).
-dontobfuscate
-dontwarn org.jetbrains.annotations.**
