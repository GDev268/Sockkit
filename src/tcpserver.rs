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

    pub async fn get_new_client(&mut self) -> Option<TcpStream> {
        match self.tcp_listener.accept().await {
            Ok((socket, addr)) => Some(TcpStream::new(addr, socket)),
            Err(..) => None,
        }
    }
}