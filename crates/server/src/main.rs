use std::net::{Ipv4Addr, SocketAddrV4};

use tokio::net::TcpListener;
use tracing::{debug, error, trace, Level};
use tracing_subscriber::{fmt::format, FmtSubscriber};

fn main() {
    let subscriber = FmtSubscriber::builder()
        .event_format(format::format().pretty().with_source_location(true))
        .with_max_level(Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Could not set default subscriber");
    tracing::info!("Hello tracing");

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

        let mut rbuf = Vec::new();
        let mut wbuf = Vec::new();

        macro_rules! read_message {
            ($stream:ident, $msg:ty) => {{
                use tokio::io::AsyncReadExt;
                match $stream.read_buf(&mut rbuf).await {
                    Ok(num) => {
                        if num == 0 {
                            error!("Read {num} bytes. Assuming the other end is disconnected.");
                            continue;
                        } else {
                            trace!("Read {num} bytes");
                        }
                    }
                    Err(err) => {
                        error!("Reading from TCP stream failed: {}", err);
                        continue;
                    }
                }
                match common::read_message::<$msg>(&mut rbuf) {
                    Ok(msg) => {
                        trace!("Received message: {msg:?}");
                        msg
                    }
                    Err(err) if !err.data_too_short() => {
                        error!("Deserializing from buffer failed: {}", err);
                        continue;
                    }
                    Err(err) => {
                        error!("{}", err);
                        continue;
                    }
                }
            }};
        }

        macro_rules! write_message {
            ($stream:ident, $msg:expr) => {{
                use tokio::io::AsyncWriteExt;
                if let Err(err) = common::write_message(&mut wbuf, $msg) {
                    error!("Serializing message failed: {}", err);
                    continue;
                }
                if let Err(err) = $stream.write_all(&mut wbuf).await {
                    error!("Writing to TCP stream failed: {}", err);
                    continue;
                }
                wbuf.clear();
            }};
        }

        loop {
            rbuf.clear();
            wbuf.clear();

            debug!("Listening for new connections");
            let (mut stream, peer_socket_addr) = match listener.accept().await {
                Ok(ok) => ok,
                Err(err) => {
                    error!("Error while trying to accept incoming connection: {}", err);
                    continue;
                }
            };
            debug!("Accepted connection on {}", peer_socket_addr);
            write_message!(
                stream,
                &common::Version {
                    major: 0,
                    minor: 1,
                    patch: 0,
                }
            );
            debug!("Server version sent");
            let version = read_message!(stream, common::Version);
            debug!(
                "Client version received: {}.{}.{}",
                version.major, version.minor, version.patch
            );
            if version.major == 0 && version.minor == 0 && version.patch == 0 {
                continue;
            }

            read_message!(stream, common::Version);
        }
    });
}
