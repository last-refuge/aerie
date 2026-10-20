//! `aerie-service`: the per-user background process that owns the Drive engine, the
//! metadata database and the sync folder. The app connects over a Unix socket in the
//! user's runtime directory; sync keeps running after the app quits. Only one service runs
//! per user, it accepts connections only from the same user, and it exits when no app is
//! connected and there's nothing to sync or transfer.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aerie_engine::{Command, Event, Hello, socket_path};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

/// How long the service waits, with no app connected and no work, before exiting.
const IDLE_EXIT: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let _logs = logging();
    let path = socket_path();
    match serve(&path).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            tracing::error!(%err, "the service stopped");
            std::process::ExitCode::FAILURE
        }
    }
}

/// The latest state of each kind, replayed to apps that connect later.
#[derive(Default)]
struct Snapshot {
    session: Option<String>,
    connectivity: Option<String>,
    sync: Option<String>,
}

impl Snapshot {
    fn record(&mut self, event: &Event, line: &str) {
        let slot = match event {
            Event::SignedIn(_) | Event::NeedsSignIn { .. } | Event::SignedOut => &mut self.session,
            Event::UpToDate | Event::Offline | Event::Indexing { .. } => &mut self.connectivity,
            Event::Sync(_) => &mut self.sync,
            _ => return,
        };
        *slot = Some(line.to_owned());
    }

    fn lines(&self) -> Vec<String> {
        [&self.session, &self.connectivity, &self.sync]
            .into_iter()
            .flatten()
            .cloned()
            .collect()
    }
}

async fn serve(path: &Path) -> std::io::Result<()> {
    let dir = path.parent().expect("the socket has a folder");
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    // One service per user: if another answers, leave it be.
    if UnixStream::connect(path).await.is_ok() {
        tracing::info!("another service is already running");
        return Ok(());
    }
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let owner = std::fs::metadata(dir)?.uid();
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "service started");

    let (events, engine_events) = async_channel::unbounded::<Event>();
    let (commands, inbox) = async_channel::unbounded::<Command>();
    let (busy_tx, busy) = tokio::sync::watch::channel(false);
    let mut engine = tokio::spawn(aerie_engine::run(events, inbox, commands.clone(), busy_tx));

    let (broadcast, _) = broadcast::channel::<String>(4096);
    let snapshot = Arc::new(Mutex::new(Snapshot::default()));
    let clients = Arc::new(AtomicUsize::new(0));
    {
        let broadcast = broadcast.clone();
        let snapshot = snapshot.clone();
        let clients = clients.clone();
        tokio::spawn(async move {
            while let Ok(event) = engine_events.recv().await {
                let Ok(line) = serde_json::to_string(&event) else {
                    continue;
                };
                snapshot.lock().expect("not poisoned").record(&event, &line);
                if clients.load(Ordering::SeqCst) == 0 {
                    notify_without_app(&event);
                }
                let _ = broadcast.send(line);
            }
        });
    }

    let mut idle_since = Some(tokio::time::Instant::now());
    loop {
        let idle_check = tokio::time::sleep(Duration::from_secs(5));
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                // Same-user connections only.
                if stream.peer_cred().map(|cred| cred.uid()).ok() != Some(owner) {
                    tracing::warn!("refused a connection from another user");
                    continue;
                }
                clients.fetch_add(1, Ordering::SeqCst);
                idle_since = None;
                let (commands, broadcast, snapshot, clients) =
                    (commands.clone(), broadcast.clone(), snapshot.clone(), clients.clone());
                tokio::spawn(async move {
                    if let Err(err) = client(stream, commands, broadcast, snapshot).await {
                        tracing::debug!(%err, "app disconnected");
                    }
                    clients.fetch_sub(1, Ordering::SeqCst);
                });
            }
            _ = &mut engine => {
                tracing::info!("engine stopped; exiting");
                break;
            }
            () = idle_check => {
                let idle = clients.load(Ordering::SeqCst) == 0 && !*busy.borrow();
                match (idle, idle_since) {
                    (false, _) => idle_since = None,
                    (true, None) => idle_since = Some(tokio::time::Instant::now()),
                    (true, Some(since)) if since.elapsed() >= IDLE_EXIT => {
                        tracing::info!("idle; exiting");
                        let _ = commands.send(Command::Shutdown).await;
                        let _ = (&mut engine).await;
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
    let _ = std::fs::remove_file(path);
    Ok(())
}

/// Serves one app: handshake, replay of current state, then events out and commands in.
async fn client(
    stream: UnixStream,
    commands: async_channel::Sender<Command>,
    broadcast: broadcast::Sender<String>,
    snapshot: Arc<Mutex<Snapshot>>,
) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();

    write_line(&mut write, &serde_json::to_string(&Hello::current())?).await?;
    let Some(first) = lines.next_line().await? else {
        return Ok(());
    };
    let hello: Hello = serde_json::from_str(&first)?;
    if hello.protocol != aerie_engine::PROTOCOL {
        tracing::info!(app = %hello.version, "app speaks another protocol");
        // The app restarts the service; only Shutdown is understood across versions.
    }

    let mut events = broadcast.subscribe();
    let replay = snapshot.lock().expect("not poisoned").lines();
    for line in replay {
        write_line(&mut write, &line).await?;
    }
    let writer = tokio::spawn(async move {
        loop {
            match events.recv().await {
                Ok(line) => {
                    if write_line(&mut write, &line).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    while let Some(line) = lines.next_line().await? {
        match serde_json::from_str::<Command>(&line) {
            Ok(command) => {
                if commands.send(command).await.is_err() {
                    break;
                }
            }
            Err(err) => tracing::warn!(%err, "ignored a command the service didn't understand"),
        }
    }
    writer.abort();
    Ok(())
}

async fn write_line(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    line: &str,
) -> std::io::Result<()> {
    write.write_all(line.as_bytes()).await?;
    write.write_all(b"\n").await
}

/// With no window open, conflicts still deserve a desktop notification.
fn notify_without_app(event: &Event) {
    let Event::ConflictKept(path) = event else {
        return;
    };
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let _ = std::process::Command::new("notify-send")
        .args([
            "--app-name=Aerie",
            "--icon=ai.lastrefuge.Aerie",
            "Kept both versions",
            &format!("Drive’s copy is “{name}”."),
        ])
        .spawn()
        .map(|mut child| std::thread::spawn(move || child.wait()));
}

/// Logs to the same local folder as the app, in files named `aerie-service`.
fn logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("AERIE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let dir = std::env::var_os("XDG_STATE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })
        .unwrap_or_default()
        .join("aerie/logs");
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("aerie-service")
        .filename_suffix("log")
        .max_log_files(7)
        .build(dir)
        .ok()?;
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .init();
    Some(guard)
}
