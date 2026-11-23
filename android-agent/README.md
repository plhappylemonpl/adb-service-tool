# Drapco Backup Agent (aplikacja pomocnicza)

Mała aplikacja Android (Kotlin, bez AndroidX) używana przez ADB Service Tool do
eksportu i przywracania **kontaktów (.vcf)** i **SMS (.xml, format SMS Backup & Restore)**.

## Kopia (`am start ... --es session <id>`)

- Eksport wyłącznie po kliknięciu „Wykonaj kopię” i zaakceptowaniu systemowych
  okien uprawnień `READ_CONTACTS` / `READ_SMS`.
- Pliki trafiają do `Download/Backup/<sesja>/` (Android 10+: MediaStore, bez
  uprawnień do pamięci; Android 5–9: `WRITE_EXTERNAL_STORAGE`).
  Na końcu `done.json` (lista plików + rozmiary) albo `error.json`.
- `CleanupReceiver` (chroniony uprawnieniem `DUMP` – tylko powłoka ADB) usuwa
  pliki sesji razem z wpisami MediaStore.
- Pakiet `com.drapco.agent`, minSdk 21, targetSdk 34.

## Przywracanie (`am start ... --es session <id> --es mode restore`)

- Komputer wgrywa `Download/Restore/<sesja>/contacts.vcf` i `sms.xml` i nadaje odczyt
  plików (Android 11+: `appops set com.drapco.agent MANAGE_EXTERNAL_STORAGE allow`,
  6–10: `pm grant ... READ_EXTERNAL_STORAGE`); bez tego agent sam prosi o dostęp.
- „Przywróć kontakty”: kopia pliku do `cacheDir`, `VcfProvider` (nieeksportowany,
  jednorazowe prawo odczytu URI) → `ACTION_VIEW` `text/x-vcard` (import aplikacji Kontakty).
  Brak takiej aplikacji → własny parser vCard 2.1/3.0 i import przez `WRITE_CONTACTS`.
- „Przywróć SMS”: prośba o rolę domyślnej aplikacji SMS (Android 10+ `RoleManager`,
  4.4–9 `Telephony.Sms.Intents.ACTION_CHANGE_DEFAULT`), wstawianie `<sms>` przez
  `ContentResolver.applyBatch` (address, body, date, date_sent, type, read, status…),
  duplikaty pomijane, MMS liczone i pomijane. Wymagane przez Androida komponenty
  domyślnej aplikacji SMS: `SmsDeliverReceiver` (zapisuje przychodzące SMS do skrzynki –
  nic nie ginie), `MmsDeliverReceiver`, `ComposeActivity`, `RespondService`.
- Potem komunikat i przycisk „Przywróć domyślną aplikację SMS”; „Gotowe” zapisuje
  `restore_done.json`. Komputer i tak sprawdza rolę SMS i przywraca poprzednią aplikację.
- Android 13+: przy instalacji ręcznej (plik z Pobranych) zmiana aplikacji SMS może być
  „ustawieniem z ograniczeniami” – Informacje o aplikacji → ⋮ → „Zezwalaj na ustawienia
  z ograniczeniami”.

## Języki

Teksty UI wyłącznie z zasobów: `res/values/strings.xml` (angielski – domyślny) +
`values-pl`, `-de`, `-es`, `-fr`, `-it`, `-pt`, `-nl`, `-cs`, `-ru`, `-uk`, `-tr`,
`-ar`, `-hi`, `-bn`, `-zh-rCN`, `-ja`, `-ko`, `-in` (indonezyjski), `-vi` (20 języków).
Źródło tłumaczeń: `tools/i18n/*.py`, generator: `python3 tools/gen_strings.py`
(escapowanie Androida, kontrola kompletu kluczy i placeholderów `%1$s`/`%1$d`).
W kodzie: `ctx.tr("klucz", args…)` (`Tr.kt`) – odwołanie po nazwie, bo kompilacja bez
Gradle kompiluje Kotlin przed zasobami (brak klasy `R`).

Gotowy plik: `../src-tauri/resources/agent/Agent.apk` (wbudowany też w binarkę
aplikacji – działa bez dodatkowych plików).

## Budowanie

Android Studio / Gradle: otwórz ten folder i `Build > Generate Signed APK`
(albo `gradle assembleRelease`), skopiuj
`app/build/outputs/apk/release/app-release.apk` do
`../src-tauri/resources/agent/Agent.apk`.

Bez Android Studio: `build-sdkless.sh` (kotlinc + R8 + apktool + uber-apk-signer –
zmienne środowiskowe opisane w skrypcie).

Wymóg Androida 11+ (targetSdk 30+): `resources.arsc` bez kompresji i wyrównany do 4 B,
inaczej `adb install` kończy się `Failure [-124]`. Skrypt to wymusza: `doNotCompress: arsc`
w apktool.yml, `tools/apkcheck.jar fix` (STORED + wyrównanie), `zipalign -p -f 4` z SDK
(jeśli jest), podpis, a na końcu `apkcheck.jar verify` – niezgodny APK nie wychodzi ze skryptu.
Ręczne sprawdzenie dowolnego APK: `java -cp tools/apkcheck.jar ApkCheck verify plik.apk`.

`agent.keystore` (hasło `android`, alias `agent`) to klucz wyłącznie dla tej
aplikacji pomocniczej – agent jest instalowany na czas kopii i od razu odinstalowywany.
