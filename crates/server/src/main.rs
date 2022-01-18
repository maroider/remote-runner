use std::{
    env, io, mem,
    net::{Ipv4Addr, SocketAddrV4},
    process::Stdio,
};

use tokio::{net::TcpListener, process::Command};
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
}

fn main() {
    let subscriber = FmtSubscriber::builder()
        .event_format(format::format().pretty().with_source_location(true))
        .with_max_level(Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Could not set default subscriber");

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
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
                    let path = env::current_dir().unwrap().join(&executable.name);
                    let mut file = tokio::fs::File::create(&path).await.unwrap();
                    file.write_all(&executable.data).await.unwrap();
                    make_executable(&file).await.unwrap();
                    mem::drop(file);

                    debug!("Running {}", executable.name);
                    let mut cmd = Command::new(path);
                    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
                    let process = cmd.spawn().unwrap();
                    tokio::spawn(async move {
                        // TODO: Process stdout and stdin here and send them to the client
                    });
                    tokio::spawn(async move {
                        let _stdout = process.stdout.unwrap();
                    });
                    tokio::spawn(async move {
                        let _stderr = process.stderr.unwrap();
                    });

                    // Temporary dummy read
                    lc_err!(mread.read_message::<(), _>(&mut stream).await);
                }
                common::ServerCmd::UpgradeSelf(_upgrade) => {
                    todo!();
                }
            }
        }
    });
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
