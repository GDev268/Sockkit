use std::net::SocketAddr;
use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream as TokioTcpStream;
use crate::errors::{ReadError, WriteError};

const READ_BUF_SIZE: usize = 1024;

pub struct TcpStream {
    addr: SocketAddr,
    stream: TokioTcpStream,
}

impl TcpStream {
    pub fn new(addr: SocketAddr, stream: TokioTcpStream) -> Self {
        Self { addr, stream }
    }

    pub async fn recv(&mut self) -> Result<BytesMut, ReadError> {
        let mut buffer = BytesMut::with_capacity(READ_BUF_SIZE);
        buffer.resize(READ_BUF_SIZE, 0);

        let n = match self.stream.read(&mut buffer).await {
            Ok(0) => return Err(ReadError::EmptyBuffer), // connection closed
            Ok(size) => size,
            Err(e) => return Err(ReadError::FailedToRead(e)),
        };

        buffer.truncate(n);
        Ok(buffer)
    }

    pub async fn send(&mut self, bytes: Bytes) -> Result<(), WriteError> {
        if bytes.is_empty() {
            return Err(WriteError::EmptyBuffer);
        }

        self.stream
            .write_all(&bytes)
            .await
            .map_err(WriteError::FailedToWrite)
    }
}
