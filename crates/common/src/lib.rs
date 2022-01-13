use std::fmt;
use std::io::{Cursor, Seek, SeekFrom};
use std::net::{Ipv4Addr, SocketAddrV4};

use serde::{Deserialize, Serialize};

pub use bincode;
use tracing::trace;

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

pub fn read_message<T>(data: &mut Vec<u8>) -> Result<T, MessageReadError>
where
    for<'de> T: Deserialize<'de>,
{
    trace!("Reading message from data buffer of {} bytes", data.len());
    let mut cursor = Cursor::new(&*data);
    let len: u32 = bincode::deserialize_from(&mut cursor).map_err(MessageReadError::Bincode)?;
    trace!("Message length should be {}", len);
    if ((data.len() - cursor.position() as usize) as u32) < len {
        return Err(MessageReadError::DataTooShort);
    }
    let message = bincode::deserialize_from(&mut cursor).map_err(MessageReadError::Bincode)?;
    let written = cursor.position() as usize;
    let _ = data.drain(..written);
    Ok(message)
}

#[derive(Debug)]
pub enum MessageReadError {
    DataTooShort,
    Bincode(bincode::Error),
}

impl MessageReadError {
    pub fn data_too_short(&self) -> bool {
        matches!(self, &Self::DataTooShort)
    }
}

impl fmt::Display for MessageReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessageReadError::DataTooShort => write!(
                f,
                "The data buffer does not contain enough bytes to deserialize the message"
            ),
            MessageReadError::Bincode(err) => err.fmt(f),
        }
    }
}

pub fn write_message<T>(buf: &mut Vec<u8>, message: &T) -> Result<usize, bincode::Error>
where
    T: Serialize,
{
    let mut cursor = Cursor::new(buf);
    bincode::serialize_into(&mut cursor, &0u32)?;
    let base = cursor.position();
    bincode::serialize_into(&mut cursor, message)?;
    let written = cursor.position() - base;
    cursor.seek(SeekFrom::Start(0)).unwrap();
    bincode::serialize_into(&mut cursor, &(written as u32))?;
    Ok(written as usize)
}
