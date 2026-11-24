//! Единственный экземпляр (app.py: raise_running_instance).
//! Linux: если на сессионной шине уже есть mpris::BUS_NAME — вызвать у него org.mpris.MediaPlayer2.Raise
//! и вернуть true (вызывающий выходит с кодом 0). Работающий экземпляр получает просьбу как MprisCommand::Raise.
//!
//! Windows и macOS (дополнение к Python-версии, где там всегда false): работающий экземпляр держит
//! [`InstanceListener`] — именованный канал `\\.\pipe\materialfy-Materialfy-<user>` (создаётся как первый экземпляр
//! канала, т. е. работает и как мьютекс) или unix-сокет `$TMPDIR/materialfy-Materialfy.sock`. Новый процесс подключается,
//! пишет "raise\n" и выходит; без слушателя (оболочка его не запустила) — false, как раньше.
//! Протокол: клиент — строка "raise\n", сервер — ответ "ok\n" или "hung\n": окно не отвечает (GUI-поток не
//! отметился в [`gui_alive`] за 20 с: подвисание на тяжёлой странице слабого ПК — ещё не зависание). Зависший экземпляр новый процесс завершает (Windows) и запускается сам —
//! иначе после зависания каждый запуск тихо выходил, «показав» мёртвое окно.
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::PlatformError;

#[cfg(any(target_os = "windows", target_os = "macos"))]
const REQUEST: &[u8] = b"raise\n";
#[cfg(any(target_os = "windows", target_os = "macos"))]
const REPLY: &[u8] = b"ok\n";
#[cfg(any(target_os = "windows", target_os = "macos"))]
const HUNG: &[u8] = b"hung\n";

/// Последняя отметка GUI-потока и начало работы слушателя, мс от эпохи (0 — ещё не было).
static GUI_BEAT_MS: AtomicU64 = AtomicU64::new(0);
static LISTEN_MS: AtomicU64 = AtomicU64::new(0);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// GUI-поток жив — звать из его таймера раз в секунду.
pub fn gui_alive() {
    GUI_BEAT_MS.store(now_ms(), Ordering::Relaxed);
}

/// Окно отвечает: отметка свежая; до первой отметки (окно ещё грузится) — первые 60 с работы.
#[cfg_attr(not(any(target_os = "windows", target_os = "macos")), allow(dead_code))]
fn gui_responsive() -> bool {
    let now = now_ms();
    match GUI_BEAT_MS.load(Ordering::Relaxed) {
        0 => now.saturating_sub(LISTEN_MS.load(Ordering::Relaxed)) < 60_000,
        beat => now.saturating_sub(beat) < 20_000,
    }
}
const TIMEOUT: Duration = Duration::from_secs(3);

/// true — работающий экземпляр найден и попрошен показать окно.
pub async fn raise_running_instance() -> bool {
    match tokio::time::timeout(TIMEOUT, imp::raise()).await {
        Ok(found) => found,
        Err(_) => {
            tracing::debug!("the running instance did not answer in time");
            false
        }
    }
}

/// Сколько новый процесс ждёт, пока уже запущенный (может, ещё стартующий) экземпляр откликнется.
#[cfg(target_os = "windows")]
const PRIMARY_WAIT: Duration = Duration::from_secs(20);

/// Стать единственным экземпляром. true — этот процесс первый, работать дальше; false — уже работает другой
/// (его окно показано) — выйти. Windows: именованный мьютекс занимается атомарно с самого начала запуска,
/// поэтому два одновременных запуска (двойной клик, автозапуск и ярлык) не стартуют оба, даже пока первый ещё
/// не открыл канал просьб. Зависший экземпляр raise завершает — тогда место занимает этот процесс.
pub async fn become_primary() -> bool {
    #[cfg(target_os = "windows")]
    {
        let deadline = std::time::Instant::now() + PRIMARY_WAIT;
        loop {
            if imp::claim() {
                // мьютекс свободен, но канал может держать версия до мьютекса — её окно и показать
                return !raise_running_instance().await;
            }
            if raise_running_instance().await {
                return false;
            }
            if std::time::Instant::now() >= deadline {
                tracing::warn!("another Materialfy is starting but doesn't answer; not starting a second one");
                return false;
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        !raise_running_instance().await
    }
}

/// Слушатель просьб «показать окно» от новых процессов (Windows/macOS).
/// На Linux не нужен: `start` возвращает Err(Unsupported), просьба приходит через MPRIS.
pub struct InstanceListener {
    requests: mpsc::UnboundedReceiver<()>,
    task: tokio::task::JoinHandle<()>,
}

impl InstanceListener {
    /// Занять канал/сокет. Ошибка — другой экземпляр уже слушает (или ОС не поддерживается).
    /// Вызывать внутри tokio-рантайма.
    pub async fn start() -> Result<InstanceListener, PlatformError> {
        let (tx, requests) = mpsc::unbounded_channel();
        LISTEN_MS.store(now_ms(), Ordering::Relaxed);
        let task = imp::listen(tx).await?;
        Ok(InstanceListener { requests, task })
    }

    /// Ждать следующей просьбы показать окно; None — слушатель остановлен.
    pub async fn recv(&mut self) -> Option<()> {
        self.requests.recv().await
    }

    /// Отдать канал просьб (слушатель продолжает работать, пока жив рантайм).
    pub fn into_receiver(self) -> mpsc::UnboundedReceiver<()> {
        self.requests
    }

    pub fn close(self) {
        self.task.abort();
    }
}

/// Обработать одно подключение: прочитать просьбу, ответить.
#[cfg(any(target_os = "windows", target_os = "macos"))]
async fn serve_client<S>(mut stream: S, tx: mpsc::UnboundedSender<()>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = [0u8; 16];
    let mut len = 0;
    let read = tokio::time::timeout(TIMEOUT, async {
        while len < buf.len() {
            match stream.read(&mut buf[len..]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => len += n,
            }
            if buf[..len].contains(&b'\n') {
                break;
            }
        }
    });
    let _ = read.await;
    if buf[..len] == *REQUEST {
        let reply = if gui_responsive() {
            let _ = tx.send(());
            REPLY
        } else {
            tracing::warn!("asked to show the window, but the GUI thread does not respond");
            HUNG
        };
        let _ = stream.write_all(reply).await;
        let _ = stream.flush().await;
    }
}

/// Ответ работающего экземпляра.
#[cfg(any(target_os = "windows", target_os = "macos"))]
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    Shown,
    Hung,
}

/// Клиент: отправить просьбу и дождаться ответа. None — запрос не ушёл.
#[cfg(any(target_os = "windows", target_os = "macos"))]
async fn send_request<S>(mut stream: S) -> Option<Answer>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    if stream.write_all(REQUEST).await.is_err() || stream.flush().await.is_err() {
        return None;
    }
    let mut reply = [0u8; 8];
    match stream.read(&mut reply).await {
        Ok(n) if reply[..n] == *HUNG => Some(Answer::Hung),
        // запрос ушёл, даже если ответ не дошёл — второй экземпляр всё равно не нужен
        _ => Some(Answer::Shown),
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    use crate::PlatformError;
    use crate::mpris::{BUS_NAME, OBJECT_PATH};

    pub async fn raise() -> bool {
        match try_raise().await {
            Ok(found) => found,
            Err(err) => {
                tracing::debug!("D-Bus unavailable: {err}");
                false
            }
        }
    }

    async fn try_raise() -> zbus::Result<bool> {
        let conn = zbus::Connection::session().await?;
        let dbus = zbus::fdo::DBusProxy::new(&conn).await?;
        let name = zbus::names::BusName::try_from(BUS_NAME)?;
        if !dbus.name_has_owner(name).await? {
            return Ok(false);
        }
        let reply = conn
            .call_method(Some(BUS_NAME), OBJECT_PATH, Some("org.mpris.MediaPlayer2"), "Raise", &())
            .await;
        Ok(reply.is_ok())
    }

    pub async fn listen(_tx: mpsc::UnboundedSender<()>) -> Result<JoinHandle<()>, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use std::os::windows::io::AsRawHandle;
    use std::time::Duration;

    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    use crate::PlatformError;
    use crate::dirs::{APPLICATION, ORGANIZATION};

    const ERROR_PIPE_BUSY: i32 = 231;

    pub fn pipe_name() -> String {
        let user: String = std::env::var("USERNAME")
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
            .collect();
        format!(r"\\.\pipe\{ORGANIZATION}-{APPLICATION}-{user}")
    }

    pub async fn raise() -> bool {
        let name = pipe_name();
        let mut attempts = 0;
        let client = loop {
            match ClientOptions::new().open(&name) {
                Ok(client) => break client,
                Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 20 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                Err(_) => return false,
            }
        };
        let pid = allow_foreground(&client);
        match super::send_request(client).await {
            Some(super::Answer::Shown) => true,
            Some(super::Answer::Hung) => {
                // окно зависло: завершить тот процесс и запуститься вместо него
                if let Some(pid) = pid {
                    terminate(pid).await;
                }
                false
            }
            None => false,
        }
    }

    /// Разрешить работающему экземпляру вывести окно на передний план (иначе Windows только мигнёт кнопкой).
    /// Возвращает его PID.
    fn allow_foreground(client: &impl AsRawHandle) -> Option<u32> {
        use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
        use windows_sys::Win32::UI::WindowsAndMessaging::AllowSetForegroundWindow;
        let mut pid = 0u32;
        // SAFETY: дескриптор открытого канала живёт дольше вызова, pid — валидный указатель
        unsafe {
            if GetNamedPipeServerProcessId(client.as_raw_handle(), &mut pid) != 0 && pid != 0 {
                AllowSetForegroundWindow(pid);
                return Some(pid);
            }
        }
        None
    }

    /// Завершить зависший экземпляр и дождаться, пока он освободит канал.
    async fn terminate(pid: u32) {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess};
        tracing::warn!("the running instance (pid {pid}) does not respond: ending it");
        // SAFETY: дескриптор процесса открыт здесь же и закрыт после вызова
        unsafe {
            let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if !process.is_null() {
                TerminateProcess(process, 1);
                CloseHandle(process);
            }
        }
        for _ in 0..40 {
            if ClientOptions::new().open(pipe_name()).is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Занять мьютекс экземпляра на всё время жизни процесса; false — его держит другой процесс.
    pub fn claim() -> bool {
        use std::sync::OnceLock;
        use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError};
        use windows_sys::Win32::System::Threading::CreateMutexW;
        static HELD: OnceLock<usize> = OnceLock::new();
        if HELD.get().is_some() {
            return true;
        }
        let name: Vec<u16> = format!(r"Local\{ORGANIZATION}-{APPLICATION}-instance").encode_utf16().chain([0]).collect();
        // SAFETY: имя — строка с нулём на конце; дескриптор либо закрывается здесь же, либо живёт до конца процесса
        unsafe {
            let handle = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
            if handle.is_null() {
                // мьютекс не создать (права, странная среда) — не мешаем запуску, остаётся канал
                return true;
            }
            if GetLastError() == ERROR_ALREADY_EXISTS {
                CloseHandle(handle);
                return false;
            }
            let _ = HELD.set(handle as usize);
        }
        true
    }

    fn create(name: &str, first: bool) -> std::io::Result<NamedPipeServer> {
        ServerOptions::new().first_pipe_instance(first).create(name)
    }

    pub async fn listen(tx: mpsc::UnboundedSender<()>) -> Result<JoinHandle<()>, PlatformError> {
        let name = pipe_name();
        let mut server = create(&name, true)?;
        Ok(tokio::spawn(async move {
            loop {
                if let Err(err) = server.connect().await {
                    tracing::debug!("single-instance channel: {err}");
                }
                let next = match create(&name, false) {
                    Ok(next) => next,
                    Err(err) => {
                        tracing::warn!("single-instance channel closed: {err}");
                        return;
                    }
                };
                let client = std::mem::replace(&mut server, next);
                tokio::spawn(super::serve_client(client, tx.clone()));
            }
        }))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::PathBuf;

    use tokio::net::{UnixListener, UnixStream};
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    use crate::PlatformError;
    use crate::dirs::{APPLICATION, ORGANIZATION};

    pub fn socket_path() -> PathBuf {
        // $TMPDIR на macOS — свой у каждого пользователя
        std::env::temp_dir().join(format!("{ORGANIZATION}-{APPLICATION}.sock"))
    }

    pub async fn raise() -> bool {
        match UnixStream::connect(socket_path()).await {
            // зависший экземпляр на macOS не завершаем — просто запускаемся
            Ok(stream) => super::send_request(stream).await == Some(super::Answer::Shown),
            Err(_) => false,
        }
    }

    pub async fn listen(tx: mpsc::UnboundedSender<()>) -> Result<JoinHandle<()>, PlatformError> {
        let path = socket_path();
        if UnixStream::connect(&path).await.is_ok() {
            return Err(std::io::Error::from(std::io::ErrorKind::AddrInUse).into());
        }
        // сокет от упавшего процесса
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;
        Ok(tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(super::serve_client(stream, tx.clone()));
                    }
                    Err(err) => {
                        tracing::warn!("single-instance socket closed: {err}");
                        return;
                    }
                }
            }
        }))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
mod imp {
    use tokio::sync::mpsc;
    use tokio::task::JoinHandle;

    use crate::PlatformError;

    pub async fn raise() -> bool {
        false
    }

    pub async fn listen(_tx: mpsc::UnboundedSender<()>) -> Result<JoinHandle<()>, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    /// Отметка окна — общая на процесс: тесты, которые её трогают, идут по очереди
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    static GUI_STATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[tokio::test]
    async fn protocol_roundtrip() {
        let _serial = GUI_STATE.lock().await;
        super::gui_alive();
        let (client, server) = tokio::io::duplex(64);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let served = tokio::spawn(super::serve_client(server, tx));
        assert_eq!(super::send_request(client).await, Some(super::Answer::Shown));
        served.await.unwrap();
        assert_eq!(rx.recv().await, Some(()));
    }

    /// Окно давно не отмечалось — ответ «завис», просьба показать окно не уходит.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[tokio::test]
    async fn a_hung_window_answers_hung() {
        use std::sync::atomic::Ordering;
        let _serial = GUI_STATE.lock().await;
        super::GUI_BEAT_MS.store(super::now_ms() - 30_000, Ordering::Relaxed);
        let (client, server) = tokio::io::duplex(64);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let served = tokio::spawn(super::serve_client(server, tx));
        assert_eq!(super::send_request(client).await, Some(super::Answer::Hung));
        served.await.unwrap();
        assert!(rx.try_recv().is_err());
        super::gui_alive();
        assert!(super::gui_responsive());
    }

    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[tokio::test]
    async fn garbage_is_ignored() {
        use tokio::io::AsyncWriteExt;
        let (mut client, server) = tokio::io::duplex(64);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let served = tokio::spawn(super::serve_client(server, tx));
        client.write_all(b"hello\n").await.unwrap();
        served.await.unwrap();
        assert!(rx.try_recv().is_err());
    }

    /// Настоящий канал/сокет: занимает имя пользователя, поэтому не по умолчанию.
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    #[tokio::test]
    #[ignore = "takes the single-instance name"]
    async fn listener_receives_raise() {
        let mut listener = super::InstanceListener::start().await.unwrap();
        assert!(super::InstanceListener::start().await.is_err());
        assert!(super::raise_running_instance().await);
        assert_eq!(listener.recv().await, Some(()));
        listener.close();
    }
}
