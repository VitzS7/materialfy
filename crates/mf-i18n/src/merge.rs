// Shared by build.rs (via #[path]) and the library tests: parses dictionary files and merges them,
// rejecting duplicate keys whose values differ (inside one file or across files).
use std::collections::BTreeMap;
use std::fmt;

use serde::de::{Deserializer, MapAccess, Visitor};

/// Ordered key/value pairs of one JSON object, duplicates preserved (serde_json's Map would silently keep the last).
struct Pairs(Vec<(String, String)>);

impl<'de> serde::Deserialize<'de> for Pairs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Pairs;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a flat JSON object of \"English source\": \"translation\" strings")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Pairs, A::Error> {
                let mut out = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, String>()? {
                    out.push((k, v));
                }
                Ok(Pairs(out))
            }
        }
        d.deserialize_map(V)
    }
}

/// Merges `(file name, file contents)` pairs into one dictionary. `plural_forms` is how many translated forms a
/// plural entry needs in this language: three for Russian (one|few|many), two for Portuguese (one|many).
pub fn merge(files: &[(String, String)], plural_forms: usize) -> Result<BTreeMap<String, String>, String> {
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut origin: BTreeMap<String, String> = BTreeMap::new();
    let mut errors = Vec::new();
    for (name, text) in files {
        let pairs: Pairs = match serde_json::from_str(text) {
            Ok(p) => p,
            Err(e) => {
                errors.push(format!("{name}: invalid JSON: {e}"));
                continue;
            }
        };
        for (k, v) in pairs.0 {
            if k.is_empty() {
                errors.push(format!("{name}: empty key"));
                continue;
            }
            if k.contains('|') {
                let forms = v.split('|').count();
                if k.split('|').count() != 2 || forms != plural_forms {
                    errors.push(format!(
                        "{name}: plural entry {k:?} must have 2 English forms (one|many) and {plural_forms} translated forms, got {forms}"
                    ));
                }
            }
            match out.get(&k) {
                Some(prev) if *prev != v => errors.push(format!(
                    "duplicate key {k:?} with different translations: {:?} in {} vs {v:?} in {name}",
                    prev, origin[&k]
                )),
                Some(_) => {}
                None => {
                    origin.insert(k.clone(), name.clone());
                    out.insert(k, v);
                }
            }
        }
    }
    if errors.is_empty() { Ok(out) } else { Err(errors.join("\n")) }
}
