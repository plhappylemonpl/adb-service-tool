#!/usr/bin/env python3
"""Generuje res/values*/strings.xml agenta z tłumaczeń (tools/i18n/*.py).

Użycie: python3 tools/gen_strings.py   (z katalogu android-agent)
Sprawdza: komplet kluczy w każdym języku i zgodność placeholderów (%1$s, %2$d) z EN.
"""
import pathlib, re, sys
from xml.sax.saxutils import escape

HERE = pathlib.Path(__file__).resolve().parent
RES = HERE.parent / "app" / "src" / "main" / "res"
PH = re.compile(r"%(\d+\$)?[sd]")


def load(name):
    ns = {}
    exec((HERE / "i18n" / name).read_text(encoding="utf-8"), ns)
    return ns


def android_escape(s):
    s = escape(s)  # & < >
    s = s.replace("\\", "\\\\").replace("'", "\\'").replace('"', '\\"').replace("\n", "\\n")
    if s[:1] in ("@", "?"):
        s = "\\" + s
    return s


FLEMISH = [
    (r"\bapparaten\b", "toestellen"), (r"\bApparaten\b", "Toestellen"),
    (r"apparaat", "toestel"), (r"Apparaat", "Toestel"),
    (r"\btelefoon\b", "gsm"), (r"sms'jes", "sms'en"), (r"Sms'jes", "Sms'en"),
]


def flemish(s):
    for a, b in FLEMISH:
        s = re.sub(a, b, s)
    return s


# historyczny kod indonezyjskiego (Android < 7 zgłasza "in", nowsze "id")
ALIASES = {"in": ["id"]}


def main():
    base = load("base.py")
    en, langs = base["EN"], {"pl": base["PL"], "de": base["DE"]}
    keys = list(en)
    for f in ("set_a.py", "set_b.py", "set_c.py", "set_d.py", "set_e.py"):
        for code, values in load(f)["L"].items():
            if len(values) != len(keys):
                sys.exit(f"{code}: {len(values)} tekstów, oczekiwano {len(keys)}")
            langs[code] = dict(zip(keys, values))
    # odmiany pochodne
    langs["nl-rBE"] = {k: flemish(v) for k, v in langs["nl"].items()}  # Vlaams: toestel, gsm
    errors = []
    for code, d in langs.items():
        if set(d) != set(keys):
            errors.append(f"{code}: klucze różnią się: {set(d) ^ set(keys)}")
            continue
        for k in keys:
            if sorted(PH.findall(d[k])) != sorted(PH.findall(en[k])) or d[k].count("%") != en[k].count("%"):
                errors.append(f"{code}/{k}: placeholdery nie zgadzają się z EN")
            if not d[k].strip():
                errors.append(f"{code}/{k}: pusty tekst")
    if errors:
        sys.exit("\n".join(errors))

    def write(folder, d, default):
        out = ['<?xml version="1.0" encoding="utf-8"?>',
               "<!-- Wygenerowane przez tools/gen_strings.py - nie edytuj ręcznie. -->",
               "<resources>"]
        if default:
            out.append('    <string name="app_name" translatable="false">Drapco Backup Agent</string>')
        for k in keys:

            out.append(f'    <string name="{k}">{android_escape(d[k])}</string>')
        out.append("</resources>\n")
        p = RES / folder / "strings.xml"
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text("\n".join(out), encoding="utf-8")

    write("values", en, True)
    for code, d in sorted(langs.items()):
        write(f"values-{code}", d, False)
        for alias in ALIASES.get(code, []):
            write(f"values-{alias}", d, False)
    print(f"OK: {len(langs) + 1} języków × {len(keys)} tekstów")


if __name__ == "__main__":
    main()
