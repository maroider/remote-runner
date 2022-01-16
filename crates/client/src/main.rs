use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
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
    let subscriber = FmtSubscriber::builder()
        .event_format(format::format().pretty().with_source_location(true))
        //  .with_max_level(tokio::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Could not set default subscriber");

    if env::args_os().nth(1).is_none() {
        loop {}
    }
    let arg = env::args_os().nth(1).expect("No argument provided");
    let action = if arg.to_str().map_or(false, |arg| arg == "--upgrade") {
        Action::Upgrade
    } else {
        Action::Run(PathBuf::from(arg))
    };
    let server_addr: SocketAddr = env::var("REMOTE_RUNNER_SERVER")
        .map(|var| var.parse().expect("Malformed socket address"))
        .unwrap_or(common::default_socket_address().into());

    ctrlc::set_handler(|| CTRL_C.store(true, Ordering::SeqCst))
        .expect("Could not set Ctrl+C handler");
    let mut rx = spawn_worker_thread(&server_addr, action);

    loop {
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
            Resp::Quit => {
                // Make the prompt print nicely after Ctrl+C
                println!();
                break;
            }
            Resp::Errored => {
                break;
            }
        }
    }
}

enum Action {
    Run(PathBuf),
    Upgrade,
}

fn spawn_worker_thread(server_addr: &SocketAddr, action: Action) -> mpsc::UnboundedReceiver<Resp> {
    let server_addr = server_addr.clone();
    let (tx, rx) = mpsc::unbounded_channel();

    let mut buf = Vec::new();
    macro_rules! mprintln {
        ($($arg:tt)*) => {
            {
                use std::io::Write;
                writeln!(&mut buf, $($arg)*).unwrap();
                tx.send(Resp::Print(buf.clone())).unwrap();
                buf.clear();
            }
        }
    }
    macro_rules! errored {
        ($error:expr) => {{
            tracing::error!("{}", $error);
            tx.send(Resp::Errored).unwrap();
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
                style("[1/3]").bold().dim(),
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
                                return;
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
                            return;
                        }
                        Err(err) => {
                            errored!(err);
                            return;
                        }
                    }
                }};
            }

            macro_rules! write_message {
                ($stream:ident, $msg:expr) => {{
                    use tokio::io::AsyncWriteExt;
                    if let Err(err) = common::write_message(&mut wbuf, $msg) {
                        errored!(err);
                        return;
                    }
                    if let Err(err) = $stream.write_all(&mut wbuf).await {
                        errored!(err);
                        return;
                    }
                    wbuf.clear();
                }};
            }

            match TcpStream::connect(&server_addr).await {
                Ok(mut stream) => {
                    let _server_version = read_message!(stream, common::Version);
                    let client_version = common::Version {
                        major: 0,
                        minor: 1,
                        patch: 0,
                    };
                    write_message!(stream, &client_version);

                    match action {
                        Action::Run(executable) => {
                            let name = executable
                                .file_name()
                                .unwrap()
                                .to_string_lossy()
                                .into_owned();
                            mprintln!(
                                "{} Uploading {}",
                                style("[2/3]").bold().dim(),
                                style(&name).bold()
                            );
                            let data = tokio::fs::read(executable).await.unwrap();
                            write_message!(
                                stream,
                                &common::ServerCmd::Run(common::RunExecutable {
                                    name: name.clone(),
                                    data
                                })
                            );
                            mprintln!(
                                "{} Running {}",
                                style("[3/3]").bold().dim(),
                                style(&name).bold()
                            );

                            // FIXME: Don't do a busy loop
                            while !CTRL_C.load(Ordering::SeqCst) {}
                            tx.send(Resp::Quit).unwrap();
                        }
                        Action::Upgrade => todo!(),
                    }
                }
                Err(err) => errored!(err),
            }
        });
    });
    rx
}

#[derive(Debug)]
enum Resp {
    Print(Vec<u8>),
    Quit,
    Errored,
}
