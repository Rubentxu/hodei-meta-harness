//! Loopback-only TCP access to an execd reachable inside a Docker container.
//!
//! The helper argv is executed directly, without a shell. Its trailing arguments
//! must select a raw TCP client (for example `["/bin/busybox", "nc"]`);
//! the destination `127.0.0.1 44772` is appended by this module.

use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use bollard::{
    Docker,
    container::LogOutput,
    exec::{CreateExecOptions, StartExecOptions, StartExecResults},
};
use futures_util::{Stream, StreamExt};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::{JoinHandle, JoinSet},
};

const DESTINATION: &str = "127.0.0.1";
const PORT: &str = "44772";

/// How long a connection may stay silent before it is closed.
///
/// The relay is a loopback listener without a credential of its own, so a
/// connection that never says anything would hold a slot and a `docker exec`
/// inside the sandbox forever. The deadline is an IDLE one: a session that
/// keeps talking is never interrupted.
const IDLE_DEADLINE: Duration = Duration::from_secs(900);

/// Owns the listener and all connection tasks. Dropping it cancels them.
#[must_use]
pub struct RelayHandle {
    address: SocketAddr,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl RelayHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Whether the listener is still running.
    ///
    /// A relay whose task ended is dead: advertising its port would send
    /// the client to a socket nobody answers.
    pub fn is_running(&self) -> bool {
        self.task.as_ref().is_some_and(|task| !task.is_finished())
    }

    /// Cancel the listener and active connections and wait for local cleanup.
    pub async fn shutdown(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }

    /// Wait for an unexpected listener failure. Dropping this future cancels
    /// the relay as well, because it owns the handle.
    pub async fn wait(mut self) -> io::Result<()> {
        let result = self.task.as_mut().expect("relay task exists").await;
        self.task.take();
        result.map_err(io::Error::other)?
    }
}

impl Drop for RelayHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Bind an ephemeral IPv4 loopback port and launch one Docker exec per client.
///
/// `helper_argv` is the executable and its fixed arguments, not a shell command;
/// it must support the appended host and port arguments and binary stdio.
/// Docker access remains entirely in the lifecycle server. The listener has no
/// authentication of its own: the credential that authorizes the execution
/// plane is the execd's own, and it travels with the endpoint contract.
pub async fn start(
    docker: Docker,
    container: String,
    helper_argv: Vec<String>,
) -> io::Result<RelayHandle> {
    if container.is_empty() || helper_argv.first().is_none_or(|s| s.is_empty()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "container and helper executable are required",
        ));
    }
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let address = listener.local_addr()?;
    let argv = Arc::new(helper_argv);
    let slots = Arc::new(tokio::sync::Semaphore::new(32));
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (socket, _) = accepted?;
                    let Ok(permit) = Arc::clone(&slots).try_acquire_owned() else { continue };
                    let docker = docker.clone();
                    let container = container.clone();
                    let argv = Arc::clone(&argv);
                    connections.spawn(async move {
                        let _permit = permit;
                        // Errors can contain daemon responses or helper output; never log them.
                        let _ = relay_connection(docker, &container, &argv, socket).await;
                    });
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    Ok(RelayHandle {
        address,
        task: Some(task),
    })
}

async fn relay_connection(
    docker: Docker,
    container: &str,
    helper: &[String],
    socket: TcpStream,
) -> io::Result<()> {
    let mut cmd = helper.to_vec();
    cmd.extend([DESTINATION.to_owned(), PORT.to_owned()]);
    let exec = docker
        .create_exec(
            container,
            CreateExecOptions {
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(false),
                tty: Some(false),
                cmd: Some(cmd),
                ..Default::default()
            },
        )
        .await
        .map_err(io::Error::other)?;
    let attached = docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                detach: false,
                tty: false,
                output_capacity: None,
            }),
        )
        .await
        .map_err(io::Error::other)?;
    match attached {
        StartExecResults::Attached { input, output } => forward(socket, input, output).await,
        StartExecResults::Detached => Err(io::Error::other("Docker exec unexpectedly detached")),
    }
}

async fn forward<S, W, O, E>(socket: S, input: W, output: O) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    W: AsyncWrite + Unpin,
    O: Stream<Item = Result<LogOutput, E>> + Unpin,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    forward_with_deadline(socket, input, output, IDLE_DEADLINE).await
}

async fn forward_with_deadline<S, W, O, E>(
    socket: S,
    mut input: W,
    mut output: O,
    idle: Duration,
) -> io::Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
    W: AsyncWrite + Unpin,
    O: Stream<Item = Result<LogOutput, E>> + Unpin,
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    let (mut reader, mut writer) = tokio::io::split(socket);
    let upload = async {
        copy_until_idle(&mut reader, &mut input, idle).await?;
        input.shutdown().await
    };
    let download = async {
        while let Some(frame) = tokio::time::timeout(idle, output.next())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "idle relay connection"))?
        {
            match frame.map_err(io::Error::other)? {
                LogOutput::StdOut { message } | LogOutput::Console { message } => {
                    writer.write_all(&message).await?
                }
                // stderr is deliberately not attached and must never enter the protocol.
                _ => {}
            }
        }
        writer.shutdown().await
    };
    tokio::try_join!(upload, download)?;
    Ok(())
}

/// Copy until the source ends or the connection stays silent for too long.
///
/// The deadline is the point of the function: a client that opens the
/// relay and never speaks would otherwise hold a slot and a process
/// inside the sandbox indefinitely.
async fn copy_until_idle<R, W>(reader: &mut R, writer: &mut W, idle: Duration) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        let read = match tokio::time::timeout(idle, reader.read(&mut buffer)).await {
            Ok(result) => result?,
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "idle relay connection",
                ));
            }
        };
        if read == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..read]).await?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn forwards_binary_and_preserves_half_close() {
        let (mut client, relay_socket) = tokio::io::duplex(1024);
        let (input, mut helper_stdin) = tokio::io::duplex(1024);
        let output = futures_util::stream::iter(vec![Ok::<_, io::Error>(LogOutput::StdOut {
            message: b"\0response\xff".as_slice().into(),
        })]);
        let task = tokio::spawn(forward(relay_socket, input, output));
        client.write_all(b"\xffrequest\0").await.unwrap();
        client.shutdown().await.unwrap();
        let mut received = Vec::new();
        helper_stdin.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"\xffrequest\0");
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"\0response\xff");
        task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rejects_empty_helper_before_binding() {
        let docker = Docker::connect_with_local_defaults().unwrap();
        assert!(
            matches!(start(docker, "container".into(), vec![]).await, Err(e) if e.kind() == io::ErrorKind::InvalidInput)
        );
    }

    #[tokio::test]
    async fn a_silent_connection_is_closed_instead_of_holding_the_slot() {
        // A client that opens the relay and never speaks would otherwise
        // hold a slot and a process inside the sandbox indefinitely.
        let (client, relay_socket) = tokio::io::duplex(1024);
        let (input, mut helper_stdin) = tokio::io::duplex(1024);
        let output = futures_util::stream::pending::<Result<LogOutput, std::io::Error>>();
        let task = tokio::spawn(forward_with_deadline(
            relay_socket,
            input,
            output,
            Duration::from_millis(150),
        ));
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the idle deadline closes the connection")
            .expect("the task joined");
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        // Nothing was forwarded: the client never said anything.
        let mut received = Vec::new();
        let _ = helper_stdin.read_to_end(&mut received).await;
        assert!(received.is_empty());
        drop(client);
    }

    #[tokio::test]
    async fn a_talking_connection_is_not_interrupted() {
        let (mut client, relay_socket) = tokio::io::duplex(1024);
        let (input, mut helper_stdin) = tokio::io::duplex(1024);
        let output = futures_util::stream::iter(vec![Ok::<_, io::Error>(LogOutput::StdOut {
            message: b"\0response\xff".as_slice().into(),
        })]);
        let task = tokio::spawn(forward_with_deadline(
            relay_socket,
            input,
            output,
            Duration::from_secs(30),
        ));
        client.write_all(b"\xffrequest\0").await.unwrap();
        client.shutdown().await.unwrap();
        let mut received = Vec::new();
        helper_stdin.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, b"\xffrequest\0");
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        assert_eq!(response, b"\0response\xff");
        task.await.unwrap().unwrap();
    }
}
