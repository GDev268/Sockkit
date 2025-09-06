use crate::server::Server;
use crate::tcpstream::TcpStream;
use std::net::SocketAddr;
use tokio::net::TcpListener;

pub(crate) struct TcpServer {
    pub(crate) tcp_listener: TcpListener,
}

impl TcpServer {
    pub(crate) async fn new(addr: SocketAddr) -> Result<TcpServer, std::io::Error> {
        let tcp_listener = TcpListener::bind(addr).await?;

        Ok(TcpServer { tcp_listener })
    }
}

impl Server for TcpServer {
    type StreamType = TcpStream;

    async fn new_client(&mut self) -> Result<Self::StreamType, std::io::Error> {
        match self.tcp_listener.accept().await {
            Ok((socket, addr)) => Ok(TcpStream::new(addr, socket)),
            Err(e) => Err(e),
        }
    }
}
