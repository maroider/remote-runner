use std::env;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use console::style;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{error, trace};
use tracing_subscriber::fmt::format;
use tracing_subscriber::FmtSubscriber;

static CTRL_C: AtomicBool = AtomicBool::new(false);

fn main() {
    ctrlc::set_handler(|| CTRL_C.store(true, Ordering::SeqCst))
        .expect("Could not set Ctrl+C handler");
    let subscriber = FmtSubscriber::builder()
        .event_format(format::format().pretty().with_source_location(true))
        //  .with_max_level(tokio::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Could not set default subscriber");

    let executable = env::args_os().nth(1).expect("No executable provided");
    let executable = Path::new(&executable);
    let server_addr: SocketAddr = env::var("REMOTE_RUNNER_SERVER")
        .map(|var| var.parse().expect("Malformed socket address"))
        .unwrap_or(common::default_socket_address().into());

    let (tx, mut rx) = spawn_worker_thread(&server_addr, executable);

    loop {
        if CTRL_C.load(Ordering::SeqCst) {
            println!();
            println!("Ctrl+C received");
            tx.send(Msg::Cancel).unwrap();
            break;
        }

        let resp = match rx.try_recv() {
            Ok(resp) => resp,
            Err(mpsc::error::TryRecvError::Empty) => continue,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                println!(
                    "{} Network thread disconnected unexpectedly",
                    style("[ERR]").bold().red(),
                );
                break;
            }
        };

        match resp {
            Resp::Print(buf) => {
                use std::io::Write;
                let stdout = std::io::stdout();
                let mut stdout = stdout.lock();
                stdout.write(&buf).unwrap();
                stdout.flush().unwrap();
            }
            Resp::Errored => {
                break;
            }
        }
    }
}

fn spawn_worker_thread(
    server_addr: &SocketAddr,
    executable: &Path,
) -> (mpsc::UnboundedSender<Msg>, mpsc::UnboundedReceiver<Resp>) {
    let server_addr = server_addr.clone();
    let _executable = executable.to_owned();
    let (tx, _trx) = mpsc::unbounded_channel();
    let (ttx, rx) = mpsc::unbounded_channel();

    let mut buf = Vec::new();
    macro_rules! mprintln {
        ($($arg:tt)*) => {
            {
                use std::io::Write;
                writeln!(&mut buf, $($arg)*).unwrap();
                ttx.send(Resp::Print(buf.clone())).unwrap();
                buf.clear();
            }
        }
    }
    macro_rules! errored {
        ($error:expr) => {{
            tracing::error!("{}", $error);
            ttx.send(Resp::Errored).unwrap();
        }};
    }

    thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let _guard = rt.enter();
        rt.block_on(async {
            mprintln!(
                "{} Looking for server at {}",
                style("[1/4]").bold().dim(),
                style(server_addr).bold()
            );

            let mut rbuf = Vec::new();
            let mut wbuf = Vec::new();

            macro_rules! read_message {
                ($stream:ident, $msg:ty) => {{
                    use tokio::io::AsyncReadExt;
                    trace!("Reading from stream to buffer");
                    match $stream.read_buf(&mut rbuf).await {
                        Ok(num) => {
                            if num == 0 {
                                error!("Read {num} bytes. Assuming the other end is disconnected.");
                                continue;
                            } else {
                                trace!("Read {num} bytes");
                            }
                        }
                        Err(err) => errored!(err),
                    }
                    match common::read_message::<$msg>(&mut rbuf) {
                        Ok(msg) => {
                            trace!("Received message {:?}", msg);
                            msg
                        }
                        Err(err) if !err.data_too_short() => {
                            errored!(err);
                            continue;
                        }
                        Err(err) => {
                            errored!(err);
                            continue;
                        }
                    }
                }};
            }

            macro_rules! write_message {
                ($stream:ident, $msg:expr) => {{
                    use tokio::io::AsyncWriteExt;
                    if let Err(err) = common::write_message(&mut wbuf, $msg) {
                        errored!(err);
                        continue;
                    }
                    if let Err(err) = $stream.write_all(&mut wbuf).await {
                        errored!(err);
                        continue;
                    }
                    wbuf.clear();
                }};
            }

            match TcpStream::connect(&server_addr).await {
                Ok(mut stream) => loop {
                    let _server_version = read_message!(stream, common::Version);
                    let client_version = common::Version {
                        major: 0,
                        minor: 1,
                        patch: 0,
                    };
                    write_message!(stream, &client_version);
                    break;
                },
                Err(err) => errored!(err),
            }
        });
    });
    (tx, rx)
}

#[derive(Debug)]
enum Msg {
    Cancel,
}

#[derive(Debug)]
enum Resp {
    Print(Vec<u8>),
    Errored,
}
