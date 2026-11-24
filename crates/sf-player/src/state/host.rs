//! Хозяин локального движка: подключение, перезапуск с подхватом воспроизведения, восстановление
//! после обрыва сессии, остановка. Сообщает актору о состоянии через Msg::Local.
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::mpsc;

use super::actor::{LocalMsg, Msg};
use super::backend::{EngineLink, LocalEngine};
use crate::engine::{Engine, EngineConfig, EngineEvent, LoadSpec, Notify, PlaybackResume};
use crate::{Credentials, EngineStarted};

/// Сколько обрывов сессии подряд терпим за десять минут.
const RECONNECT_LIMIT: usize = 5;
const RECONNECT_WINDOW: Duration = Duration::from_secs(600);
/// На телефоне сессия обрывается, когда Wi-Fi сменился на мобильную сеть или экран спал; первое
/// переподключение тогда падает, потому что сети ещё нет, и без повторов музыка молчала до ручного включения.
const RECONNECT_RETRY_WAITS: [Duration; 6] = [
    Duration::from_secs(3),
    Duration::from_secs(8),
    Duration::from_secs(15),
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
];

/// Английский ключ перевода; наружу — `mf_i18n::t(PREMIUM_REQUIRED)` (в обоих языках есть «Premium»: по нему
/// sf-services узнаёт отказ).
pub(crate) const PREMIUM_REQUIRED: &str = "Spotify Premium required";

pub(crate) struct Slot {
    pub engine: Arc<EngineLink>,
    pub config: EngineConfig,
    pub credentials: Credentials,
}

pub(crate) struct EngineHost {
    slot: Mutex<Option<Slot>>,
    tx: mpsc::UnboundedSender<Msg>,
    reconnects: Mutex<Vec<Instant>>,
    /// поколение подключения: устаревший connect не занимает слот
    generation: Mutex<u64>,
}

impl EngineHost {
    pub fn new(tx: mpsc::UnboundedSender<Msg>) -> Self {
        Self { slot: Mutex::new(None), tx, reconnects: Mutex::new(Vec::new()), generation: Mutex::new(0) }
    }

    pub fn engine(&self) -> Option<Arc<EngineLink>> {
        self.slot.lock().ok()?.as_ref().map(|slot| slot.engine.clone())
    }

    fn take(&self) -> Option<Slot> {
        self.slot.lock().ok()?.take()
    }

    fn next_generation(&self) -> u64 {
        let mut generation = self.generation.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *generation += 1;
        *generation
    }

    fn current_generation(&self) -> u64 {
        *self.generation.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Подключить движок; предыдущий останавливается.
    pub async fn connect(
        self: &Arc<Self>,
        credentials: Credentials,
        config: EngineConfig,
        resume: Option<PlaybackResume>,
    ) -> Result<EngineStarted, String> {
        let generation = self.next_generation();
        if let Some(old) = self.take() {
            old.engine.engine.shutdown();
        }
        let _ = self.tx.send(Msg::Local(LocalMsg::Connecting));
        let notify: Notify = {
            let tx = self.tx.clone();
            Arc::new(move |event: EngineEvent| {
                let _ = tx.send(Msg::Engine(Box::new(event)));
            })
        };
        let result = async {
            let cache = config.open_cache().map_err(|error| format!("{error:#}"))?;
            Engine::connect(&config, config.proxy.librespot_url(), credentials.clone(), cache, notify)
                .await
                .map_err(|error| format!("{error:#}"))
        }
        .await;
        let engine = match result {
            Ok(engine) => engine,
            Err(error) => {
                // ошибки движка уже переведены (friendly_connect_error), а «Premium» есть в обоих языках
                let text = if error.to_lowercase().contains("premium") { mf_i18n::t(PREMIUM_REQUIRED) } else { error };
                tracing::warn!("local playback didn't start: {text}");
                if generation == self.current_generation() {
                    let _ = self.tx.send(Msg::Local(LocalMsg::Failed(text.clone())));
                }
                return Err(text);
            }
        };
        if generation != self.current_generation() {
            engine.shutdown();
            return Err(mf_i18n::t("Playback setup cancelled"));
        }
        let device_id = engine.device_id().to_owned();
        let reusable = engine.credentials();
        let link = Arc::new(EngineLink { engine, tap: config.tap.clone() });
        if let Some(resume) = resume {
            resume_playback(link.clone(), resume);
        }
        let ready = LocalMsg::Ready {
            engine: link.clone() as Arc<dyn LocalEngine>,
            device_id: device_id.clone(),
            name: config.device_name.clone(),
            bitrate: config.bitrate_kbps,
            autoplay: config.autoplay,
        };
        if let Ok(mut slot) = self.slot.lock() {
            *slot = Some(Slot {
                engine: link,
                config,
                credentials: reusable.clone().unwrap_or(credentials),
            });
        }
        let _ = self.tx.send(Msg::Local(ready));
        Ok(EngineStarted { device_id, reusable })
    }

    /// Перезапуск с новыми настройками (или прежними) и подхватом трека и позиции.
    /// `load` — что загрузить в новый движок вместо одного трека (контекст, список, режимы — из ядра);
    /// оборвавшаяся сессия librespot всё равно восстанавливается своим снимком.
    pub fn restart(self: &Arc<Self>, rt: &Handle, config: Option<EngineConfig>, volume: u16, load: Option<LoadSpec>) {
        self.restart_with_retries(rt, config, volume, load, &[]);
    }

    /// `retry_waits`: паузы между повторными попытками, если подключение не удалось (сети в этот момент нет).
    fn restart_with_retries(
        self: &Arc<Self>,
        rt: &Handle,
        config: Option<EngineConfig>,
        volume: u16,
        load: Option<LoadSpec>,
        retry_waits: &'static [Duration],
    ) {
        let Some(old) = self.take() else {
            return;
        };
        let resume = match old.engine.engine.resume_point() {
            Some(PlaybackResume::Session(snapshot)) => Some(PlaybackResume::Session(snapshot)),
            other => load.map(PlaybackResume::Track).or(other),
        };
        old.engine.engine.shutdown();
        let mut config = config.unwrap_or(old.config);
        // новый движок звучит на той же громкости, что и прежний
        config.initial_volume = volume;
        let host = self.clone();
        let credentials = old.credentials;
        rt.spawn(async move {
            let mut waits = retry_waits.iter();
            loop {
                let ours = host.current_generation() + 1;
                let Err(error) = host.connect(credentials.clone(), config.clone(), resume.clone()).await else {
                    return;
                };
                // отказ по подписке не лечится повтором; чужой connect/disconnect уже занял поколение
                let Some(wait) = waits.next().filter(|_| !error.to_lowercase().contains("premium")) else {
                    return;
                };
                if host.current_generation() != ours {
                    return;
                }
                tokio::time::sleep(*wait).await;
                if host.current_generation() != ours {
                    return;
                }
            }
        });
    }

    /// Сессия librespot оборвалась сама — переподключиться (не чаще RECONNECT_LIMIT за 10 минут).
    pub fn reconnect(self: &Arc<Self>, rt: &Handle, volume: u16) {
        let exhausted = {
            let mut reconnects = self.reconnects.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let now = Instant::now();
            reconnects.retain(|at| now.duration_since(*at) < RECONNECT_WINDOW);
            reconnects.push(now);
            reconnects.len() > RECONNECT_LIMIT
        };
        if exhausted {
            if let Some(old) = self.take() {
                old.engine.engine.shutdown();
            }
            let _ = self.tx.send(Msg::Local(LocalMsg::Failed(
                mf_i18n::t("Local playback keeps dropping. Turn it on again in Settings"),
            )));
            return;
        }
        tracing::info!("the librespot session ended; reconnecting");
        self.restart_with_retries(rt, None, volume, None, &RECONNECT_RETRY_WAITS);
    }

    pub fn disconnect(&self) {
        self.next_generation();
        if let Some(old) = self.take() {
            old.engine.engine.shutdown();
        }
        let _ = self.tx.send(Msg::Local(LocalMsg::Unavailable));
    }

    /// Выход: остановить без сообщений актору.
    pub fn shutdown(&self) {
        self.next_generation();
        if let Some(old) = self.take() {
            old.engine.engine.shutdown();
        }
    }
}

/// Подхватить воспроизведение на новой сессии; Spirc может отклонять загрузку, пока регистрируется.
fn resume_playback(link: Arc<EngineLink>, resume: PlaybackResume) {
    tokio::spawn(async move {
        for attempt in 0..3 {
            if let Err(error) = link.engine.resume(resume.clone()) {
                tracing::warn!("couldn't resume playback: {error:#}");
            }
            tokio::time::sleep(Duration::from_secs(4)).await;
            if link.engine.interrupted().is_some() {
                return;
            }
            tracing::info!("retrying playback resume (attempt {})", attempt + 2);
        }
    });
}
