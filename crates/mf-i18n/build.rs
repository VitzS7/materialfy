// Embeds the dictionaries: reads ../../i18n/<lang>/*.json for every supported language, merges each set
// (duplicate keys must agree) and writes one JSON object per language to $OUT_DIR/<lang>.json for include_str!.
use std::path::{Path, PathBuf};

#[path = "src/merge.rs"]
mod merge;

/// Supported languages and how many translated forms a plural entry needs: Russian one|few|many, Portuguese one|many.
const LANGS: &[(&str, usize)] = &[("ru", 3), ("pt", 2)];

fn main() {
    for (lang, plural_forms) in LANGS {
        embed(lang, *plural_forms);
    }
}

fn embed(lang: &str, plural_forms: usize) {
    // MF_I18N_DIR overrides the dictionary root (isolated tests of this crate); language folders live under it
    println!("cargo::rerun-if-env-changed=MF_I18N_DIR");
    let root = match std::env::var_os("MF_I18N_DIR") {
        Some(d) => PathBuf::from(d),
        None => Path::new(env!("CARGO_MANIFEST_DIR")).join("../../i18n"),
    };
    let dir = root.join(lang);
    println!("cargo::rerun-if-changed={}", dir.display());
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect())
        .unwrap_or_default();
    paths.sort();
    let mut files = Vec::new();
    for p in &paths {
        println!("cargo::rerun-if-changed={}", p.display());
        let text = std::fs::read_to_string(p).unwrap_or_else(|e| panic!("i18n: cannot read {}: {e}", p.display()));
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        files.push((format!("i18n/{lang}/{name}"), text.trim_start_matches('\u{feff}').to_owned()));
    }
    let merged = match merge::merge(&files, plural_forms) {
        Ok(m) => m,
        Err(e) => panic!("\n\ni18n dictionary errors:\n{e}\n\n"),
    };
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join(format!("{lang}.json"));
    std::fs::write(out, serde_json::to_string(&merged).unwrap()).unwrap();
}
