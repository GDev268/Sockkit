use crate::tcpstream::TcpStream;
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[derive(Debug)]
pub enum TcpServerError {
    IoError(std::io::Error),
}

pub(crate) struct TcpServer {
    pub(crate) tcp_listener: TcpListener,
}

impl TcpServer {
    pub(crate) async fn new(addr: SocketAddr) -> Result<TcpServer, std::io::Error> {
        let tcp_listener = TcpListener::bind(addr).await?;

        Ok(TcpServer { tcp_listener })
    }

    pub async fn get_new_client(
        &mut self,
    ) -> Result<TcpStream, TcpServerError> {
        match self.tcp_listener.accept().await {
            Ok((socket, addr)) => Ok(TcpStream::new(addr, socket)),
            Err(e) => Err(TcpServerError::IoError(e)),
        }
    }
}