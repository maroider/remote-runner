use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use console::style;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing_subscriber::fmt::format;
use tracing_subscriber::FmtSubscriber;

fn main() {
    let subscriber = FmtSubscriber::builder()
        .event_format(format::format().pretty().with_source_location(true))
        //  .with_max_level(tokio::Level::TRACE)
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("Could not set default subscriber");

    let arg = env::args_os().nth(1).expect("No argument provided");
    let action = if arg.to_str().map_or(false, |arg| arg == "--upgrade") {
        Action::Upgrade
    } else {
        Action::Run(PathBuf::from(arg))
    };
    let server_addr: SocketAddr = env::var("REMOTE_RUNNER_SERVER")
        .map(|var| var.parse().expect("Malformed socket address"))
        .unwrap_or(common::default_socket_address().into());

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
            Resp::Print(stdio) => {
                use std::io::Write;
                let stdout = std::io::stdout();
                let mut stdout = stdout.lock();
                stdout.write_all(&stdio.data).unwrap();
                stdout.flush().unwrap();
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
                tx.send(
                    Resp::Print(
                        common::StdioBytes {
                            stream: common::StdStream::Stdout,
                            data: buf.clone()
                        }
                    )
                ).unwrap();
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
            .enable_time()
            .build()
            .unwrap();
        let _guard = rt.enter();
        rt.block_on(async {
            mprintln!(
                "{} Looking for server at {}",
                style("[1/3]").bold().dim(),
                style(server_addr).bold()
            );

            let mut mread = common::MessageReader::new();
            let mut mwrite = common::MessageWriter::new();

            match TcpStream::connect(&server_addr).await {
                Ok(mut stream) => {
                    let _server_version = mread
                        .read_message::<common::Version, _>(&mut stream)
                        .await
                        .unwrap();
                    let client_version = common::Version {
                        major: 0,
                        minor: 1,
                        patch: 0,
                    };
                    mwrite
                        .write_message(&mut stream, &client_version)
                        .await
                        .unwrap();

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
                            mwrite
                                .write_message(
                                    &mut stream,
                                    &common::ServerCmd::Run(common::RunExecutable {
                                        name: name.clone(),
                                        data,
                                    }),
                                )
                                .await
                                .unwrap();
                            mprintln!(
                                "{} Running {}",
                                style("[3/3]").bold().dim(),
                                style(&name).bold()
                            );

                            #[derive(Debug)]
                            enum InternalMsg {
                                Stdio(common::StdioBytes),
                            }

                            let (itx, mut irx) = mpsc::channel(1);
                            let (mut rstream, wstream) = stream.into_split();
                            tokio::spawn({
                                let itx = itx;
                                async move {
                                    loop {
                                        let update = mread
                                            .read_message::<common::ServerUpdate, _>(&mut rstream)
                                            .await
                                            .unwrap();
                                        if update.panicked {
                                            // TODO: Is this even useful?
                                        }
                                        itx.send(InternalMsg::Stdio(update.stdio)).await.unwrap();
                                    }
                                }
                            });
                            while let Some(msg) = irx.recv().await {
                                match msg {
                                    InternalMsg::Stdio(stdio) => {
                                        tx.send(Resp::Print(stdio)).unwrap()
                                    }
                                }
                            }
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
    Print(common::StdioBytes),
    Errored,
}
