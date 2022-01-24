use std::env;
use std::io;
use std::mem;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::process::Stdio;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::OnceCell;
use tracing::trace;
use tracing::{debug, error, Level};
use tracing_subscriber::{fmt::format, FmtSubscriber};

macro_rules! lc_err {
    ($e:expr) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => {
                error!("{err}");
                continue;
            }
        }
    };
    ($e:expr, $label:lifetime) => {
        match $e {
            Ok(ok) => ok,
            Err(err) => {
                error!("{err}");
                continue $label;
            }
        }
    };
}

fn main() {
    let subscriber = FmtSubscriber::builder()
        .event_format(format::format().pretty().with_source_location(true))
        .with_max_level(Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Could not set default subscriber");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .unwrap();
    let _guard = rt.enter();
    rt.block_on(async {
        let listener = TcpListener::bind(SocketAddrV4::new(
            Ipv4Addr::UNSPECIFIED,
            common::default_port(),
        ))
        .await
        .unwrap();

        let mut mread = common::MessageReader::new();
        let mut mwrite = common::MessageWriter::new();

        loop {
            debug!("Listening for new connections");
            let (mut stream, peer_socket_addr) = match listener.accept().await {
                Ok(ok) => ok,
                Err(err) => {
                    error!("Error while trying to accept incoming connection: {}", err);
                    continue;
                }
            };
            debug!("Accepted connection on {}", peer_socket_addr);
            lc_err!(
                mwrite
                    .write_message(
                        &mut stream,
                        &common::Version {
                            major: 0,
                            minor: 1,
                            patch: 0,
                        },
                    )
                    .await
            );
            debug!("Server version sent");
            let version = lc_err!(mread.read_message::<common::Version, _>(&mut stream).await);
            debug!(
                "Client version received: {}.{}.{}",
                version.major, version.minor, version.patch
            );
            if version.major == 0 && version.minor == 0 && version.patch == 0 {
                continue;
            }

            debug!("Waiting for a command");
            let cmd = mread
                .read_message::<common::ServerCmd, _>(&mut stream)
                .await;
            let cmd = lc_err!(cmd);
            match cmd {
                common::ServerCmd::Run(executable) => {
                    use tokio::io::AsyncWriteExt;

                    // FIXME: Signal potential write errors back to the client...
                    debug!("Writing executable data to ./{}", executable.name);
                    let path = env::current_dir()
                        .unwrap()
                        .join("target")
                        .join(&executable.name);
                    let mut file = tokio::fs::File::create(&path).await.unwrap();
                    file.write_all(&executable.data).await.unwrap();
                    make_executable(&file).await.unwrap();
                    mem::drop(file);

                    debug!("Running {}", executable.name);
                    let mut cmd = Command::new(path);
                    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
                    let (iotx, mut iorx) = mpsc::channel(1);
                    let mut process = cmd.spawn().unwrap();
                    let stdio_err = Arc::new(OnceCell::new());

                    trace!("Spawing worker tasks");
                    tokio::spawn({
                        let stdout = process.stdout.take().unwrap();
                        redirect_stdio(
                            iotx.clone(),
                            common::StdStream::Stdout,
                            stdout,
                            stdio_err.clone(),
                        )
                    });
                    tokio::spawn({
                        let stderr = process.stderr.take().unwrap();
                        redirect_stdio(
                            iotx.clone(),
                            common::StdStream::Stderr,
                            stderr,
                            stdio_err.clone(),
                        )
                    });
                    let (rstream, mut wstream) = stream.into_split();
                    tokio::spawn({
                        async move {
                            loop {
                                if let Some(_) = stdio_err.get() {
                                    trace!("Error set");
                                    break;
                                }
                                match rstream.try_read(&mut [0]) {
                                    Ok(written) => {
                                        if written == 0 {
                                            error!(
                                                "0 bytes read. Assuming the client disconnected..."
                                            );
                                            break;
                                        }
                                    }
                                    Err(err) => match err.kind() {
                                        io::ErrorKind::WouldBlock => {}
                                        _ => {
                                            error!("{err}");
                                            break;
                                        }
                                    },
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                            iotx.send(InternalMessage::Stop).await.unwrap();
                        }
                    });
                    trace!("Entering loop");
                    loop {
                        if let Some(msg) = iorx.recv().await {
                            match msg {
                                InternalMessage::Stdio(stdio) => {
                                    if let Err(err) = mwrite
                                        .write_message(
                                            &mut wstream,
                                            &common::ServerUpdate {
                                                panicked: false,
                                                stdio,
                                            },
                                        )
                                        .await
                                    {
                                        error!("Failed to write message to stream: {err}");
                                        break;
                                    }
                                }
                                InternalMessage::Stop => {
                                    trace!("Stop message received");
                                    break;
                                }
                            }
                        }
                    }
                    trace!("Exiting loop ... killing process");
                    if let Err(err) = process.kill().await {
                        error!("Failed to kill process: {err}");
                    }
                }
                common::ServerCmd::UpgradeSelf(_upgrade) => {
                    todo!();
                }
            }
        }
    });
}

#[derive(Debug)]
enum InternalMessage {
    Stdio(common::StdioBytes),
    Stop,
}

async fn redirect_stdio<S>(
    iotx: mpsc::Sender<InternalMessage>,
    stream: common::StdStream,
    mut stdio: S,
    err: Arc<OnceCell<io::Error>>,
) where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    while !err.initialized() {
        if let Err(e) = stdio.read_buf(&mut buf).await {
            error!("Reading from {stream:?} failed: {e}");
            let _ = err.set(e);
            return;
        }
        if !buf.is_empty() {
            iotx.send(InternalMessage::Stdio(common::StdioBytes {
                stream,
                data: buf.clone(),
            }))
            .await
            .unwrap();
            buf.clear();
        }
    }
}

async fn make_executable(file: &tokio::fs::File) -> io::Result<()> {
    #[cfg(target_family = "unix")]
    {
        use std::os::unix::fs::PermissionsExt;

        debug!("Setting the executable bit");
        let metadata = file.metadata().await?;
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o0111);
        file.set_permissions(permissions).await?;
    }
    let _ = file;
    Ok(())
}
