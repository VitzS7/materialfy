// Translations: language of the UI for QML (Md3/I18n.qml). Source strings are English; `ru` is the embedded Russian
// dictionary from mf-i18n (i18n/ru/*.json). The SettingKey::Language setting is the source of truth: a change from any
// place (Settings.language in QML) arrives through the store subscription, switches mf_i18n, Qt's own translator
// (file dialogs, context menus), then `language` here — QML bindings over I18n re-evaluate on languageChanged.
// The setting "system" (default) follows the OS UI language, read once per run (no live tracking of OS changes).
use std::pin::Pin;
use std::sync::OnceLock;

use cxx_qt::{CxxQtType, Threading};
use cxx_qt_lib::{QString, QVariant};

use crate::qt::QVariantMap;
use mf_i18n::Lang;
use mf_platform::SettingKey;

use super::{Delivery, forward_events};
use crate::context::ctx;

#[cxx_qt::bridge]
pub mod qobject {
    unsafe extern "C++" {
        include!("cxx-qt-lib/qstring.h");
        type QString = cxx_qt_lib::QString;
        include!("cxx-qt-lib/qmap.h");
        type QMap_QString_QVariant = cxx_qt_lib::QMap<cxx_qt_lib::QMapPair_QString_QVariant>;
    }

    unsafe extern "RustQt" {
        #[qobject]
        /// "en" | "ru" | "pt" — the language applied right now (follows Settings.language; "system" resolved)
        #[qproperty(QString, language, READ, NOTIFY = language_changed)]
        /// English source → Russian; plural keys "one|many" → "one|few|many"
        #[qproperty(QMap_QString_QVariant, ru, READ, CONSTANT)]
        /// English source → Portuguese; plural keys "one|many" → "one|many"
        #[qproperty(QMap_QString_QVariant, pt, READ, CONSTANT)]
        type Translations = super::TranslationsRust;

        #[qsignal]
        #[cxx_name = "languageChanged"]
        fn language_changed(self: Pin<&mut Translations>);

        /// Translation of one string in the current language (for code that is not a binding).
        #[qinvokable]
        fn tr(self: &Translations, en: &QString) -> QString;
    }

    impl cxx_qt::Threading for Translations {}
    impl cxx_qt::Initialize for Translations {}
}

pub struct TranslationsRust {
    language: QString,
    ru: QVariantMap,
    pt: QVariantMap,
}

/// The embedded dictionary of `lang` as a QML map (empty for English).
fn dictionary_map(lang: Lang) -> QVariantMap {
    let mut map = QVariantMap::default();
    for (en, value) in mf_i18n::dictionary(lang).into_iter().flatten() {
        map.insert(QString::from(en.as_str()), QVariant::from(&QString::from(value.as_str())));
    }
    map
}

impl Default for TranslationsRust {
    fn default() -> Self {
        Self {
            language: QString::from(mf_i18n::lang().code()),
            ru: dictionary_map(Lang::Ru),
            pt: dictionary_map(Lang::Pt),
        }
    }
}

/// Switches the process language: mf_i18n (Rust strings) and Qt's translator. GUI thread only.
pub fn apply_language(lang: Lang) {
    mf_i18n::set_lang(lang);
    crate::qt::ffi::ffi::app_set_qt_language(&QString::from(lang.code()));
}

/// The OS UI languages (most preferred first), read from Qt once per run. GUI thread (after app_create).
fn os_languages() -> &'static [String] {
    static LIST: OnceLock<Vec<String>> = OnceLock::new();
    LIST.get_or_init(|| {
        let list = crate::qt::ffi::ffi::system_ui_languages();
        let list: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        tracing::debug!("OS UI languages: {list:?}");
        list
    })
}

/// The language to show for the stored setting: "en"/"ru"/"pt" as is, "system" (default) from the OS UI language.
pub fn effective_language(store: &mf_platform::SettingsStore) -> Lang {
    mf_i18n::parse_setting(&store.get_str(SettingKey::Language)).resolve(os_languages())
}

impl qobject::Translations {
    fn tr(&self, en: &QString) -> QString {
        QString::from(mf_i18n::t(&en.to_string()).as_str())
    }

    fn on_store(mut self: Pin<&mut Self>, delivery: Delivery<SettingKey>) {
        if matches!(delivery, Delivery::Event(key) if key != SettingKey::Language) {
            return;
        }
        let lang = effective_language(&ctx().services.settings);
        let code = QString::from(lang.code());
        if self.language == code {
            return;
        }
        apply_language(lang);
        self.as_mut().rust_mut().language = code;
        self.as_mut().language_changed();
        // Rust-made texts (snapshots of services) refresh with their next update.
    }
}

impl cxx_qt::Initialize for qobject::Translations {
    fn initialize(self: Pin<&mut Self>) {
        forward_events(self.qt_thread(), ctx().services.settings.subscribe(), Self::on_store);
    }
}
