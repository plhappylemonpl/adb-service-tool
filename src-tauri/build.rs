fn main() {
    // Agent.apk (aplikacja pomocnicza "Kontakty i SMS") jest wbudowywany w binarkę
    // przez include_bytes!. Gdy pliku brak (np. świeży klon bez zbudowanego agenta),
    // aplikacja nadal się kompiluje - funkcja kopii zgłosi wtedy czytelny błąd.
    let apk = std::path::Path::new("resources/agent/Agent.apk");
    println!("cargo:rerun-if-changed=resources/agent/Agent.apk");
    // katalog: wykrywa też dodanie/usunięcie pliku (mv zachowuje stare mtime)
    println!("cargo:rerun-if-changed=resources/agent");
    println!("cargo::rustc-check-cfg=cfg(agent_apk)");
    if apk.is_file() {
        println!("cargo:rustc-cfg=agent_apk");
    } else {
        println!("cargo:warning=Brak src-tauri/resources/agent/Agent.apk - kopia kontaktów/SMS będzie niedostępna (zbuduj: ./build.sh agent)");
        // bez pliku nie dołączamy go do pakietu (tauri_build sprawdza istnienie zasobów).
        // Przy `tauri build --config tauri.noagent.conf.json` CLI ustawia już TAURI_CONFIG.
        if std::env::var_os("TAURI_CONFIG").is_none() {
            const NO_AGENT: &str = r#"{"bundle":{"resources":[]}}"#;
            std::env::set_var("TAURI_CONFIG", NO_AGENT);
            println!("cargo:rustc-env=TAURI_CONFIG={NO_AGENT}");
        }
    }
    tauri_build::build()
}
