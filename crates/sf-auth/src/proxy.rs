//! Прокси (перенос ProxyMode/ProxyConfig/ManualProxy из spotifast/src/settings.rs).
//! Настройки хранит mf-platform (ProxyMode/ProxyHost/ProxyPort/ProxyUsername), пароль — слот Proxy в credentials.
use serde::{Deserialize, Serialize};

use crate::credentials::ProxyPassword;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// без прокси, игнорируя переменные окружения и настройки ОС
    Off,
    /// переменные окружения и (Windows/macOS) системный прокси
    #[default]
    System,
    Http,
    Socks,
}

impl ProxyMode {
    pub const ALL: [ProxyMode; 4] = [Self::Off, Self::System, Self::Http, Self::Socks];

    /// Значение настройки: "off" | "system" | "http" | "socks".
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::System => "system",
            Self::Http => "http",
            Self::Socks => "socks",
        }
    }

    pub fn from_setting(value: &str) -> Self {
        match value {
            "off" => Self::Off,
            "http" => Self::Http,
            "socks" => Self::Socks,
            _ => Self::System,
        }
    }

    pub fn is_manual(self) -> bool {
        matches!(self, Self::Http | Self::Socks)
    }

    /// Подпись для интерфейса.
    pub fn label(self) -> String {
        match self {
            Self::Off => mf_i18n::t("No proxy"),
            Self::System => mf_i18n::t("System proxy"),
            Self::Http => "HTTP".to_owned(),
            Self::Socks => "SOCKS5".to_owned(),
        }
    }
}

/// Итоговая политика прокси для HTTP-клиента и librespot.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ProxyConfig {
    /// неверные сохранённые настройки блокируют запросы до исправления
    Invalid(String),
    Off,
    #[default]
    System,
    Http(ManualProxy),
    Socks(ManualProxy),
}

#[derive(Clone, Copy)]
enum ManualKind {
    Http,
    Socks,
}

/// HTTP или SOCKS5 адрес с логином. Debug без логина и пароля.
#[derive(Clone, PartialEq, Eq)]
pub struct ManualProxy {
    pub(crate) endpoint: reqwest::Url,
    pub(crate) username: String,
    pub(crate) password: String,
}

impl std::fmt::Debug for ManualProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ManualProxy").field("url", &self.redacted()).finish()
    }
}

impl ProxyConfig {
    /// Из полей настроек; ошибка — текст по-русски («укажите хост», «порт — число от 1 до 65535»).
    pub fn from_parts(mode: ProxyMode, host: &str, port: &str, username: &str, password: &str) -> Result<Self, String> {
        match mode {
            ProxyMode::Off => Ok(Self::Off),
            ProxyMode::System => Ok(Self::System),
            ProxyMode::Http => ManualProxy::parse(ManualKind::Http, host, port, username, password).map(Self::Http),
            ProxyMode::Socks => ManualProxy::parse(ManualKind::Socks, host, port, username, password).map(Self::Socks),
        }
    }

    /// Как `from_parts`, но ошибка становится `Invalid` (запросы блокируются до исправления).
    pub fn resolve(mode: ProxyMode, host: &str, port: &str, username: &str, password: &str) -> Self {
        Self::from_parts(mode, host, port, username, password).unwrap_or_else(Self::Invalid)
    }

    pub fn mode(&self) -> Option<ProxyMode> {
        match self {
            Self::Invalid(_) => None,
            Self::Off => Some(ProxyMode::Off),
            Self::System => Some(ProxyMode::System),
            Self::Http(_) => Some(ProxyMode::Http),
            Self::Socks(_) => Some(ProxyMode::Socks),
        }
    }

    pub fn password_record(&self) -> Option<ProxyPassword> {
        match self {
            Self::Http(manual) | Self::Socks(manual) => manual.password_record(),
            _ => None,
        }
    }

    pub fn restore_password(&mut self, saved: &ProxyPassword) -> bool {
        match self {
            Self::Http(manual) | Self::Socks(manual) => manual.restore_password(saved),
            _ => false,
        }
    }

    /// librespot понимает только HTTP CONNECT без логина: URL с учётными данными ему не отдаём
    /// (он пишет его в лог и всё равно не отправляет авторизацию прокси).
    pub fn librespot_url(&self) -> Option<reqwest::Url> {
        match self {
            Self::Http(manual) => manual.librespot_url(),
            Self::System => system_http_proxy(),
            Self::Invalid(_) | Self::Off | Self::Socks(_) => None,
        }
    }

    /// Выбранный вручную прокси, через который librespot пойти не может (SOCKS5): движок тогда не запускают,
    /// чтобы он не ушёл к Spotify напрямую. Текст — для интерфейса.
    pub fn engine_unsupported(&self) -> Option<String> {
        match self {
            Self::Socks(_) => Some(mf_i18n::t(ENGINE_NEEDS_HTTP)),
            _ => None,
        }
    }
}

const ENGINE_NEEDS_HTTP: &str =
    "Playback on this computer only works through an HTTP proxy — over SOCKS5 audio would bypass the proxy";

/// HTTP-прокси, который режим System отдал бы движку. Системный SOCKS движок не умеет.
fn system_http_proxy() -> Option<reqwest::Url> {
    let matcher = hyper_util::client::proxy::matcher::Matcher::from_system();
    let dest = http::Uri::from_static("https://apresolve.spotify.com");
    let intercept = matcher.intercept(&dest)?;
    librespot_system_proxy(&intercept)
}

fn librespot_system_proxy(intercept: &hyper_util::client::proxy::matcher::Intercept) -> Option<reqwest::Url> {
    if intercept.basic_auth().is_some() || intercept.raw_auth().is_some() {
        return None;
    }
    http_proxy_from_uri(intercept.uri())
}

fn http_proxy_from_uri(uri: &http::Uri) -> Option<reqwest::Url> {
    match uri.scheme_str() {
        Some("http") => reqwest::Url::parse(&uri.to_string()).ok(),
        _ => None,
    }
}

fn validate_host(host: &str) -> Result<String, String> {
    let host = host.trim();
    if host.is_empty() {
        return Err(mf_i18n::t("Enter the proxy host"));
    }
    let host = host
        .split_once("://")
        .map_or(host, |(_, rest)| rest)
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.is_empty() {
        return Err(mf_i18n::t("Enter the proxy host"));
    }
    if host.parse::<std::net::IpAddr>().is_ok() || is_hostname(host) {
        return Ok(host.to_string());
    }
    Err(mf_i18n::t("The proxy host must be a server name or IP address"))
}

fn is_hostname(host: &str) -> bool {
    if host.len() > 253 || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    host.split('.').all(|label| {
        (1..=63).contains(&label.len())
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

fn validate_port(port: &str) -> Result<u16, String> {
    let port = port.trim();
    if port.is_empty() {
        return Err(mf_i18n::t("Enter the proxy port"));
    }
    if !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(mf_i18n::t("The proxy port must be a number from 1 to 65535"));
    }
    match port.parse::<u16>() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(mf_i18n::t("The proxy port must be a number from 1 to 65535")),
    }
}

impl ManualProxy {
    fn parse(kind: ManualKind, host: &str, port: &str, username: &str, password: &str) -> Result<Self, String> {
        let host = validate_host(host)?;
        let port = validate_port(port)?;
        let host = if host.contains(':') { format!("[{host}]") } else { host };
        let scheme = match kind {
            ManualKind::Http => "http",
            // имена Spotify разрешает прокси: SOCKS не выдаёт DNS-запросы мимо него
            ManualKind::Socks => "socks5h",
        };
        let endpoint = reqwest::Url::parse(&format!("{scheme}://{host}:{port}"))
            .map_err(|_| mf_i18n::t("Invalid proxy address"))?;
        Ok(Self { endpoint, username: username.to_string(), password: password.to_string() })
    }

    /// Запись пароля для хранилища — привязана к хосту, порту и логину.
    pub(crate) fn password_record(&self) -> Option<ProxyPassword> {
        if self.password.is_empty() {
            return None;
        }
        Some(ProxyPassword {
            host: self.endpoint.host_str()?.to_string(),
            port: self.port(),
            username: self.username.clone(),
            password: self.password.clone(),
        })
    }

    fn restore_password(&mut self, saved: &ProxyPassword) -> bool {
        if self.endpoint.host_str() != Some(saved.host.as_str())
            || self.port() != saved.port
            || self.username != saved.username
        {
            return false;
        }
        self.password.clone_from(&saved.password);
        true
    }

    /// Учётные данные ставятся отдельно, чтобы ошибка сборки не могла показать пароль.
    pub fn reqwest_proxy(&self) -> Result<reqwest::Proxy, reqwest::Error> {
        let mut proxy = reqwest::Proxy::all(self.endpoint.clone())?;
        if !self.username.is_empty() || !self.password.is_empty() {
            proxy = proxy.basic_auth(&self.username, &self.password);
        }
        Ok(proxy)
    }

    /// Адрес для librespot, с логином: он уходит прокси в Proxy-Authorization (форк librespot). Не для логов.
    fn librespot_url(&self) -> Option<reqwest::Url> {
        let mut url = self.endpoint.clone();
        if !self.username.is_empty() {
            url.set_username(&self.username).ok()?;
            url.set_password((!self.password.is_empty()).then_some(self.password.as_str())).ok()?;
        }
        Some(url)
    }

    /// Адрес без учётных данных — для логов.
    pub fn redacted(&self) -> reqwest::Url {
        self.endpoint.clone()
    }

    pub fn has_password(&self) -> bool {
        !self.password.is_empty()
    }

    /// Хост без скобок IPv6 (для QNetworkProxy).
    pub fn host(&self) -> &str {
        self.endpoint.host_str().unwrap_or_default().trim_start_matches('[').trim_end_matches(']')
    }

    /// Порт; Url прячет стандартный (80 у http), поэтому с умолчанием схемы.
    pub fn port(&self) -> u16 {
        self.endpoint.port_or_known_default().unwrap_or_default()
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub fn password(&self) -> &str {
        &self.password
    }
}

/// Проверка сохранённого пароля: запись должна получаться из своих же полей.
pub(crate) fn password_is_valid(saved: &ProxyPassword) -> bool {
    ManualProxy::parse(ManualKind::Http, &saved.host, &saved.port.to_string(), &saved.username, &saved.password)
        .ok()
        .and_then(|manual| manual.password_record())
        .as_ref()
        == Some(saved)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual(kind: ManualKind, host: &str, port: &str, user: &str, pass: &str) -> Result<ManualProxy, String> {
        ManualProxy::parse(kind, host, port, user, pass)
    }

    #[test]
    fn off_and_system_always_succeed() {
        assert_eq!(ProxyConfig::from_parts(ProxyMode::Off, "", "", "", "").unwrap(), ProxyConfig::Off);
        assert_eq!(ProxyConfig::from_parts(ProxyMode::System, "x y", "abc", "", "").unwrap(), ProxyConfig::System);
        assert_eq!(ProxyConfig::Off.librespot_url(), None);
        assert!(matches!(ProxyConfig::resolve(ProxyMode::Http, "", "1", "", ""), ProxyConfig::Invalid(_)));
    }

    #[test]
    fn modes_round_trip_through_settings_names() {
        for mode in ProxyMode::ALL {
            assert_eq!(ProxyMode::from_setting(mode.as_str()), mode);
        }
        assert_eq!(ProxyMode::from_setting("unknown"), ProxyMode::System);
    }

    #[test]
    fn a_saved_proxy_password_is_bound_to_its_endpoint_and_username() {
        let original = ProxyConfig::from_parts(ProxyMode::Http, "127.0.0.1", "8080", "dummy-user", " dummy-private-password\n")
            .unwrap();
        let password = original.password_record().unwrap();
        assert!(password_is_valid(&password));
        let mut restored = ProxyConfig::from_parts(ProxyMode::Http, "127.0.0.1", "8080", "dummy-user", "").unwrap();
        assert!(restored.restore_password(&password));
        assert_eq!(restored, original);
        for (host, port, username) in
            [("other.invalid", "8080", "dummy-user"), ("127.0.0.1", "8081", "dummy-user"), ("127.0.0.1", "8080", "other-user")]
        {
            let mut changed = ProxyConfig::from_parts(ProxyMode::Http, host, port, username, "").unwrap();
            assert!(!changed.restore_password(&password));
            assert!(changed.password_record().is_none());
        }
        let broken = ProxyPassword { host: "not a host".into(), ..password };
        assert!(!password_is_valid(&broken));
    }

    #[test]
    fn http_and_socks_parse_a_host_and_port() {
        let http = manual(ManualKind::Http, "127.0.0.1", "7890", "", "").unwrap();
        assert_eq!(http.redacted().as_str(), "http://127.0.0.1:7890/");
        assert!(ProxyConfig::Http(http).librespot_url().is_some());

        let socks = manual(ManualKind::Socks, "127.0.0.1", "1080", "", "").unwrap();
        assert_eq!(socks.redacted().scheme(), "socks5h");
        assert_eq!(socks.redacted().port(), Some(1080));
        assert_eq!(ProxyConfig::Socks(socks.clone()).librespot_url(), None);
        assert!(ProxyConfig::Socks(socks).engine_unsupported().is_some());
        assert_eq!(ProxyConfig::Http(manual(ManualKind::Http, "127.0.0.1", "7890", "", "").unwrap()).engine_unsupported(), None);
        assert_eq!(ProxyConfig::System.engine_unsupported(), None);
        assert_eq!(ProxyConfig::Off.engine_unsupported(), None);
        let ProxyConfig::Socks(from_parts) = ProxyConfig::from_parts(ProxyMode::Socks, "127.0.0.1", "1080", "", "").unwrap()
        else {
            panic!("expected SOCKS");
        };
        assert_eq!(from_parts.redacted().scheme(), "socks5h");
    }

    #[test]
    fn engine_only_accepts_plain_http_proxy_urls() {
        let http_uri: http::Uri = "http://127.0.0.1:8080".parse().unwrap();
        let https_uri: http::Uri = "https://127.0.0.1:8080".parse().unwrap();
        let socks_uri: http::Uri = "socks5://127.0.0.1:1080".parse().unwrap();
        assert!(http_proxy_from_uri(&http_uri).is_some());
        assert!(http_proxy_from_uri(&https_uri).is_none());
        assert!(http_proxy_from_uri(&socks_uri).is_none());
    }

    #[test]
    fn proxy_credentials_stay_opaque_and_out_of_urls() {
        let proxy = manual(ManualKind::Http, "127.0.0.1", "7890", "alice/name", " secret/with spaces ").unwrap();
        let redacted = proxy.redacted();
        assert!(redacted.username().is_empty());
        assert_eq!(redacted.password(), None);
        assert_eq!(proxy.username, "alice/name");
        assert_eq!(proxy.password, " secret/with spaces ");
        assert!(proxy.reqwest_proxy().is_ok());
        // движку логин нужен (Proxy-Authorization), поэтому он в его адресе — но не в Debug
        let engine = ProxyConfig::Http(proxy.clone()).librespot_url().unwrap();
        assert_eq!(engine.username(), "alice%2Fname");
        assert!(ProxyConfig::Http(proxy.clone()).engine_unsupported().is_none());
        let debug = format!("{proxy:?} {:?}", ProxyConfig::Http(proxy.clone()));
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn system_proxy_authentication_and_tls_endpoints_stay_out_of_librespot() {
        let destination = http::Uri::from_static("https://apresolve.spotify.com");
        let authenticated =
            hyper_util::client::proxy::matcher::Matcher::builder().all("http://alice:secret@127.0.0.1:8080").build();
        let intercept = authenticated.intercept(&destination).unwrap();
        assert!(librespot_system_proxy(&intercept).is_none());

        let tls = hyper_util::client::proxy::matcher::Matcher::builder().all("https://127.0.0.1:8080").build();
        let intercept = tls.intercept(&destination).unwrap();
        assert!(librespot_system_proxy(&intercept).is_none());

        let plain = hyper_util::client::proxy::matcher::Matcher::builder().all("http://127.0.0.1:8080").build();
        let intercept = plain.intercept(&destination).unwrap();
        assert!(librespot_system_proxy(&intercept).is_some());
    }

    #[test]
    fn proxy_parse_rejects_a_missing_host_or_port() {
        assert!(manual(ManualKind::Http, "", "8080", "", "").unwrap_err().contains("host"));
        assert!(manual(ManualKind::Http, "127.0.0.1", "", "", "").unwrap_err().contains("port"));
        assert!(manual(ManualKind::Http, "127.0.0.1", "abc", "", "").unwrap_err().contains("number"));
        assert!(manual(ManualKind::Http, "not a host", "1080", "", "").unwrap_err().contains("server name"));
        assert!(manual(ManualKind::Http, "127.0.0.1", "0", "", "").unwrap_err().contains("65535"));
        assert!(manual(ManualKind::Http, "127.0.0.1", "65536", "", "").unwrap_err().contains("65535"));
        manual(ManualKind::Http, "localhost", "8080", "", "").unwrap();
        let v6 = manual(ManualKind::Http, "::1", "8080", "", "").unwrap();
        assert_eq!(v6.redacted().as_str(), "http://[::1]:8080/");
        manual(ManualKind::Http, "http://proxy.example", " 3128 ", "", "").unwrap();
    }

    #[test]
    fn endpoint_parts_for_the_shell_keep_the_default_port() {
        let v6 = manual(ManualKind::Socks, "[::1]", "1080", "u", "p").unwrap();
        assert_eq!((v6.host(), v6.port(), v6.username(), v6.password()), ("::1", 1080, "u", "p"));
        // http://host:80 — Url прячет порт, но пароль всё равно сохраняется и восстанавливается
        let http = manual(ManualKind::Http, "proxy.example", "80", "user", "secret").unwrap();
        assert_eq!(http.port(), 80);
        let record = http.password_record().expect("password record");
        assert_eq!(record.port, 80);
        let mut restored = manual(ManualKind::Http, "proxy.example", "80", "user", "").unwrap();
        assert!(restored.restore_password(&record));
        assert_eq!(restored.password(), "secret");
    }
}
