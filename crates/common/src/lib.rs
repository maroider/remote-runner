use std::fmt;
use std::io;
use std::io::{Cursor, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddrV4};

use serde::{Deserialize, Serialize};
use tracing::{error, trace};

pub fn default_socket_address() -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, default_port())
}

pub fn default_port() -> u16 {
    4444
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub enum ServerCmd {
    Run(RunExecutable),
    UpgradeSelf(SelfUpdate),
}

#[derive(Deserialize, Serialize)]
pub struct RunExecutable {
    pub name: String,
    pub data: Vec<u8>,
}

impl fmt::Debug for RunExecutable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RunExecutable")
            .field("name", &self.name)
            .field("data", &[0])
            .finish()
    }
}

#[derive(Deserialize, Serialize)]
pub struct SelfUpdate {
    pub data: Vec<u8>,
}

impl fmt::Debug for SelfUpdate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SelfUpdate").field("data", &[0]).finish()
    }
}

pub struct ServerUpdate {
    pub panicked: bool,
    pub stdio: StdioBytes,
}

#[derive(Deserialize, Serialize)]
pub struct StdioBytes {
    pub stream: StdStream,
    pub data: Vec<u8>,
}

impl fmt::Debug for StdioBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StdioBytes")
            .field("stream", &self.stream)
            .field("data", &[0])
            .finish()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub enum StdStream {
    Stdout,
    Stderr,
}

pub struct MessageReader {
    buf: Vec<u8>,
}

impl MessageReader {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub async fn read_message<T, S>(&mut self, stream: &mut S) -> Result<T, MessageReadError>
    where
        for<'de> T: Deserialize<'de> + fmt::Debug,
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        loop {
            use tokio::io::AsyncReadExt;

            match stream.read_buf(&mut self.buf).await {
                Ok(num) => {
                    if num == 0 {
                        error!("Read {num} bytes. Assuming the other end is disconnected.");
                        return Err(MessageReadError::NoBytesRead);
                    } else {
                        trace!("Read {num} bytes");
                    }
                }
                Err(err) => {
                    error!("Reading from TCP stream failed: {err}");
                    return Err(MessageReadError::StreamRead(err));
                }
            }

            trace!(
                "Reading message from data buffer of {} bytes",
                self.buf.len(),
            );
            let mut cursor = Cursor::new(&*self.buf);
            let len: u32 = bincode::deserialize_from(&mut cursor).map_err(|err| {
                error!("Deserializing from buffer failed: {err}");
                MessageReadError::Bincode(err)
            })?;
            trace!("Message length should be {len}");

            if ((self.buf.len() - cursor.position() as usize) as u32) < len {
                trace!("Buffer does not contain sufficient data yet ...");
                continue;
            }

            let message = bincode::deserialize_from(&mut cursor).map_err(|err| {
                error!("Deserializing from buffer failed: {err}");
                MessageReadError::Bincode(err)
            })?;
            let written = cursor.position() as usize;
            let _ = self.buf.drain(..written);

            trace!("Received message: {message:?}");
            return Ok(message);
        }
    }
}

pub struct MessageWriter {
    buf: Vec<u8>,
}

impl MessageWriter {
    pub fn new() -> Self {
        Self { buf: Vec::new() }
    }

    pub async fn write_message<T, S>(
        &mut self,
        stream: &mut S,
        msg: &T,
    ) -> Result<usize, MessageWriteError>
    where
        T: Serialize,
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        use tokio::io::AsyncWriteExt;

        let mut cursor = Cursor::new(&mut self.buf);
        bincode::serialize_into(&mut cursor, &0u32).map_err(|err| {
            error!("Serializing message failed: {err}");
            MessageWriteError::Serialize(err)
        })?;
        let base = cursor.position();
        bincode::serialize_into(&mut cursor, msg).map_err(|err| {
            error!("Serializing message failed: {err}");
            MessageWriteError::Serialize(err)
        })?;
        let written = cursor.position() - base;
        cursor.seek(SeekFrom::Start(0)).unwrap();
        bincode::serialize_into(&mut cursor, &(written as u32)).map_err(|err| {
            error!("Serializing message failed: {err}");
            MessageWriteError::Serialize(err)
        })?;

        if let Err(err) = stream.write_all(&mut self.buf).await {
            error!("Writing to TCP stream failed: {err}");
            return Err(MessageWriteError::StreamWrite(err));
        }
        self.buf.clear();

        Ok(written as usize)
    }
}

#[derive(Debug)]
pub enum MessageReadError {
    /// No bytes read, assuming disconnect
    NoBytesRead,
    /// Reading from the stream failed
    StreamRead(io::Error),
    Bincode(bincode::Error),
}

impl fmt::Display for MessageReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageReadError::NoBytesRead => write!(
                f,
                "Read 0 bytes from stream. The other end has likely disconnected."
            ),
            MessageReadError::StreamRead(err) => write!(f, "Could not read from stream: {err}"),
            MessageReadError::Bincode(err) => err.fmt(f),
        }
    }
}

#[derive(Debug)]
pub enum MessageWriteError {
    Serialize(bincode::Error),
    StreamWrite(io::Error),
}
impl fmt::Display for MessageWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageWriteError::Serialize(err) => write!(f, "Could not serialize message: {err}"),
            MessageWriteError::StreamWrite(err) => {
                write!(f, "Could not write message to stream: {err}")
            }
        }
    }
}
