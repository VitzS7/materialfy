//! Каталоги приложения (organization "materialfy", application "Materialfy" — как у QCoreApplication в оболочке).
//!
//! | что                    | Linux                                   | Windows                                        | macOS                                                   |
//! |------------------------|-----------------------------------------|------------------------------------------------|---------------------------------------------------------|
//! | app_data_dir (AppData) | ~/.local/share/materialfy/Materialfy  | %APPDATA%\materialfy\Materialfy              | ~/Library/Application Support/materialfy/Materialfy   |
//! | cache_dir (Cache)      | ~/.cache/materialfy/Materialfy        | %LOCALAPPDATA%\materialfy\Materialfy\cache    | ~/Library/Caches/materialfy/Materialfy                |
//! | settings INI           | ~/.config/materialfy/Materialfy.conf  | (реестр HKCU\Software\materialfy\Materialfy) | ~/Library/Preferences/materialfy/Materialfy.conf      |
use std::path::{Path, PathBuf};

pub const ORGANIZATION: &str = "materialfy";
pub const APPLICATION: &str = "Materialfy";

fn home() -> PathBuf {
    ::dirs::home_dir().unwrap_or_else(std::env::temp_dir)
}

fn app_subdir(base: PathBuf) -> PathBuf {
    base.join(ORGANIZATION).join(APPLICATION)
}

/// Каталог данных приложения (session.json, history.json, state/). Не создаётся автоматически.
pub fn app_data_dir() -> PathBuf {
    // Linux: $XDG_DATA_HOME или ~/.local/share; Windows: Roaming; macOS: Application Support
    let base = ::dirs::data_dir().unwrap_or_else(|| home().join(".local").join("share"));
    app_subdir(base)
}

/// Корень кэша приложения.
pub fn cache_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        let base = ::dirs::data_local_dir().unwrap_or_else(|| home().join("AppData").join("Local"));
        app_subdir(base).join("cache")
    }
    #[cfg(not(target_os = "windows"))]
    {
        let base = ::dirs::cache_dir().unwrap_or_else(|| home().join(".cache"));
        app_subdir(base)
    }
}

/// Подкаталог кэша ("images", "covers", "lyrics"), создаётся при первом вызове.
pub fn cache_subdir(sub: &str) -> PathBuf {
    let dir = cache_dir().join(sub);
    if let Err(err) = std::fs::create_dir_all(&dir) {
        tracing::warn!("could not create {}: {err}", dir.display());
    }
    dir
}

/// Файл сессии плеера: $MATERIALFY_SESSION_FILE или app_data_dir()/session.json.
pub fn session_file() -> PathBuf {
    match std::env::var_os("MATERIALFY_SESSION_FILE") {
        Some(path) if !path.is_empty() => PathBuf::from(path),
        _ => app_data_dir().join("session.json"),
    }
}

// --- Spotify ---

/// Служебное состояние (маркеры отзыва и профиль хранилища секретов sf-auth): app_data_dir()/state.
pub fn state_dir() -> PathBuf {
    app_data_dir().join("state")
}

/// Локальная история прослушиваний (sf-services HistoryService): app_data_dir()/history.json.
pub fn history_file() -> PathBuf {
    app_data_dir().join("history.json")
}

/// Файл громкости librespot (Cache volume): app_data_dir()/volume.
pub fn volume_dir() -> PathBuf {
    app_data_dir().join("volume")
}

/// Аудиокэш librespot: cache_dir()/audio (лимит — настройка AudioCacheMb).
pub fn audio_cache_dir() -> PathBuf {
    cache_dir().join("audio")
}

/// Кэш «Любимых треков» аккаунта: cache_dir()/liked/<account>.json.
pub fn liked_cache_file(account_id: &str) -> PathBuf {
    cache_subdir("liked").join(format!("{}.json", safe_name(account_id)))
}

/// Кэш полок главной аккаунта: cache_dir()/home/<account>.json.
pub fn home_cache_file(account_id: &str) -> PathBuf {
    cache_subdir("home").join(format!("{}.json", safe_name(account_id)))
}

/// Кэш строк плейлистов аккаунта: cache_dir()/playlists/<account>/.
pub fn playlist_cache_dir(account_id: &str) -> PathBuf {
    cache_subdir("playlists").join(safe_name(account_id))
}

/// Id аккаунта как имя файла: base62 и обычные имена — как есть, остальное (слеши, «..», пусто) — hex.
fn safe_name(id: &str) -> String {
    let plain = !id.is_empty()
        && !id.starts_with('.')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if plain {
        id.to_owned()
    } else if id.is_empty() {
        "_".to_owned()
    } else {
        id.bytes().map(|b| format!("{b:02x}")).collect()
    }
}

/// Занято на диске, байт (Settings → «Хранилище»).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheUsage {
    pub audio: u64,
    /// обложки covers + картинки QML images (и прочее в кэше)
    pub covers: u64,
    /// плейлисты + «Любимые треки»
    pub playlists: u64,
    pub lyrics: u64,
}

impl CacheUsage {
    pub fn total(&self) -> u64 {
        self.audio + self.covers + self.playlists + self.lyrics
    }
}

/// Посчитать размеры каталогов кэша (блокирующе — звать из spawn_blocking).
pub fn cache_usage() -> CacheUsage {
    cache_usage_in(&cache_dir())
}

/// Очистить кэш (аудио, обложки, картинки, плейлисты, тексты). Вход, state/ и настройки лежат вне cache_dir()
/// и не трогаются. Каталоги остаются (их пути держат сервисы) — удаляется содержимое. Занятые файлы
/// (аудиокэш играющего трека на Windows) пропускаются; ошибка — первая встреченная.
pub fn clear_cache() -> std::io::Result<()> {
    clear_cache_in(&cache_dir())
}

fn cache_usage_in(root: &Path) -> CacheUsage {
    let mut usage = CacheUsage::default();
    let Ok(entries) = std::fs::read_dir(root) else { return usage };
    for entry in entries.flatten() {
        let size = tree_size(&entry.path());
        match entry.file_name().to_str().unwrap_or("") {
            "audio" => usage.audio += size,
            "playlists" | "liked" => usage.playlists += size,
            "lyrics" => usage.lyrics += size,
            _ => usage.covers += size,
        }
    }
    usage
}

/// Размер файла или дерева; ссылки не разворачиваются.
fn tree_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries.flatten().map(|entry| tree_size(&entry.path())).sum()
}

fn clear_cache_in(root: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    let mut first_error = None;
    for entry in entries.flatten() {
        let path = entry.path();
        let result = if is_real_dir(&path) { clear_dir(&path) } else { std::fs::remove_file(&path) };
        if let Err(err) = result {
            tracing::warn!("cache {}: {err}", path.display());
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Удалить содержимое каталога, оставив сам каталог.
fn clear_dir(dir: &Path) -> std::io::Result<()> {
    let mut first_error = None;
    for entry in std::fs::read_dir(dir)?.flatten() {
        let path = entry.path();
        let result = if is_real_dir(&path) { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
        if let Err(err) = result {
            first_error.get_or_insert(err);
        }
    }
    first_error.map_or(Ok(()), Err)
}

fn is_real_dir(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Файл настроек INI. `config_override` — каталог из --config-dir: тогда <dir>/materialfy/Materialfy.conf на любой ОС
/// (так QSettings::setPath раскладывает INI). Без него: Linux/macOS — путь из таблицы; на Windows INI не используется.
pub fn settings_ini_file(config_override: Option<&Path>) -> PathBuf {
    let base = match config_override {
        Some(dir) => dir.to_path_buf(),
        None => default_config_base(),
    };
    base.join(ORGANIZATION).join(format!("{APPLICATION}.conf"))
}

fn default_config_base() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home().join("Library").join("Preferences")
    }
    #[cfg(not(target_os = "macos"))]
    {
        // Linux: $XDG_CONFIG_HOME или ~/.config; на Windows путь только для полноты (там реестр)
        ::dirs::config_dir().unwrap_or_else(|| home().join(".config"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        assert!(app_data_dir().ends_with(Path::new("materialfy").join("Materialfy")));
        let ini = settings_ini_file(Some(Path::new("cfg")));
        assert_eq!(ini, Path::new("cfg").join("materialfy").join("Materialfy.conf"));
        #[cfg(target_os = "windows")]
        assert!(cache_dir().ends_with(Path::new("materialfy").join("Materialfy").join("cache")));
    }

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("mf-platform-cache-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    fn put(path: &Path, bytes: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    #[test]
    fn usage_and_clear() {
        let root = temp_root("usage");
        assert_eq!(cache_usage_in(&root), CacheUsage::default());
        assert!(clear_cache_in(&root).is_ok());

        put(&root.join("audio").join("ab").join("file"), 1000);
        put(&root.join("audio").join("cd"), 24);
        put(&root.join("covers").join("x.jpg"), 300);
        put(&root.join("images").join("y.jpg"), 200);
        put(&root.join("playlists").join("user").join("p.json"), 50);
        put(&root.join("liked").join("user.json"), 7);
        put(&root.join("lyrics").join("l.json"), 11);
        put(&root.join("stray.tmp"), 3);
        let usage = cache_usage_in(&root);
        assert_eq!(usage, CacheUsage { audio: 1024, covers: 503, playlists: 57, lyrics: 11 });
        assert_eq!(usage.total(), 1595);

        clear_cache_in(&root).unwrap();
        assert_eq!(cache_usage_in(&root).total(), 0);
        // каталоги на месте, содержимого нет
        for sub in ["audio", "covers", "images", "playlists", "liked", "lyrics"] {
            let dir = root.join(sub);
            assert!(dir.is_dir(), "{sub}");
            assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0, "{sub}");
        }
        assert!(!root.join("stray.tmp").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cache_is_apart_from_credentials() {
        // clear_cache чистит только cache_dir(): данные входа, state/ и настройки лежат вне его
        let cache = cache_dir();
        assert!(!app_data_dir().starts_with(&cache));
        assert!(!state_dir().starts_with(&cache));
        assert!(!volume_dir().starts_with(&cache));
        assert!(!history_file().starts_with(&cache));
        assert!(!settings_ini_file(None).starts_with(&cache));
        assert!(audio_cache_dir().starts_with(&cache));
    }

    #[test]
    fn account_names_are_safe() {
        assert_eq!(safe_name("31abcXYZ"), "31abcXYZ");
        assert_eq!(safe_name("user.name_1-2"), "user.name_1-2");
        assert_eq!(safe_name("../x"), "2e2e2f78");
        assert_eq!(safe_name("a/b"), "612f62");
        assert_eq!(safe_name(""), "_");
    }
}
