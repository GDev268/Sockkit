use std::fmt;

#[derive(Debug)]
pub enum ReadError {
    EmptyBuffer,
    FailedToRead(std::io::Error)
}

#[derive(Debug)]
pub enum WriteError {
    EmptyBuffer,
    FailedToWrite(std::io::Error)
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::EmptyBuffer => write!(f, "read error: buffer is empty"),
            ReadError::FailedToRead(err) => write!(f, "read error: failed to read: {}", err),
        }
    }
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::EmptyBuffer => write!(f, "write error: buffer is empty"),
            WriteError::FailedToWrite(err) => write!(f, "write error: failed to write: {}", err),
        }
    }
}