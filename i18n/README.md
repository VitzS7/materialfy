# Localisation

Materialfy's source language is **English**: every user-visible string is written in English in the code, and that
English text is also the lookup key. Other languages are dictionaries in this directory: Russian (`i18n/ru/*.json`,
three plural forms) and Portuguese, Brazil (`i18n/pt/*.json`, two plural forms). The language is chosen in
Settings → Appearance → Language (setting `language`: `"system"` by default, `"en"`, `"ru"` or `"pt"`) and switches
live, without a restart. `"system"` resolves to the OS UI language when we have a dictionary for it (the first of
`QLocale::system().uiLanguages()`), and to English otherwise; it is evaluated
at startup (`mf_i18n::parse_setting(..).resolve(..)`), so a change of the OS language applies on the next launch.

## Layout

```
i18n/
  ru/
    settings.json   # one file per area of the app; all files of a language are merged
    home.json
    ...
  pt/
    ...
```

Each file is a flat JSON object: `"English source string": "translation"`. Files are merged at build time by
`crates/mf-i18n/build.rs` and embedded into the binary. A key may appear in several files only with **the same
value** — a conflicting duplicate (also inside one file) fails the build with a message naming both files. Invalid
JSON and malformed plural entries fail the build too. `cargo test -p mf-i18n` runs the checks.

## Adding a string

QML (every file that imports `Md3`):

```qml
Label { text: I18n.t("Play") }
Label { text: I18n.t("{0} of {1}", done, total) }
Label { text: I18n.n(count, "{0} track", "{0} tracks") }
```

Rust (`mf-i18n` crate):

```rust
mf_i18n::t("Play");
mf_i18n::tf("{0} of {1}", &[&done, &total]);   // or mf_i18n::tr!("{0} of {1}", done, total)
mf_i18n::n(count, "{0} track", "{0} tracks");
```

Then add the translation to the matching `i18n/ru/<area>.json` and `i18n/pt/<area>.json`. A missing translation is
not an error: the English text is shown.

Bindings in QML re-evaluate when the language changes because every `I18n` function reads `Translations.language`.
Strings that Rust has already produced (service snapshots) keep their language until the service sends its next
update.

## Placeholders

Positional: `{0}`, `{1}`, … in both the key and the translation. The translation may reorder them:

```json
"{0} by {1}": "{1}: {0}"
```

Never build a sentence by concatenating translated fragments — translate the whole sentence with placeholders.

## Plurals

The key is the English singular and plural joined by `|`; the value has as many forms as the language needs —
Russian three *one|few|many*, Portuguese two *one|many*:

```json
"{0} track|{0} tracks": "{0} трек|{0} трека|{0} треков"
"{0} track|{0} tracks": "{0} faixa|{0} faixas"
```

`I18n.n(count, "{0} track", "{0} tracks")` picks the form (English and Portuguese: 1 → one, else many; Russian:
1, 21, 101 → one; 2–4, 22–24 → few; 0, 5–20, 11–14, 25… → many) and replaces `{0}` with the count.
`I18n.nCompact(...)` does the same with `{0}` = `I18n.compact(count)` ("1.2M listeners" / "1,2 млн слушателей" /
"1,2 mi ouvintes").

## Formatting helpers (QML)

Numbers, dates and sizes are formatted by `I18n` (locale data in `src/materialfy/qml/Md3/I18n.qml`, `_loc`), not by
the dictionary:

| Helper | English | Russian | Portuguese |
| --- | --- | --- | --- |
| `I18n.compact(1234567)` | 1.2M | 1,2 млн | 1,2 mi |
| `I18n.grouped(8765)` | 8,765 | 8 765 | 8.765 |
| `I18n.decimal(1.25, 1)` | 1.3 | 1,3 | 1,3 |
| `I18n.dayMonth(date)` | Oct 9 | 9 октября | 9 de out. |
| `I18n.date(date)` / `shortDate` | Oct 9, 2024 | 9 окт. 2024 | 9 de out. de 2024 |
| `I18n.monthYear(date)` | October 2024 | октябрь 2024 | outubro de 2024 |
| `I18n.relative(date)` | 3 days ago | 3 дня назад | 3 dias atrás |
| `I18n.duration(ms)` | 1 hr 5 min | 1 ч 5 мин | 1 h 5 min |
| `I18n.bytes(n)` | 1.5 GB | 1,5 ГБ | 1,5 GB |

`date` arguments may be an ISO string, a millisecond timestamp, a `Date`, or a 4-digit year.

## Qt's own strings

File dialogs, standard context menus and other Qt texts follow the language through a `QTranslator` for
`qtbase_<code>.qm` (`app_set_qt_language` in `crates/materialfy/cpp/mf_shell.cpp`). The release build deploys these
catalogs with `windeployqt --translations ru,pt`; English needs none.

## Adding a language

1. Create `i18n/<code>/` with JSON files of the same shape (plural values: the forms that language needs).
2. `crates/mf-i18n`: add a `Lang` variant and its code to `DICTIONARIES`, list it in the `LANGS` table of `build.rs`
   (code + number of plural forms), add its plural rule to `plural_index`, and handle it in `parse_lang`,
   `parse_setting` and `dictionary`.
3. `crates/materialfy/src/bridges/translations.rs`: expose its dictionary to QML as another `#[qproperty]`.
4. `src/materialfy/qml/Md3/I18n.qml`: map the code in `lang`, `_dictOf` and `_cur`, add a plural rule and a `_loc`
   block.
5. Settings page: add the option to the Language button group (labels are written in their own language).
6. Deploy Qt's catalogs: add the code to `--translations` in `build_windows.ps1`.
