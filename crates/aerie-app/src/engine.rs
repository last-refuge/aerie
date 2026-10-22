//! The app's connection to `aerie-service`, which runs the Drive engine. The connection
//! lives on its own thread: it starts the service when none is running, reconnects if the
//! service restarts, and replaces a service from another version.

use std::path::PathBuf;
use std::time::Duration;

pub use aerie_engine::{
    Activity, ActivityItem, Command, Event, ItemState, LocalFile, SyncState, Way,
};
use aerie_engine::{Hello, PROTOCOL, socket_path};
use async_channel::{Receiver, Sender};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub struct Engine {
    commands: Sender<Command>,
}

impl Engine {
    pub fn start(events: Sender<Event>) -> Self {
        let (commands, outbox) = async_channel::unbounded();
        std::thread::Builder::new()
            .name("aerie-connection".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("the async runtime starts");
                runtime.block_on(stay_connected(events, outbox));
            })
            .expect("the connection thread starts");
        Self { commands }
    }

    /// Queues a command; it's delivered as soon as the service is reachable.
    pub fn send(&self, command: Command) {
        let _ = self.commands.try_send(command);
    }
}

async fn stay_connected(events: Sender<Event>, outbox: Receiver<Command>) {
    let mut attempts = 0u32;
    while !outbox.is_closed() {
        match UnixStream::connect(socket_path()).await {
            Ok(stream) => {
                attempts = 0;
                match converse(stream, &events, &outbox).await {
                    Ok(Ending::Closed) => tracing::info!("the service closed the connection"),
                    Ok(Ending::Replaced) => {
                        // Give the old service a moment to exit before starting ours.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        start_service();
                    }
                    Err(err) => tracing::warn!(%err, "lost the connection to the service"),
                }
            }
            Err(_) => {
                if attempts.is_multiple_of(20) {
                    start_service();
                }
                attempts += 1;
                let backoff = Duration::from_millis(100 * u64::from(attempts.min(20)));
                tokio::time::sleep(backoff).await;
            }
        }
    }
}

enum Ending {
    Closed,
    /// The service spoke another protocol and was asked to stop.
    Replaced,
}

async fn converse(
    stream: UnixStream,
    events: &Sender<Event>,
    outbox: &Receiver<Command>,
) -> std::io::Result<Ending> {
    let (read, mut write) = stream.into_split();
    let mut lines = BufReader::new(read).lines();
    let Some(first) = lines.next_line().await? else {
        return Ok(Ending::Closed);
    };
    let theirs: Hello = serde_json::from_str(&first)?;
    tracing::info!(service = %theirs.version, "connected to the service");
    send(&mut write, &Hello::current()).await?;
    if theirs.protocol != PROTOCOL {
        tracing::info!(service = %theirs.version, "replacing a service from another version");
        send(&mut write, &Command::Shutdown).await?;
        return Ok(Ending::Replaced);
    }

    loop {
        tokio::select! {
            line = lines.next_line() => match line? {
                Some(line) => match serde_json::from_str::<Event>(&line) {
                    Ok(event) => {
                        if events.send(event).await.is_err() {
                            return Ok(Ending::Closed);
                        }
                    }
                    Err(err) => tracing::warn!(%err, "ignored an event the app didn't understand"),
                },
                None => return Ok(Ending::Closed),
            },
            command = outbox.recv() => match command {
                Ok(command) => send(&mut write, &command).await?,
                Err(_) => return Ok(Ending::Closed),
            },
        }
    }
}

async fn send(
    write: &mut tokio::net::unix::OwnedWriteHalf,
    message: &impl serde::Serialize,
) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    write.write_all(&line).await
}

/// Starts `aerie-service`, preferring the copy installed beside this binary. The service
/// keeps running after the app quits; a thread reaps it if it exits first.
pub fn start_service() {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("aerie-service")))
        .filter(|path| path.exists());
    let program = beside.unwrap_or_else(|| PathBuf::from("aerie-service"));
    match std::process::Command::new(&program)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            tracing::info!("started the background service");
            std::thread::spawn(move || child.wait());
        }
        Err(err) => tracing::error!(%err, "couldn't start the background service"),
    }
}
