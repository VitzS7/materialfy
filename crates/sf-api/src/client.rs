//! Клиент Web API (https://api.spotify.com/v1): ≤6 запросов в полёте, Retry-After (≤30 с, 3 повтора, не дольше 8 с
//! ожидания на запрос, кроме очереди), квота Dev Mode.
//! Перенос spotifast/src/api/client.rs. Все вызовы идут через один `send`: bearer-токен, семафор,
//! охлаждение после 429, тексты ошибок. Различия общего/личного приложения решает gateway до вызова.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::Semaphore;

use sf_auth::credentials::{self, Grant, Lease};
use sf_auth::oauth::{self, StoredToken, TokenEndpointError};

use crate::ApiSource;
use crate::http::Http;
use crate::limits::{self, Limits};
use crate::models::*;

const BASE_URL: &str = "https://api.spotify.com/v1";
const MAX_IN_FLIGHT: usize = 6;
const RATE_LIMIT_RETRIES: u32 = 3;
const MAX_RETRY_AFTER: Duration = Duration::from_secs(30);
/// Сколько запрос (кроме добавления в очередь) вообще ждёт охлаждения после 429: Retry-After соблюдается,
/// но если он не укладывается в этот срок, запрос сразу отдаёт RateLimited — страница показывает то, что
/// загрузилось, или понятную ошибку, а не висит минутами (три повтора по 30 с).
const RATE_LIMIT_BUDGET: Duration = Duration::from_secs(8);
/// Добавление в очередь повторяется дольше обычного, но не бесконечно.
const QUEUE_RATE_LIMIT_RETRIES: u32 = 10;

/// Display — короткий текст для интерфейса через mf_i18n (единственное место перевода ошибок Web API);
/// `{:#}` добавляет подробности Spotify/сети для логов.
#[derive(Clone, Debug, Error)]
pub enum ApiError {
    NotSignedIn,
    /// message — текст Spotify (обычно английский) или свой текст интерфейса (ключ словаря mf_i18n)
    Status { status: u16, message: String },
    RateLimited,
    QuotaExhausted,
    SignInExpired { api_source: ApiSource },
    Network(String),
    Decode(String),
}

impl ApiError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// Короткий текст для пользователя.
    pub fn user_text(&self) -> String {
        match self {
            Self::NotSignedIn => mf_i18n::t("Sign in to your account"),
            Self::Status { status: 401, .. } | Self::SignInExpired { .. } => {
                mf_i18n::t("Your Spotify sign-in has expired — sign in again")
            }
            Self::Status { message, .. } if is_own_text(message) => mf_i18n::t(message),
            Self::Status { status, .. } => status_text(StatusCode::from_u16(*status).unwrap_or(StatusCode::BAD_REQUEST)),
            Self::RateLimited => mf_i18n::t("Spotify is limiting requests — try again in a moment"),
            Self::QuotaExhausted => mf_i18n::t("Spotify's development-mode quota is used up — try again after it resets"),
            Self::Network(_) => mf_i18n::t("Can't reach Spotify — check your network and proxy"),
            Self::Decode(_) => mf_i18n::t("Spotify sent a response we couldn't read"),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::Status { status, message } if !is_own_text(message) => Some(format!("HTTP {status}: {message}")),
            Self::Network(detail) | Self::Decode(detail) if !detail.is_empty() => Some(detail.clone()),
            _ => None,
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.user_text())?;
        match self.detail() {
            Some(detail) if f.alternate() => write!(f, " ({detail})"),
            _ => Ok(()),
        }
    }
}

/// Свой текст для интерфейса в `Status.message` (а не ответ Spotify): уже переведённый русский
/// или английский ключ словаря. Такие тексты показываются как есть (через перевод), без «HTTP …» в `{:#}`.
fn is_own_text(text: &str) -> bool {
    !text.is_empty()
        && (text.chars().any(|c| matches!(c, 'А'..='я' | 'ё' | 'Ё')) || mf_i18n::in_any_dictionary(text))
}

impl From<reqwest::Error> for ApiError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_decode() {
            Self::Decode(error.without_url().to_string())
        } else {
            Self::Network(error.without_url().to_string())
        }
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

fn is_quota_exhausted(body: &str) -> bool {
    serde_json::from_str::<ApiErrorBody>(body)
        .ok()
        .and_then(|body| body.error.reason)
        .is_some_and(|reason| reason == "QUOTA_EXCEEDED")
}

/// Текст ошибки, когда Spotify не прислал своего.
fn status_text(status: StatusCode) -> String {
    match status.as_u16() {
        400 => mf_i18n::t("Spotify rejected the request"),
        401 => mf_i18n::t("Spotify didn't accept the sign-in"),
        403 => mf_i18n::t("Spotify doesn't allow this action"),
        404 => mf_i18n::t("Spotify couldn't find this"),
        413 => mf_i18n::t("The request is too large"),
        code if code >= 500 => mf_i18n::t("Spotify is temporarily unavailable"),
        code => mf_i18n::tf("The request failed (HTTP {0})", &[&code]),
    }
}

/// Откуда берутся bearer-токены: PKCE-грант зарегистрированного приложения, обновляется по требованию
/// и сохраняется в хранилище. `Fixed` — только для тестов.
#[derive(Clone)]
pub enum TokenProvider {
    Web(Arc<WebTokens>),
    #[cfg(test)]
    Fixed(String),
}

impl TokenProvider {
    async fn access_token(&self) -> Result<String> {
        match self {
            Self::Web(tokens) => tokens.access_token(false).await,
            #[cfg(test)]
            Self::Fixed(token) => Ok(token.clone()),
        }
    }

    async fn invalidate(&self) {
        match self {
            Self::Web(tokens) => {
                let _ = tokens.access_token(true).await;
            }
            #[cfg(test)]
            Self::Fixed(_) => {}
        }
    }
}

/// Грант Web API: обновляется с возрастом и сохраняется после проверки аккаунта.
pub struct WebTokens {
    http: Http,
    token: tokio::sync::Mutex<StoredToken>,
    lease: Lease,
    remember: AtomicBool,
    storage_error: Arc<dyn Fn(credentials::Error) + Send + Sync>,
    source: ApiSource,
}

impl WebTokens {
    pub fn new(
        http: impl Into<Http>,
        token: StoredToken,
        lease: Lease,
        source: ApiSource,
        storage_error: Arc<dyn Fn(credentials::Error) + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            http: http.into(),
            token: tokio::sync::Mutex::new(token),
            lease,
            remember: AtomicBool::new(false),
            storage_error,
            source,
        })
    }

    /// Сохранять грант начинаем только после того, как Web API подтвердил аккаунт.
    pub async fn remember(&self) -> std::result::Result<(), credentials::Error> {
        let token = self.token.lock().await;
        self.remember.store(true, Ordering::Relaxed);
        let pending = self.lease.save(Grant::Web(token.clone()));
        drop(token);
        pending.await
    }

    /// Действующий токен: обновляется заранее (за 90 с до истечения) или принудительно после 401.
    async fn access_token(&self, force: bool) -> Result<String> {
        let mut guard = self.token.lock().await;
        if !self.lease.current() {
            return Err(ApiError::SignInExpired { api_source: self.source });
        }
        if force || guard.needs_refresh() {
            let client_id = guard.client_id.clone();
            let refresh_token = guard.refresh_token.clone();
            let client = self.http.client().map_err(ApiError::Network)?;
            match oauth::refresh(&client, &client_id, &refresh_token).await {
                Ok(response) => match StoredToken::from_response(&client_id, response, Some(&refresh_token)) {
                    Ok(updated) => {
                        if !self.lease.current() {
                            return Err(ApiError::SignInExpired { api_source: self.source });
                        }
                        if self.remember.load(Ordering::Relaxed) {
                            let pending = self.lease.save(Grant::Web(updated.clone()));
                            let notice = self.storage_error.clone();
                            let lease = self.lease.clone();
                            tokio::spawn(async move {
                                if let Err(error) = pending.await
                                    && lease.current()
                                {
                                    notice(error);
                                }
                            });
                        }
                        *guard = updated;
                    }
                    Err(error) => tracing::warn!("token refresh returned an unusable response: {error:#}"),
                },
                Err(TokenEndpointError::Rejected { .. }) => {
                    return Err(ApiError::SignInExpired { api_source: self.source });
                }
                Err(TokenEndpointError::Unreachable(detail)) => {
                    if force || guard.expired() {
                        return Err(ApiError::Network(detail));
                    }
                    tracing::warn!("token refresh failed, keeping the current token: {detail}");
                }
            }
        }
        Ok(guard.access_token.clone())
    }
}

/// Что запустить.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PlayRequest {
    pub context_uri: Option<String>,
    pub uris: Vec<String>,
    pub offset_uri: Option<String>,
    pub offset_position: Option<u32>,
    pub position_ms: u32,
}

impl PlayRequest {
    pub fn context(uri: impl Into<String>) -> Self {
        Self { context_uri: Some(uri.into()), ..Self::default() }
    }

    pub fn tracks(uris: Vec<String>) -> Self {
        Self { uris, ..Self::default() }
    }

    pub fn starting_at_uri(mut self, uri: impl Into<String>) -> Self {
        self.offset_uri = Some(uri.into());
        self
    }

    pub fn starting_at_index(mut self, index: u32) -> Self {
        self.offset_position = Some(index);
        self
    }

    fn body(&self) -> Value {
        let mut body = serde_json::Map::new();
        // Одиночный трек/выпуск Web API играет только как `uris` (как context_uri — 400 «Non supported context
        // uri»). Если возобновление называет более поздний трек (автовоспроизведение ушло дальше) — играем его.
        let single_item = self
            .context_uri
            .as_deref()
            .filter(|uri| matches!(crate::util::uri_kind(uri), Some("track" | "episode")))
            .map(|context| self.offset_uri.as_deref().unwrap_or(context));
        if let Some(item) = single_item {
            body.insert("uris".into(), json!([item]));
        } else if let Some(context) = &self.context_uri {
            body.insert("context_uri".into(), json!(context));
        } else if !self.uris.is_empty() {
            body.insert("uris".into(), json!(self.uris));
        }
        // у одиночного элемента смещение ничего не добавляет
        let offset = single_item.is_none();
        if let Some(uri) = self.offset_uri.as_ref().filter(|_| offset) {
            body.insert("offset".into(), json!({ "uri": uri }));
        } else if let Some(position) = self.offset_position.filter(|_| offset) {
            body.insert("offset".into(), json!({ "position": position }));
        }
        if self.position_ms > 0 {
            body.insert("position_ms".into(), json!(self.position_ms));
        }
        Value::Object(body)
    }
}

/// Живой признак сетевой активности: интерфейс показывает, что ждём Spotify, а не тормозим сами.
pub struct NetActivity {
    started_at: Instant,
    in_flight: AtomicUsize,
    /// мс от `started_at`, когда начался текущий непрерывный период запросов
    busy_since_ms: AtomicU64,
}

impl Default for NetActivity {
    fn default() -> Self {
        Self { started_at: Instant::now(), in_flight: AtomicUsize::new(0), busy_since_ms: AtomicU64::new(0) }
    }
}

impl NetActivity {
    fn now_ms(&self) -> u64 {
        self.started_at.elapsed().as_millis() as u64
    }

    fn begin(&self) {
        if self.in_flight.fetch_add(1, Ordering::SeqCst) == 0 {
            self.busy_since_ms.store(self.now_ms(), Ordering::SeqCst);
        }
    }

    fn end(&self) {
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
    }

    /// Запросы непрерывно в полёте не меньше `for_at_least`.
    pub fn busy(&self, for_at_least: Duration) -> bool {
        self.in_flight.load(Ordering::SeqCst) > 0
            && self.now_ms().saturating_sub(self.busy_since_ms.load(Ordering::SeqCst))
                >= for_at_least.as_millis() as u64
    }
}

/// Сбрасывает кэш GET после записи, даже если future запроса бросили.
struct WroteGuard<'a>(&'a Limits);

impl Drop for WroteGuard<'_> {
    fn drop(&mut self) {
        self.0.wrote();
    }
}

/// Снимает счётчик, даже если future запроса бросили.
struct ActivityGuard<'a>(&'a NetActivity);

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        self.0.end();
    }
}

pub struct ApiClient {
    #[cfg(test)]
    base_url: Option<String>,
    http: Http,
    tokens: Mutex<Option<TokenProvider>>,
    limiter: Semaphore,
    queue_writes: tokio::sync::Mutex<()>,
    cooldown_until: tokio::sync::Mutex<Instant>,
    search_limit: u32,
    artist_albums_limit: u32,
    source: ApiSource,
    activity: Arc<NetActivity>,
    /// Бюджет, склейка и кэш GET, охлаждение источника — общие для клиентов одного ApiGateway.
    limits: Arc<Limits>,
}

impl ApiClient {
    pub fn new(
        http: impl Into<Http>,
        activity: Arc<NetActivity>,
        search_limit: u32,
        artist_albums_limit: u32,
        source: ApiSource,
    ) -> Self {
        Self {
            #[cfg(test)]
            base_url: None,
            http: http.into(),
            tokens: Mutex::new(None),
            limiter: Semaphore::new(MAX_IN_FLIGHT),
            queue_writes: tokio::sync::Mutex::new(()),
            cooldown_until: tokio::sync::Mutex::new(Instant::now()),
            search_limit,
            artist_albums_limit,
            source,
            activity,
            limits: Limits::new(),
        }
    }

    /// Общие с другими клиентами лимиты (ApiGateway даёт один набор на оба источника).
    pub fn with_limits(mut self, limits: Arc<Limits>) -> Self {
        self.limits = limits;
        self
    }

    pub fn set_token_provider(&self, provider: Option<TokenProvider>) {
        *self.tokens.lock().unwrap_or_else(|p| p.into_inner()) = provider;
    }

    /// Новая авторизация — свой провайдер и своё охлаждение: старый запрос не должен подхватить токены
    /// сменившегося аккаунта.
    pub fn for_authorization(&self, provider: TokenProvider) -> Self {
        let client =
            Self::new(self.http.clone(), self.activity.clone(), self.search_limit, self.artist_albums_limit, self.source)
                .with_limits(self.limits.clone());
        client.set_token_provider(Some(provider));
        client
    }

    pub fn source(&self) -> ApiSource {
        self.source
    }

    fn provider(&self) -> Result<TokenProvider> {
        self.tokens.lock().unwrap_or_else(|p| p.into_inner()).clone().ok_or(ApiError::NotSignedIn)
    }

    /// Ждёт конца охлаждения; false — оно кончится позже `deadline` (тогда не ждём вовсе).
    async fn wait_for_cooldown(&self, deadline: Option<Instant>) -> bool {
        loop {
            let until = *self.cooldown_until.lock().await;
            let Some(wait) = until.checked_duration_since(Instant::now()) else {
                return true;
            };
            if deadline.is_some_and(|deadline| until > deadline) {
                return false;
            }
            tokio::time::sleep(wait).await;
        }
    }

    async fn extend_cooldown(&self, wait: Duration) {
        let mut until = self.cooldown_until.lock().await;
        *until = (*until).max(Instant::now() + wait);
    }

    // ---- транспорт ----

    async fn send(&self, method: Method, path: &str, query: &[(&str, String)], body: Option<&Value>) -> Result<String> {
        if method == Method::GET && body.is_none() {
            // одинаковые GET в полёте — один запрос, ответ живёт в кэше limits несколько секунд
            return self.limits.get(self.source, path, query, self.send_body(method, path, query, None, None)).await;
        }
        self.send_body(method, path, query, body, None).await
    }

    async fn send_body(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        jpeg: Option<&str>,
    ) -> Result<String> {
        let url = if path.starts_with("http") {
            path.to_string()
        } else {
            #[cfg(test)]
            let base = self.base_url.as_deref().unwrap_or(BASE_URL);
            #[cfg(not(test))]
            let base = BASE_URL;
            format!("{base}{path}")
        };
        let provider = self.provider()?;
        // запись сбрасывает кэш GET, чем бы ни кончилась; команды плееру и очередь — нет: их GET не кэшируются
        let _wrote = (method != Method::GET && !path.starts_with("/me/player")).then(|| WroteGuard(&self.limits));
        let priority = limits::priority_of(method == Method::GET, path);
        self.limits.admit(self.source, priority).await?;
        let started = Instant::now();
        // Один логический запрос, даже если он ждёт семафор или охлаждение: признак активности живёт всё это время.
        self.activity.begin();
        let _activity = ActivityGuard(&self.activity);

        let mut attempt = 0;
        let queue_write = method == Method::POST && path == "/me/player/queue";
        let deadline = (!queue_write).then(|| started + RATE_LIMIT_BUDGET);
        loop {
            attempt = u32::saturating_add(attempt, 1);
            if attempt > 1 {
                self.limits.count_retry(self.source);
            }
            if !self.wait_for_cooldown(deadline).await {
                tracing::debug!("Spotify source={}: 429 cooldown is longer than the request budget {path}", self.source);
                return Err(ApiError::RateLimited);
            }
            let permit = self.limiter.acquire().await.map_err(|_| ApiError::NotSignedIn)?;
            let token = provider.access_token().await?;
            let mut request = self
                .http
                .client()
                .map_err(ApiError::Network)?
                .request(method.clone(), &url)
                .bearer_auth(&token)
                .query(query);
            if let Some(jpeg) = jpeg {
                request = request.header(reqwest::header::CONTENT_TYPE, "image/jpeg").body(jpeg.to_owned());
            } else if let Some(body) = body {
                request = request.json(body);
            } else if matches!(method, Method::PUT | Method::POST | Method::DELETE) {
                request = request.header(reqwest::header::CONTENT_LENGTH, "0");
            }
            let sent_at = Instant::now();
            let response = request.send().await?;
            let status = response.status();

            if status == StatusCode::UNAUTHORIZED && attempt == 1 {
                drop(permit);
                provider.invalidate().await;
                continue;
            }
            if status == StatusCode::TOO_MANY_REQUESTS {
                let wait = response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok())
                    .map_or(Duration::from_secs(1), Duration::from_secs);
                let text = response.text().await.unwrap_or_default();
                let quota = is_quota_exhausted(&text);
                self.limits.note_limited(self.source, wait, quota);
                if quota {
                    return Err(ApiError::QuotaExhausted);
                }
                // Отклонённое добавление в очередь безопасно повторить: оно держит место в замке записи и повторяется
                // дольше обычного. Общее охлаждение — не дольше MAX_RETRY_AFTER: долгий Retry-After не должен
                // останавливать весь источник; такую очередь отдаём UI как RateLimited.
                tracing::warn!("Spotify is rate limiting source={} wait={wait:?}", self.source);
                drop(permit);
                self.extend_cooldown(limits::jittered(wait).min(MAX_RETRY_AFTER)).await;
                // фон и опрос после 429 не повторяют: каждый повтор — ещё запрос в и так полное окно
                let retries = if queue_write {
                    QUEUE_RATE_LIMIT_RETRIES
                } else if priority == limits::Priority::Interactive {
                    RATE_LIMIT_RETRIES
                } else {
                    0
                };
                let over_budget = deadline.is_some_and(|deadline| Instant::now() + wait > deadline);
                if attempt > retries || over_budget || (queue_write && wait > MAX_RETRY_AFTER) {
                    return Err(ApiError::RateLimited);
                }
                continue;
            }
            if status.is_server_error() && method == Method::GET && attempt == 1 {
                drop(permit);
                tokio::time::sleep(limits::jittered(Duration::from_millis(700))).await;
                continue;
            }
            let text = response.text().await?;
            tracing::debug!(
                "Spotify source={} method={} status={} duration_ms={}",
                self.source,
                method,
                status.as_u16(),
                started.elapsed().as_millis()
            );
            if status.is_success() {
                self.limits.note_success(self.source, sent_at);
                return Ok(text);
            }
            let message = serde_json::from_str::<ApiErrorBody>(&text)
                .ok()
                .map(|body| body.error.message)
                .filter(|message| !message.is_empty())
                .unwrap_or_else(|| status_text(status));
            return Err(ApiError::Status { status: status.as_u16(), message });
        }
    }

    async fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T> {
        let text = self.send(Method::GET, path, query, None).await?;
        if text.trim().is_empty() {
            return serde_json::from_value(Value::Null).map_err(|error| ApiError::Decode(error.to_string()));
        }
        serde_json::from_str(&text).map_err(|error| ApiError::Decode(error.to_string()))
    }

    async fn get_optional<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<Option<T>> {
        // 204 без тела — ничего не играет
        let text = self.send(Method::GET, path, query, None).await?;
        if text.trim().is_empty() || text.trim() == "null" {
            return Ok(None);
        }
        serde_json::from_str(&text).map(Some).map_err(|error| ApiError::Decode(error.to_string()))
    }

    /// Изменение. Успех решает статус; тело читаем, только если оно нужно: ответы команд плеера не всегда JSON,
    /// и считать нечитаемое тело ошибкой значило бы сообщать «не запустилось», когда музыка уже играет.
    async fn write(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<Option<Value>> {
        let text = self.send(method, path, query, body).await?;
        if text.trim().is_empty() {
            return Ok(None);
        }
        match serde_json::from_str(&text) {
            Ok(value) => Ok(Some(value)),
            Err(error) => {
                tracing::debug!("Spotify source={}: successful response is not JSON: {error:#}", self.source);
                Ok(None)
            }
        }
    }

    // ---- аккаунт и плеер ----

    pub async fn me(&self) -> Result<User> {
        self.get("/me", &[]).await
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        let list: DeviceList = self.get("/me/player/devices", &[]).await?;
        Ok(list.devices)
    }

    pub async fn playback_state(&self) -> Result<Option<PlaybackState>> {
        self.get_optional("/me/player", &[("additional_types", "track,episode".to_string())]).await
    }

    pub async fn queue(&self) -> Result<Queue> {
        self.get("/me/player/queue", &[]).await
    }

    pub async fn recently_played(
        &self,
        limit: u32,
        after: Option<&str>,
        before: Option<&str>,
    ) -> Result<CursorPage<PlayHistory>> {
        let mut query = vec![("limit", limit.to_string())];
        if let Some(after) = after {
            query.push(("after", after.to_string()));
        }
        if let Some(before) = before {
            query.push(("before", before.to_string()));
        }
        self.get("/me/player/recently-played", &query).await
    }

    fn device_query(device_id: Option<&str>) -> Vec<(&'static str, String)> {
        device_id.map(|id| vec![("device_id", id.to_string())]).unwrap_or_default()
    }

    pub async fn play(&self, device_id: Option<&str>, request: Option<&PlayRequest>) -> Result<()> {
        let body = request.map(PlayRequest::body);
        self.write(Method::PUT, "/me/player/play", &Self::device_query(device_id), body.as_ref()).await?;
        Ok(())
    }

    pub async fn pause(&self, device_id: Option<&str>) -> Result<()> {
        self.write(Method::PUT, "/me/player/pause", &Self::device_query(device_id), None).await?;
        Ok(())
    }

    pub async fn next(&self, device_id: Option<&str>) -> Result<()> {
        self.write(Method::POST, "/me/player/next", &Self::device_query(device_id), None).await?;
        Ok(())
    }

    pub async fn previous(&self, device_id: Option<&str>) -> Result<()> {
        self.write(Method::POST, "/me/player/previous", &Self::device_query(device_id), None).await?;
        Ok(())
    }

    pub async fn seek(&self, position_ms: u32, device_id: Option<&str>) -> Result<()> {
        let mut query = Self::device_query(device_id);
        query.push(("position_ms", position_ms.to_string()));
        self.write(Method::PUT, "/me/player/seek", &query, None).await?;
        Ok(())
    }

    pub async fn set_volume(&self, percent: u8, device_id: Option<&str>) -> Result<()> {
        let mut query = Self::device_query(device_id);
        query.push(("volume_percent", percent.min(100).to_string()));
        self.write(Method::PUT, "/me/player/volume", &query, None).await?;
        Ok(())
    }

    pub async fn set_shuffle(&self, state: bool, device_id: Option<&str>) -> Result<()> {
        let mut query = Self::device_query(device_id);
        query.push(("state", state.to_string()));
        self.write(Method::PUT, "/me/player/shuffle", &query, None).await?;
        Ok(())
    }

    /// `state` — `off`, `context` или `track`.
    pub async fn set_repeat(&self, state: &str, device_id: Option<&str>) -> Result<()> {
        let mut query = Self::device_query(device_id);
        query.push(("state", state.to_string()));
        self.write(Method::PUT, "/me/player/repeat", &query, None).await?;
        Ok(())
    }

    pub async fn transfer(&self, device_id: &str, play: bool) -> Result<()> {
        let body = json!({ "device_ids": [device_id], "play": play });
        self.write(Method::PUT, "/me/player", &[], Some(&body)).await?;
        Ok(())
    }

    pub async fn add_to_queue(&self, uri: &str, device_id: Option<&str>) -> Result<()> {
        let _write = self.queue_writes.lock().await;
        self.append_to_queue(uri, device_id).await
    }

    async fn append_to_queue(&self, uri: &str, device_id: Option<&str>) -> Result<()> {
        let mut query = Self::device_query(device_id);
        query.push(("uri", uri.to_string()));
        self.write(Method::POST, "/me/player/queue", &query, None).await?;
        Ok(())
    }

    /// Spotify добавляет по одному треку за запрос. Ждём каждый, чтобы сохранить порядок альбома (и повторы),
    /// и останавливаемся на первой ошибке. Возвращает, сколько добавлено.
    pub async fn add_many_to_queue(&self, uris: &[String], device_id: Option<&str>) -> (usize, Result<()>) {
        let _write = self.queue_writes.lock().await;
        for (added, uri) in uris.iter().enumerate() {
            if let Err(error) = self.append_to_queue(uri, device_id).await {
                return (added, Err(error));
            }
        }
        (uris.len(), Ok(()))
    }

    // ---- плейлисты ----

    pub async fn my_playlists(&self, offset: u32, limit: u32) -> Result<Page<Playlist>> {
        self.get("/me/playlists", &[("limit", limit.to_string()), ("offset", offset.to_string())]).await
    }

    pub async fn playlist(&self, id: &str) -> Result<Playlist> {
        self.get(&format!("/playlists/{id}"), &[]).await
    }

    pub async fn playlist_items(&self, id: &str, offset: u32, limit: u32) -> Result<Page<PlaylistItem>> {
        self.get::<PositionedPage<PlaylistItem>>(
            &format!("/playlists/{id}/items"),
            &[
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
                ("additional_types", "track,episode".to_string()),
            ],
        )
        .await
        .map(Into::into)
    }

    /// Какие из запрошенных треков уже есть в плейлисте. Отдельного запроса членства у Spotify нет — идём по
    /// страницам, пока не найдём все или плейлист не кончится.
    pub async fn playlist_duplicates(&self, id: &str, uris: &[String]) -> Result<Vec<String>> {
        let wanted: HashSet<&str> = uris.iter().map(String::as_str).collect();
        if wanted.is_empty() {
            return Ok(Vec::new());
        }
        let mut found = HashSet::new();
        let mut offset = 0;
        loop {
            let page = self.playlist_items(id, offset, 50).await?;
            for uri in page.items.iter().filter_map(PlaylistItem::playable).map(PlayableItem::uri) {
                if wanted.contains(uri) {
                    found.insert(uri.to_string());
                }
            }
            if found.len() == wanted.len() {
                break;
            }
            let Some(next) = page.next_offset() else {
                break;
            };
            offset = next;
        }
        Ok(uris.iter().filter(|uri| found.contains(uri.as_str())).cloned().collect())
    }

    pub async fn create_playlist(&self, name: &str, public: bool, description: &str) -> Result<Playlist> {
        let body = json!({ "name": name, "public": public, "description": description });
        let value = self.write(Method::POST, "/me/playlists", &[], Some(&body)).await?.unwrap_or(Value::Null);
        serde_json::from_value(value).map_err(|error| ApiError::Decode(error.to_string()))
    }

    /// `encoded` — JPEG в base64 (playlist_cover::prepare), не больше MAX_PAYLOAD.
    pub async fn upload_playlist_cover(&self, id: &str, encoded: &str) -> Result<()> {
        if encoded.is_empty() || encoded.len() > crate::playlist_cover::MAX_PAYLOAD {
            return Err(ApiError::Status { status: 413, message: mf_i18n::t("Choose a smaller image.") });
        }
        self.send_body(Method::PUT, &format!("/playlists/{id}/images"), &[], None, Some(encoded)).await?;
        Ok(())
    }

    pub async fn update_playlist(
        &self,
        id: &str,
        name: Option<&str>,
        description: Option<&str>,
        public: Option<bool>,
    ) -> Result<()> {
        let mut body = serde_json::Map::new();
        if let Some(name) = name {
            body.insert("name".into(), json!(name));
        }
        if let Some(description) = description {
            body.insert("description".into(), json!(description));
        }
        if let Some(public) = public {
            body.insert("public".into(), json!(public));
        }
        self.write(Method::PUT, &format!("/playlists/{id}"), &[], Some(&Value::Object(body))).await?;
        Ok(())
    }

    /// Возвращает новый snapshot_id, если Spotify его прислал.
    pub async fn add_playlist_items(&self, id: &str, uris: &[String], position: Option<u32>) -> Result<Option<String>> {
        let mut body = json!({ "uris": uris });
        if let Some(position) = position {
            body["position"] = json!(position);
        }
        let value = self.write(Method::POST, &format!("/playlists/{id}/items"), &[], Some(&body)).await?;
        Ok(Self::snapshot(value))
    }

    pub async fn remove_playlist_items(
        &self,
        id: &str,
        uris: &[String],
        snapshot_id: Option<&str>,
    ) -> Result<Option<String>> {
        let entries: Vec<Value> = uris.iter().map(|uri| json!({ "uri": uri })).collect();
        let mut body = json!({ "items": entries });
        if let Some(snapshot) = snapshot_id {
            body["snapshot_id"] = json!(snapshot);
        }
        let value = self.write(Method::DELETE, &format!("/playlists/{id}/items"), &[], Some(&body)).await?;
        Ok(Self::snapshot(value))
    }

    pub async fn reorder_playlist(
        &self,
        id: &str,
        range_start: u32,
        insert_before: u32,
        snapshot_id: Option<&str>,
    ) -> Result<Option<String>> {
        let mut body = json!({
            "range_start": range_start,
            "insert_before": insert_before,
            "range_length": 1,
        });
        if let Some(snapshot) = snapshot_id {
            body["snapshot_id"] = json!(snapshot);
        }
        let value = self.write(Method::PUT, &format!("/playlists/{id}/items"), &[], Some(&body)).await?;
        Ok(Self::snapshot(value))
    }

    fn snapshot(value: Option<Value>) -> Option<String> {
        value
            .and_then(|value| serde_json::from_value::<SnapshotId>(value).ok())
            .and_then(|snapshot| snapshot.snapshot_id)
    }

    pub async fn follow_playlist(&self, id: &str) -> Result<()> {
        self.write(Method::PUT, "/me/library", &[("uris", format!("spotify:playlist:{id}"))], None).await?;
        Ok(())
    }

    pub async fn unfollow_playlist(&self, id: &str) -> Result<()> {
        self.write(Method::DELETE, "/me/library", &[("uris", format!("spotify:playlist:{id}"))], None).await?;
        Ok(())
    }

    // ---- библиотека ----

    pub async fn saved_tracks(&self, offset: u32, limit: u32) -> Result<Page<SavedTrack>> {
        self.get("/me/tracks", &[("limit", limit.to_string()), ("offset", offset.to_string())]).await
    }

    pub async fn saved_albums(&self, offset: u32, limit: u32) -> Result<Page<SavedAlbum>> {
        self.get("/me/albums", &[("limit", limit.to_string()), ("offset", offset.to_string())]).await
    }

    pub async fn followed_artists(&self, after: Option<&str>, limit: u32) -> Result<CursorPage<Artist>> {
        let mut query = vec![("type", "artist".to_string()), ("limit", limit.to_string())];
        if let Some(after) = after {
            query.push(("after", after.to_string()));
        }
        let followed: FollowedArtists = self.get("/me/following", &query).await?;
        Ok(followed.artists)
    }

    pub async fn saved_shows(&self, offset: u32, limit: u32) -> Result<Page<SavedShow>> {
        self.get("/me/shows", &[("limit", limit.to_string()), ("offset", offset.to_string())]).await
    }

    pub async fn saved_episodes(&self, offset: u32, limit: u32) -> Result<Page<SavedEpisode>> {
        self.get("/me/episodes", &[("limit", limit.to_string()), ("offset", offset.to_string())]).await
    }

    /// `time_range` — short_term | medium_term | long_term.
    pub async fn top_tracks(&self, time_range: &str, limit: u32, offset: u32) -> Result<Page<Track>> {
        self.get(
            "/me/top/tracks",
            &[
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
                ("time_range", time_range.to_string()),
            ],
        )
        .await
    }

    pub async fn top_artists(&self, time_range: &str, limit: u32) -> Result<Page<Artist>> {
        self.get("/me/top/artists", &[("limit", limit.to_string()), ("time_range", time_range.to_string())]).await
    }

    async fn library_write(&self, method: Method, uris: &[String]) -> Result<()> {
        self.write(method, "/me/library", &[("uris", uris.join(","))], None).await?;
        Ok(())
    }

    /// Сохраняет треки, альбомы, исполнителей, шоу, выпуски или плейлисты (по URI).
    pub async fn save(&self, uris: &[String]) -> Result<()> {
        self.library_write(Method::PUT, uris).await
    }

    pub async fn unsave(&self, uris: &[String]) -> Result<()> {
        self.library_write(Method::DELETE, uris).await
    }

    /// Есть ли каждый URI в библиотеке — в том же порядке, что `uris`.
    pub async fn contains(&self, uris: &[String]) -> Result<Vec<bool>> {
        self.get("/me/library/contains", &[("uris", uris.join(","))]).await
    }

    // ---- каталог ----

    /// Лимит — из профиля (общий ключ 20, личный 10).
    pub async fn search(&self, query: &str, types: &[&str]) -> Result<SearchResults> {
        self.get(
            "/search",
            &[("q", query.to_string()), ("type", types.join(",")), ("limit", self.search_limit.to_string())],
        )
        .await
    }

    /// Плейлисты категории browse (личные — для «Made For You»). Приложениям в режиме разработки — 404.
    pub async fn category_playlists(&self, category_id: &str, offset: u32, limit: u32) -> Result<Page<Playlist>> {
        let query = [("limit", limit.to_string()), ("offset", offset.to_string())];
        let category: CategoryPlaylists = self.get(&format!("/browse/categories/{category_id}/playlists"), &query).await?;
        Ok(category.playlists)
    }

    pub async fn artist(&self, id: &str) -> Result<Artist> {
        self.get(&format!("/artists/{id}"), &[]).await
    }

    pub async fn artist_top_tracks(&self, id: &str) -> Result<Vec<Track>> {
        self.get::<TopTracks>(&format!("/artists/{id}/top-tracks"), &[]).await.map(|top| top.tracks)
    }

    pub async fn artist_albums(&self, id: &str, include_groups: &str, offset: u32, limit: u32) -> Result<Page<Album>> {
        let limit = limit.min(self.artist_albums_limit);
        self.get(
            &format!("/artists/{id}/albums"),
            &[
                ("include_groups", include_groups.to_string()),
                ("limit", limit.to_string()),
                ("offset", offset.to_string()),
            ],
        )
        .await
    }

    pub async fn related_artists(&self, id: &str) -> Result<Vec<Artist>> {
        let related: RelatedArtists = self.get(&format!("/artists/{id}/related-artists"), &[]).await?;
        Ok(related.artists)
    }

    pub async fn album(&self, id: &str) -> Result<Album> {
        self.get(&format!("/albums/{id}"), &[]).await
    }

    pub async fn album_tracks(&self, id: &str, offset: u32, limit: u32) -> Result<Page<Track>> {
        self.get::<PositionedPage<Track>>(
            &format!("/albums/{id}/tracks"),
            &[("limit", limit.to_string()), ("offset", offset.to_string())],
        )
        .await
        .map(Into::into)
    }

    pub async fn show(&self, id: &str) -> Result<Show> {
        self.get(&format!("/shows/{id}"), &[]).await
    }

    pub async fn show_episodes(&self, id: &str, offset: u32, limit: u32) -> Result<Page<Episode>> {
        self.get(&format!("/shows/{id}/episodes"), &[("limit", limit.to_string()), ("offset", offset.to_string())])
            .await
    }

    pub async fn track(&self, id: &str) -> Result<Track> {
        self.get(&format!("/tracks/{id}"), &[]).await
    }

    pub async fn episode(&self, id: &str) -> Result<Episode> {
        self.get(&format!("/episodes/{id}"), &[]).await
    }

    pub async fn recommendations(&self, seed_tracks: &[String], seed_artists: &[String], limit: u32) -> Result<Vec<Track>> {
        let mut query = vec![("limit", limit.to_string())];
        if !seed_tracks.is_empty() {
            query.push(("seed_tracks", seed_tracks.join(",")));
        }
        if !seed_artists.is_empty() {
            query.push(("seed_artists", seed_artists.join(",")));
        }
        let recommendations: Recommendations = self.get("/recommendations", &query).await?;
        Ok(recommendations.tracks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client(base: String, http: reqwest::Client) -> ApiClient {
        let mut client = ApiClient::new(http, Arc::new(NetActivity::default()), 20, 50, ApiSource::Shared);
        client.base_url = Some(base);
        client.set_token_provider(Some(TokenProvider::Fixed("test-only".into())));
        client
    }

    #[tokio::test]
    async fn revoked_provider_cannot_return_or_persist_its_token() {
        let root = std::env::temp_dir().join(format!("sf-api-revoked-provider-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let store = sf_auth::Store::in_memory(root.clone());
        let slot = sf_auth::Slot::Shared;
        let tokens = WebTokens::new(
            reqwest::Client::new(),
            StoredToken {
                client_id: sf_auth::DEFAULT_WEB_CLIENT_ID.into(),
                access_token: "dummy-access".into(),
                refresh_token: "dummy-refresh".into(),
                expires_at: u64::MAX,
                scope: String::new(),
            },
            store.lease(slot),
            ApiSource::Shared,
            Arc::new(|_| {}),
        );
        // проверка аккаунта пользуется токеном в памяти, ещё не сохраняя его
        assert_eq!(tokens.access_token(false).await.unwrap(), "dummy-access");
        assert!(store.lease(slot).load().await.unwrap().grant.is_none());
        tokens.remember().await.unwrap();
        assert!(store.lease(slot).load().await.unwrap().grant.is_some());
        store.revoke_spotify().unwrap();
        store.lease(slot).delete().await.unwrap();
        assert!(matches!(tokens.access_token(false).await, Err(ApiError::SignInExpired { .. })));
        assert_eq!(tokens.remember().await, Err(credentials::Error::Stale));
        assert!(store.lease(slot).load().await.unwrap().grant.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn queue_batch_preserves_order_and_duplicates_and_stops_on_failure() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (fail, rate_limit) in [(false, false), (true, false), (false, true)] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let first_request = Arc::new(tokio::sync::Notify::new());
            let notify = Arc::clone(&first_request);
            let server = tokio::spawn(async move {
                let mut paths = Vec::new();
                let mut cooldown_started: Option<Instant> = None;
                for index in 0..if fail {
                    2
                } else if rate_limit {
                    8
                } else {
                    4
                } {
                    let (mut socket, _) =
                        tokio::time::timeout(Duration::from_secs(5), listener.accept()).await.unwrap().unwrap();
                    if index == 2 && rate_limit {
                        assert!(cooldown_started.unwrap().elapsed() >= Duration::from_secs(1));
                    }
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        request.push(socket.read_u8().await.unwrap());
                        assert!(request.len() < 8192);
                    }
                    paths.push(String::from_utf8(request).unwrap().lines().next().unwrap().to_string());
                    if index == 0 {
                        notify.notify_one();
                    }
                    assert!(
                        tokio::time::timeout(Duration::from_millis(10), listener.accept()).await.is_err(),
                        "the next add must wait for this response"
                    );
                    let limited = rate_limit && (1..=4).contains(&index);
                    let status = if limited {
                        "429 Too Many Requests"
                    } else if fail && index == 1 {
                        "403 Forbidden"
                    } else {
                        "204 No Content"
                    };
                    let retry_after = if limited && index == 1 { 1 } else { 0 };
                    cooldown_started = Some(Instant::now());
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 {status}\r\nRetry-After: {retry_after}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            )
                            .as_bytes(),
                        )
                        .await
                        .unwrap();
                }
                paths
            });
            let http = reqwest::Client::builder().no_proxy().build().unwrap();
            let client = test_client(format!("http://{address}"), http);
            let mut uris = vec!["spotify:track:a".into(), "spotify:track:b".into(), "spotify:track:a".into()];
            let (added, result) = if fail {
                client.add_many_to_queue(&uris, Some("phone")).await
            } else {
                let (album, later_song) = tokio::join!(client.add_many_to_queue(&uris, Some("phone")), async {
                    first_request.notified().await;
                    client.add_to_queue("spotify:track:later", Some("phone")).await
                });
                later_song.unwrap();
                uris.push("spotify:track:later".into());
                album
            };
            assert_eq!(result.is_err(), fail);
            assert_eq!(added, if fail { 1 } else { 3 });
            if fail {
                assert_eq!(result.unwrap_err().status(), Some(403));
            }
            let paths = server.await.unwrap();
            if rate_limit {
                // повторяется только отклонённый трек, даже сверх обычного бюджета; одиночный — после альбома
                uris.splice(1..1, std::iter::repeat_n("spotify:track:b".into(), 4));
            }
            assert_eq!(paths.len(), if fail { 2 } else { uris.len() });
            for (path, expected) in paths.iter().zip(uris) {
                assert!(path.starts_with("POST /me/player/queue?"));
                let url =
                    reqwest::Url::parse(&format!("http://test{}", path.split_whitespace().nth(1).unwrap())).unwrap();
                let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
                assert_eq!(query["uri"], expected);
                assert_eq!(query["device_id"], "phone");
            }
        }
    }

    /// Простой HTTP-сервер на один запрос: возвращает полученный запрос целиком.
    fn serve_once(status: u16, body: &'static str) -> (std::net::SocketAddr, std::thread::JoinHandle<Vec<u8>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut received = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0);
                received.extend_from_slice(&buffer[..count]);
                if let Some(end) = received.windows(4).position(|part| part == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&received[..end]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map_or(0, |value| value.trim().parse().unwrap());
                    if received.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            write!(
                socket,
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
            received
        });
        (address, server)
    }

    #[tokio::test]
    async fn cover_upload_sends_raw_base64_jpeg_and_reports_spotify_errors() {
        for status in [202, 403, 413] {
            let (address, server) = serve_once(status, "");
            let client = test_client(format!("http://{address}"), reqwest::Client::new());
            let result = client.upload_playlist_cover("test-playlist", "/9j/test").await;
            if status == 202 {
                assert!(result.is_ok());
            } else {
                assert_eq!(result.unwrap_err().status(), Some(status));
            }
            let received = String::from_utf8(server.join().unwrap()).unwrap();
            assert!(received.starts_with("PUT /playlists/test-playlist/images HTTP/1.1"));
            assert!(received.to_lowercase().contains("content-type: image/jpeg"));
            assert_eq!(received.split("\r\n\r\n").nth(1), Some("/9j/test"));
        }
    }

    #[tokio::test]
    async fn oversized_cover_is_rejected_before_authentication_or_network() {
        let client = ApiClient::new(reqwest::Client::new(), Arc::new(NetActivity::default()), 20, 50, ApiSource::Shared);
        let result = client.upload_playlist_cover("test", &"x".repeat(crate::playlist_cover::MAX_PAYLOAD + 1)).await;
        assert_eq!(result.unwrap_err().status(), Some(413));
    }

    #[tokio::test]
    async fn requests_without_a_provider_are_not_signed_in() {
        let client = ApiClient::new(reqwest::Client::new(), Arc::new(NetActivity::default()), 20, 50, ApiSource::Shared);
        assert!(matches!(client.me().await, Err(ApiError::NotSignedIn)));
        assert_eq!(ApiError::NotSignedIn.to_string(), "Sign in to your account");
    }

    #[test]
    fn own_status_texts_are_shown_without_http_details() {
        let own = ApiError::Status { status: 413, message: mf_i18n::t("Choose a smaller image.") };
        assert_eq!(format!("{own:#}"), "Choose a smaller image.");
        let translated = ApiError::Status { status: 409, message: "Плейлист изменился".into() };
        assert_eq!(format!("{translated:#}"), "Плейлист изменился");
        let spotify = ApiError::Status { status: 413, message: "Payload too large".into() };
        assert_eq!(format!("{spotify:#}"), "The request is too large (HTTP 413: Payload too large)");
    }

    #[tokio::test]
    async fn spotify_error_message_and_fallback_text() {
        let (address, server) = serve_once(404, r#"{"error":{"status":404,"message":"Resource not found"}}"#);
        let client = test_client(format!("http://{address}"), reqwest::Client::new());
        let error = client.album("missing").await.unwrap_err();
        assert_eq!(error.status(), Some(404));
        assert_eq!(error.to_string(), "Spotify couldn't find this");
        assert_eq!(format!("{error:#}"), "Spotify couldn't find this (HTTP 404: Resource not found)");
        let received = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(received.starts_with("GET /albums/missing HTTP/1.1"));
        assert!(received.to_lowercase().contains("authorization: bearer test-only"));

        let (address, server) = serve_once(403, "");
        let client = test_client(format!("http://{address}"), reqwest::Client::new());
        let error = client.pause(None).await.unwrap_err();
        assert_eq!(error.to_string(), "Spotify doesn't allow this action");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn player_command_succeeds_on_a_non_json_body_and_nothing_playing_is_none() {
        let (address, server) = serve_once(200, "not json");
        let client = test_client(format!("http://{address}"), reqwest::Client::new());
        client.play(Some("dev"), Some(&PlayRequest::context("spotify:album:x"))).await.unwrap();
        let received = String::from_utf8(server.join().unwrap()).unwrap();
        assert!(received.starts_with("PUT /me/player/play?device_id=dev HTTP/1.1"));
        assert!(received.ends_with(r#"{"context_uri":"spotify:album:x"}"#));

        let (address, server) = serve_once(204, "");
        let client = test_client(format!("http://{address}"), reqwest::Client::new());
        assert!(client.playback_state().await.unwrap().is_none());
        server.join().unwrap();
    }

    #[tokio::test]
    async fn quota_answer_stops_without_retrying() {
        let (address, server) = serve_once(429, r#"{"error":{"status":429,"reason":"QUOTA_EXCEEDED"}}"#);
        let client = test_client(format!("http://{address}"), reqwest::Client::new());
        assert!(matches!(client.me().await, Err(ApiError::QuotaExhausted)));
        server.join().unwrap();
    }

    #[test]
    fn play_request_body_shapes() {
        let context = PlayRequest::context("spotify:album:x").starting_at_uri("spotify:track:y");
        assert_eq!(context.body(), json!({ "context_uri": "spotify:album:x", "offset": { "uri": "spotify:track:y" } }));
        let tracks = PlayRequest::tracks(vec!["spotify:track:a".into()]).starting_at_index(0);
        assert_eq!(tracks.body(), json!({ "uris": ["spotify:track:a"], "offset": { "position": 0 } }));
        // одиночный трек/выпуск как контекст уходит единственным uri
        let mut song = PlayRequest::context("spotify:track:t").starting_at_uri("spotify:track:t");
        song.position_ms = 5_000;
        assert_eq!(song.body(), json!({ "uris": ["spotify:track:t"], "position_ms": 5_000 }));
        assert_eq!(PlayRequest::context("spotify:episode:e").body(), json!({ "uris": ["spotify:episode:e"] }));
        // возобновление, ушедшее дальше одиночного контекста, играет названный трек
        let mut resumed = PlayRequest::context("spotify:track:a").starting_at_uri("spotify:track:b");
        resumed.position_ms = 42_000;
        assert_eq!(resumed.body(), json!({ "uris": ["spotify:track:b"], "position_ms": 42_000 }));
    }

    #[test]
    fn quota_exhaustion_is_distinct_from_an_ordinary_rate_limit() {
        assert!(is_quota_exhausted(r#"{"error":{"status":429,"reason":"QUOTA_EXCEEDED"}}"#));
        assert!(!is_quota_exhausted(r#"{"error":{"status":429,"message":"Too many requests"}}"#));
    }

    /// Сервер, отвечающий по очереди заданными ответами; возвращает число принятых запросов.
    async fn serve_in_turn(answers: Vec<&'static str>) -> (std::net::SocketAddr, tokio::task::JoinHandle<usize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut served = 0;
            for answer in answers {
                let Ok(Ok((mut socket, _))) = tokio::time::timeout(Duration::from_secs(3), listener.accept()).await
                else {
                    break;
                };
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                }
                socket.write_all(answer.as_bytes()).await.unwrap();
                served += 1;
            }
            served
        });
        (address, server)
    }

    #[tokio::test]
    async fn a_read_honours_a_short_retry_after_but_not_a_long_one() {
        const LIMITED_1S: &str =
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        const LIMITED_HOUR: &str =
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"name\":\"M\"}";
        let http = reqwest::Client::builder().no_proxy().build().unwrap();

        // короткий Retry-After — переждать и повторить
        let (address, server) = serve_in_turn(vec![LIMITED_1S, OK]).await;
        let client = test_client(format!("http://{address}"), http.clone());
        let artist = client.artist("a").await.unwrap();
        assert_eq!(artist.name, "M");
        assert_eq!(server.await.unwrap(), 2);

        // Retry-After за пределами бюджета — сразу RateLimited, без повтора
        let (address, server) = serve_in_turn(vec![LIMITED_HOUR, OK]).await;
        let client = test_client(format!("http://{address}"), http.clone());
        let started = Instant::now();
        assert!(matches!(client.artist("a").await, Err(ApiError::RateLimited)));
        assert!(started.elapsed() < Duration::from_secs(2), "doesn't wait an hour or hang for 3×30 s");
        // охлаждение источника (≤30 с) дольше бюджета — следующие чтения тоже отвечают сразу
        let started = Instant::now();
        assert!(matches!(client.album("b").await, Err(ApiError::RateLimited)));
        assert!(started.elapsed() < Duration::from_secs(1));
        drop(client);
        assert_eq!(server.await.unwrap(), 1, "no requests go out during the cooldown");
    }

    #[tokio::test]
    async fn long_retry_after_on_queue_write_fails_fast_and_caps_cooldown() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(socket.read_u8().await.unwrap());
            }
            socket
                .write_all(
                    b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 3600\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let client = test_client(format!("http://{address}"), http);
        let result =
            tokio::time::timeout(Duration::from_secs(5), client.add_to_queue("spotify:track:a", None)).await.unwrap();
        assert!(matches!(result, Err(ApiError::RateLimited)));
        let until = *client.cooldown_until.lock().await;
        assert!(until <= Instant::now() + MAX_RETRY_AFTER);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn cooldown_state_is_owned_by_one_session() {
        let activity = Arc::new(NetActivity::default());
        let shared = ApiClient::new(reqwest::Client::new(), activity.clone(), 20, 50, ApiSource::Shared);
        let personal = ApiClient::new(reqwest::Client::new(), activity, 10, 10, ApiSource::Personal);
        shared.extend_cooldown(Duration::from_secs(10)).await;
        assert!(*shared.cooldown_until.lock().await > Instant::now());
        assert!(*personal.cooldown_until.lock().await <= Instant::now());
    }

    #[test]
    fn net_activity_reports_only_sustained_traffic() {
        let activity = NetActivity::default();
        assert!(!activity.busy(Duration::ZERO));
        activity.begin();
        assert!(activity.busy(Duration::ZERO));
        assert!(!activity.busy(Duration::from_secs(60)));
        {
            let _guard = ActivityGuard(&activity);
        }
        assert!(!activity.busy(Duration::ZERO));
    }
}
