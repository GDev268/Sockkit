use std::net::SocketAddr;
use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use crate::client::{ReadError, WriteError};

const READ_BUF_SIZE: usize = 1024;

pub(crate) struct TcpClient {
    stream: TcpStream,
}

impl TcpClient {
    pub(crate) async fn new(addr: SocketAddr) -> Result<Self, std::io::Error> {
        let stream = TcpStream::connect(addr).await?;

        Ok(Self { stream })
    }

    pub(crate) async fn recv(&mut self) -> Result<BytesMut, ReadError> {
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

    pub(crate) async fn send(&mut self, bytes: Bytes) -> Result<(), WriteError> {
        if bytes.is_empty() {
            return Err(WriteError::EmptyBuffer);
        }

        self.stream
            .write_all(&bytes)
            .await
            .map_err(WriteError::FailedToWrite)
    }
}