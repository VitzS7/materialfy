//! Маршрутизация запросов между общим и личным приложением Spotify. Перенос spotifast/src/api/gateway.rs.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

use crate::ApiSource;
use crate::client::{ApiClient, ApiError, NetActivity, TokenProvider};
use crate::limits::{LimitKind, Limits, WebApiLimits};
use crate::models::Playlist;

/// Общий и личный Web API вошли в разные аккаунты (ключ перевода; sf-services сверяет по нему код ошибки).
pub const ACCOUNT_MISMATCH: &str = "The shared and personal Spotify apps are signed in to different accounts";

pub use md3_model::{AccountId, PlaylistId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionState {
    Unavailable,
    Authorizing,
    Ready { account: AccountId },
}

impl SessionState {
    pub fn account(&self) -> Option<&AccountId> {
        match self {
            Self::Ready { account } => Some(account),
            Self::Unavailable | Self::Authorizing => None,
        }
    }
}

#[derive(Clone, Copy)]
struct ApiProfile {
    source: ApiSource,
    search_limit: u32,
    artist_albums_limit: u32,
}

impl ApiProfile {
    pub const SHARED: Self = Self {
        source: ApiSource::Shared,
        search_limit: 20,
        artist_albums_limit: 50,
    };
    pub const PERSONAL: Self = Self {
        source: ApiSource::Personal,
        search_limit: 10,
        artist_albums_limit: 10,
    };
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PlaylistAccess {
    Owned,
    Collaborative,
    External,
    #[default]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    CanonicalAccount,
    Playback,
    UserData,
    PlaylistLibrary,
    PlaylistCreation,
    PlaylistSearch,
    CatalogSearch,
    Catalog,
    PlaylistMetadata(PlaylistAccess),
    PlaylistItems(PlaylistAccess),
    PlaylistMutation(PlaylistAccess),
    UnsupportedDevelopmentMode,
}

/// The streaming session reads every playlist the shared app would have
/// been asked for: other people's, which no personal app may read, and the
/// account's own when it has no personal app. A personal app keeps its own
/// playlists, which it reads quickly and with every field.
fn session_serves(operation: Operation, personal_ready: bool) -> bool {
    matches!(
        operation,
        Operation::PlaylistMetadata(_) | Operation::PlaylistItems(_)
    ) && plan(operation, personal_ready) == ApiSource::Shared
}

/// A playlist with unknown access dispatches to the shared app, which can
/// serve anything; later requests move once a response reveals the owner.
fn plan(operation: Operation, personal_ready: bool) -> ApiSource {
    use Operation::*;
    match operation {
        CanonicalAccount | PlaylistLibrary | PlaylistSearch | UnsupportedDevelopmentMode => {
            ApiSource::Shared
        }
        PlaylistMetadata(PlaylistAccess::External | PlaylistAccess::Unknown)
        | PlaylistItems(PlaylistAccess::External | PlaylistAccess::Unknown)
        | PlaylistMutation(PlaylistAccess::External | PlaylistAccess::Unknown) => ApiSource::Shared,
        PlaylistMetadata(_) | PlaylistItems(_) | PlaylistMutation(_) if personal_ready => {
            ApiSource::Personal
        }
        Playback | UserData | PlaylistCreation | Catalog | CatalogSearch if personal_ready => {
            ApiSource::Personal
        }
        Playback | UserData | PlaylistCreation | Catalog | CatalogSearch | PlaylistMetadata(_)
        | PlaylistItems(_) | PlaylistMutation(_) => ApiSource::Shared,
    }
}

fn classify_playlist(account: &AccountId, playlist: &Playlist) -> PlaylistAccess {
    if playlist.owner.id.as_deref() == Some(account.as_str()) {
        PlaylistAccess::Owned
    } else if playlist.collaborative {
        PlaylistAccess::Collaborative
    } else if playlist.owner.id.is_some() {
        PlaylistAccess::External
    } else {
        PlaylistAccess::Unknown
    }
}

struct Session {
    // The counter remembers a sign-out even if a new sign-in becomes ready
    // before a waiting request wakes up.
    state: tokio::sync::watch::Sender<(u64, SessionState)>,
    client: RwLock<Arc<ApiClient>>,
}

impl Session {
    fn new(
        http: crate::http::Http,
        activity: Arc<NetActivity>,
        profile: ApiProfile,
        limits: Arc<Limits>,
    ) -> Self {
        Self {
            state: tokio::sync::watch::channel((0, SessionState::Unavailable)).0,
            client: RwLock::new(Arc::new(
                ApiClient::new(
                    http,
                    activity,
                    profile.search_limit,
                    profile.artist_albums_limit,
                    profile.source,
                )
                .with_limits(limits),
            )),
        }
    }

    fn state(&self) -> SessionState {
        self.state.borrow().1.clone()
    }

    fn set_state(&self, state: SessionState) {
        self.state.send_modify(|current| current.1 = state);
    }

    fn client(&self) -> Arc<ApiClient> {
        Arc::clone(&self.client.read().unwrap_or_else(|lock| lock.into_inner()))
    }
}

pub struct ApiGateway {
    http: crate::http::Http,
    shared: Session,
    personal: Session,
    /// Аккаунт сессии librespot (вход воспроизведения): известен и без токена Web API.
    session_account: RwLock<Option<AccountId>>,
    playlist_access: Mutex<HashMap<PlaylistId, PlaylistAccess>>,
    /// Бюджет запросов, склейка и кэш GET, охлаждение после 429/квоты (см. limits.rs).
    limits: Arc<Limits>,
}

impl ApiGateway {
    pub fn new(http: impl Into<crate::http::Http>, activity: Arc<NetActivity>) -> Self {
        let http = http.into();
        let limits = Limits::new();
        Self {
            http: http.clone(),
            shared: Session::new(http.clone(), activity.clone(), ApiProfile::SHARED, limits.clone()),
            personal: Session::new(http, activity, ApiProfile::PERSONAL, limits.clone()),
            limits,
            session_account: RwLock::new(None),
            playlist_access: Mutex::new(HashMap::new()),
        }
    }

    /// Общий HTTP-клиент (с прокси) — для запросов мимо Web API, например картинок.
    pub fn http(&self) -> crate::http::Http {
        self.http.clone()
    }

    /// «Web API ограничен»: охлаждение общего/личного приложения после 429 или QUOTA_EXCEEDED.
    pub fn web_api_limits(&self) -> tokio::sync::watch::Receiver<WebApiLimits> {
        self.limits.watch()
    }

    /// Охлаждается ли источник, которому досталась бы операция: фоновой работе лучше не начинать.
    pub fn web_api_blocked(&self, operation: Operation) -> Option<LimitKind> {
        self.limits.blocked(plan(operation, self.personal_ready()))
    }

    /// Забыть кэш ответов GET (после записи мимо Web API, например через сессию librespot); каталог остаётся.
    pub fn forget_cached(&self) {
        self.limits.forget_user_data();
    }

    fn session(&self, source: ApiSource) -> &Session {
        match source {
            ApiSource::Shared => &self.shared,
            ApiSource::Personal => &self.personal,
        }
    }

    pub fn state(&self, source: ApiSource) -> SessionState {
        self.session(source).state()
    }

    pub fn set_state(&self, source: ApiSource, state: SessionState) {
        self.session(source).set_state(state);
    }

    /// Marks a verified session ready, keeping the token provider that
    /// `begin_verification` installed.
    pub fn install(&self, source: ApiSource, account: AccountId) -> Result<(), ApiError> {
        let other = match source {
            ApiSource::Shared => ApiSource::Personal,
            ApiSource::Personal => ApiSource::Shared,
        };
        if self
            .state(other)
            .account()
            .is_some_and(|active| active != &account)
            || self.session_account().is_some_and(|active| active != account)
        {
            return Err(ApiError::Status {
                status: 403,
                message: mf_i18n::t(ACCOUNT_MISMATCH),
            });
        }
        self.session(source)
            .set_state(SessionState::Ready { account });
        Ok(())
    }

    pub fn begin_verification(&self, source: ApiSource, provider: TokenProvider) {
        let session = self.session(source);
        let client = session.client().for_authorization(provider);
        *session
            .client
            .write()
            .unwrap_or_else(|lock| lock.into_inner()) = Arc::new(client);
        session.set_state(SessionState::Authorizing);
    }

    pub fn verification_client(&self, source: ApiSource) -> Arc<ApiClient> {
        self.session(source).client()
    }

    pub fn clear(&self, source: ApiSource) {
        let session = self.session(source);
        session.client().set_token_provider(None);
        session.state.send_modify(|current| {
            current.0 += 1;
            current.1 = SessionState::Unavailable;
        });
    }

    pub fn clear_all(&self) {
        self.clear(ApiSource::Shared);
        self.clear(ApiSource::Personal);
        self.set_session_account(None);
        self.limits.forget();
        self.playlist_access
            .lock()
            .unwrap_or_else(|lock| lock.into_inner())
            .clear();
    }

    pub fn account(&self) -> Option<AccountId> {
        self.web_account().or_else(|| self.session_account())
    }

    /// Аккаунт, в который вошёл Web API (общее или личное приложение).
    pub fn web_account(&self) -> Option<AccountId> {
        self.state(ApiSource::Shared)
            .account()
            .cloned()
            .or_else(|| self.state(ApiSource::Personal).account().cloned())
    }

    /// Аккаунт сессии librespot (None — входа воспроизведения нет).
    pub fn session_account(&self) -> Option<AccountId> {
        self.session_account
            .read()
            .unwrap_or_else(|lock| lock.into_inner())
            .clone()
    }

    /// Вход воспроизведения задаёт аккаунт приложения и без Web API; Web API другого аккаунта
    /// после этого не устанавливается (`install`).
    pub fn set_session_account(&self, account: Option<AccountId>) {
        *self
            .session_account
            .write()
            .unwrap_or_else(|lock| lock.into_inner()) = account;
    }

    pub fn personal_ready(&self) -> bool {
        matches!(self.state(ApiSource::Personal), SessionState::Ready { .. })
    }

    pub async fn client_for(&self, operation: Operation) -> Result<Arc<ApiClient>, ApiError> {
        let source = plan(operation, self.personal_ready());
        let session = self.session(source);
        let mut state = session.state.subscribe();
        let generation = state.borrow().0;
        loop {
            let (current, status) = state.borrow_and_update().clone();
            if current != generation {
                return Err(ApiError::NotSignedIn);
            }
            match status {
                SessionState::Ready { .. } => break,
                SessionState::Unavailable => return Err(ApiError::NotSignedIn),
                // A personal grant can open the app before the shared grant
                // finishes verification. Keep shared-only views loading.
                SessionState::Authorizing => {
                    state.changed().await.map_err(|_| ApiError::NotSignedIn)?;
                }
            }
        }
        tracing::debug!("Spotify route operation={operation:?} source={source}");
        Ok(session.client())
    }

    /// Whether the streaming session, when up, should answer an operation.
    pub fn session_serves(&self, operation: Operation) -> bool {
        session_serves(operation, self.personal_ready())
    }

    pub fn playlist_access(&self, id: &str) -> PlaylistAccess {
        self.playlist_access
            .lock()
            .unwrap_or_else(|lock| lock.into_inner())
            .get(id)
            .copied()
            .unwrap_or_default()
    }

    pub fn observe_playlist(&self, playlist: &Playlist) {
        let Some(account) = self.account() else {
            return;
        };
        let access = classify_playlist(&account, playlist);
        self.playlist_access
            .lock()
            .unwrap_or_else(|lock| lock.into_inner())
            .insert(PlaylistId::new(playlist.id.clone()), access);
    }

    pub fn observe_playlists<'a>(&self, playlists: impl IntoIterator<Item = &'a Playlist>) {
        for playlist in playlists {
            self.observe_playlist(playlist);
        }
    }

    pub fn invalidate_playlist_access(&self, id: &PlaylistId) {
        self.playlist_access
            .lock()
            .unwrap_or_else(|lock| lock.into_inner())
            .insert(id.clone(), PlaylistAccess::Unknown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    #[tokio::test]
    async fn shared_requests_wait_while_a_personal_session_is_already_ready() {
        let gateway = ApiGateway::new(reqwest::Client::new(), Arc::new(NetActivity::default()));
        gateway.set_state(ApiSource::Shared, SessionState::Authorizing);
        gateway.begin_verification(
            ApiSource::Personal,
            provider("ready-personal", ApiSource::Personal),
        );
        gateway
            .install(ApiSource::Personal, AccountId::new("same"))
            .unwrap();
        let mut waiting = Box::pin(gateway.client_for(Operation::PlaylistLibrary));
        std::future::poll_fn(|cx| {
            assert!(waiting.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        gateway.begin_verification(
            ApiSource::Shared,
            provider("slow-shared", ApiSource::Shared),
        );
        gateway
            .install(ApiSource::Shared, AccountId::new("same"))
            .unwrap();
        let client = waiting.await.unwrap();
        assert!(Arc::ptr_eq(
            &client,
            &gateway.verification_client(ApiSource::Shared)
        ));
    }

    #[tokio::test]
    async fn sign_out_cancels_waiting_requests_even_if_sign_in_finishes_before_they_wake() {
        let gateway = ApiGateway::new(reqwest::Client::new(), Arc::new(NetActivity::default()));
        gateway.begin_verification(ApiSource::Shared, provider("old-shared", ApiSource::Shared));
        let old_client = gateway.verification_client(ApiSource::Shared);
        let mut waiting = Box::pin(gateway.client_for(Operation::PlaylistLibrary));
        std::future::poll_fn(|cx| {
            assert!(waiting.as_mut().poll(cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        gateway.clear_all();
        gateway.begin_verification(ApiSource::Shared, provider("new-shared", ApiSource::Shared));
        gateway
            .install(ApiSource::Shared, AccountId::new("new-account"))
            .unwrap();
        assert!(matches!(waiting.await, Err(ApiError::NotSignedIn)));
        assert!(matches!(old_client.me().await, Err(ApiError::NotSignedIn)));
        let new_client = gateway
            .client_for(Operation::PlaylistLibrary)
            .await
            .unwrap();
        assert!(!Arc::ptr_eq(&old_client, &new_client));
    }

    fn provider(name: &str, _source: ApiSource) -> TokenProvider {
        TokenProvider::Fixed(format!("{name}-test-token"))
    }

    #[test]
    fn the_session_reads_what_the_shared_app_would_have() {
        for (operation, personal) in [
            (Operation::PlaylistItems(PlaylistAccess::External), true),
            (Operation::PlaylistMetadata(PlaylistAccess::Unknown), false),
            (Operation::PlaylistMetadata(PlaylistAccess::Owned), false),
            (
                Operation::PlaylistItems(PlaylistAccess::Collaborative),
                false,
            ),
        ] {
            assert!(session_serves(operation, personal), "{operation:?}");
        }
        for (operation, personal) in [
            (Operation::PlaylistItems(PlaylistAccess::Owned), true),
            (
                Operation::PlaylistItems(PlaylistAccess::Collaborative),
                true,
            ),
            (Operation::PlaylistMutation(PlaylistAccess::External), true),
            (Operation::Catalog, false),
            (Operation::PlaylistLibrary, false),
        ] {
            assert!(!session_serves(operation, personal), "{operation:?}");
        }
    }

    #[test]
    fn routing_matrix_selects_source() {
        let personal = true;
        for operation in [
            Operation::Playback,
            Operation::UserData,
            Operation::PlaylistCreation,
            Operation::Catalog,
            Operation::CatalogSearch,
            Operation::PlaylistMetadata(PlaylistAccess::Owned),
            Operation::PlaylistMetadata(PlaylistAccess::Collaborative),
            Operation::PlaylistItems(PlaylistAccess::Owned),
            Operation::PlaylistItems(PlaylistAccess::Collaborative),
        ] {
            assert_eq!(plan(operation, personal), ApiSource::Personal);
        }
        for operation in [
            Operation::CanonicalAccount,
            Operation::PlaylistLibrary,
            Operation::PlaylistSearch,
            Operation::UnsupportedDevelopmentMode,
            Operation::PlaylistMetadata(PlaylistAccess::External),
            Operation::PlaylistMetadata(PlaylistAccess::Unknown),
            Operation::PlaylistItems(PlaylistAccess::External),
            Operation::PlaylistItems(PlaylistAccess::Unknown),
            Operation::PlaylistMutation(PlaylistAccess::External),
            Operation::PlaylistMutation(PlaylistAccess::Unknown),
        ] {
            assert_eq!(plan(operation, personal), ApiSource::Shared);
        }
        for operation in [
            Operation::Playback,
            Operation::UserData,
            Operation::PlaylistLibrary,
            Operation::PlaylistCreation,
            Operation::PlaylistSearch,
            Operation::Catalog,
            Operation::CatalogSearch,
            Operation::PlaylistMetadata(PlaylistAccess::Unknown),
            Operation::PlaylistItems(PlaylistAccess::Unknown),
        ] {
            assert_eq!(plan(operation, false), ApiSource::Shared);
        }
    }

    #[test]
    fn playlist_access_keeps_unknown_explicit() {
        let account = AccountId::new("me");
        let mut playlist = Playlist {
            id: "p".into(),
            ..Playlist::default()
        };
        assert_eq!(
            classify_playlist(&account, &playlist),
            PlaylistAccess::Unknown
        );
        playlist.owner.id = Some("other".into());
        assert_eq!(
            classify_playlist(&account, &playlist),
            PlaylistAccess::External
        );
        playlist.collaborative = true;
        assert_eq!(
            classify_playlist(&account, &playlist),
            PlaylistAccess::Collaborative
        );
        playlist.owner.id = Some("me".into());
        assert_eq!(
            classify_playlist(&account, &playlist),
            PlaylistAccess::Owned
        );
    }

    #[test]
    fn dual_sessions_require_the_same_account() {
        let gateway = ApiGateway::new(reqwest::Client::new(), Arc::new(NetActivity::default()));
        gateway.begin_verification(ApiSource::Shared, provider("shared", ApiSource::Shared));
        gateway
            .install(ApiSource::Shared, AccountId::new("same"))
            .unwrap();
        gateway.begin_verification(
            ApiSource::Personal,
            provider("personal", ApiSource::Personal),
        );
        gateway
            .install(ApiSource::Personal, AccountId::new("same"))
            .unwrap();
        assert!(gateway.personal_ready());
        gateway.clear(ApiSource::Personal);
        gateway.begin_verification(ApiSource::Personal, provider("other", ApiSource::Personal));
        assert!(
            gateway
                .install(ApiSource::Personal, AccountId::new("other"))
                .is_err()
        );
        assert!(matches!(
            gateway.state(ApiSource::Shared),
            SessionState::Ready { .. }
        ));

        gateway.begin_verification(
            ApiSource::Personal,
            provider("personal-again", ApiSource::Personal),
        );
        gateway
            .install(ApiSource::Personal, AccountId::new("same"))
            .unwrap();
        gateway.clear(ApiSource::Shared);
        gateway.begin_verification(
            ApiSource::Shared,
            provider("other-shared", ApiSource::Shared),
        );
        assert!(
            gateway
                .install(ApiSource::Shared, AccountId::new("other"))
                .is_err()
        );
        assert!(matches!(
            gateway.state(ApiSource::Personal),
            SessionState::Ready { .. }
        ));
    }

    #[test]
    fn a_session_sign_in_names_the_account_without_the_web_api() {
        let gateway = ApiGateway::new(reqwest::Client::new(), Arc::new(NetActivity::default()));
        assert_eq!(gateway.account(), None);
        gateway.set_session_account(Some(AccountId::new("listener")));
        assert_eq!(gateway.account(), Some(AccountId::new("listener")));
        assert_eq!(gateway.web_account(), None);
        // Web API другого аккаунта не встаёт рядом с сессией
        gateway.begin_verification(ApiSource::Shared, provider("stranger", ApiSource::Shared));
        assert!(gateway.install(ApiSource::Shared, AccountId::new("stranger")).is_err());
        gateway.begin_verification(ApiSource::Shared, provider("same", ApiSource::Shared));
        gateway.install(ApiSource::Shared, AccountId::new("listener")).unwrap();
        assert_eq!(gateway.web_account(), Some(AccountId::new("listener")));
        gateway.clear_all();
        assert_eq!(gateway.account(), None);
    }
}
