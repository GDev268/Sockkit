use crate::udpstream::UdpStream;
use crate::utils::RawPacket;
use crossbeam::queue::SegQueue;
use dashmap::DashMap;
use flume::{Receiver, Sender, TryRecvError};
use std::cell::RefCell;
use std::net::SocketAddr;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::thread::JoinHandle;
use std::time::Duration;
use tokio::runtime::Handle;
use uflow::SendMode;
use uflow::server::{Config, RemoteClient, Server};

const MAX_PACKET_SIZE: usize = 16 * 1024;
const MIN_THREAD_SLEEP_TIME: Duration = Duration::from_micros(500);
const MAX_THREAD_SLEEP_TIME: Duration = Duration::from_millis(5);

#[derive(Debug, PartialEq)]
pub enum UdpServerError {
    UflowError(uflow::server::ErrorType),
    ChannelClosed,
}

pub struct UdpServer {
    new_clients: Receiver<(SocketAddr, Result<UdpStream, uflow::server::ErrorType>)>,
    disconnected: Arc<AtomicBool>,
    thread_handle: JoinHandle<()>,
}

impl UdpServer {
    pub async fn new(addr: SocketAddr, config: Config) -> UdpServer {
        let (client_tx, client_rx) = flume::unbounded();
        let disconnected = Arc::new(AtomicBool::new(false));
        let disconnected_clone = disconnected.clone();

        let handle = Handle::current();

        let thread_handle = std::thread::spawn(move || {
            Self::listening_loop(
                Server::bind(addr, config).unwrap(),
                client_tx,
                disconnected_clone,
                handle,
            )
        });

        UdpServer {
            new_clients: client_rx,
            disconnected,
            thread_handle,
        }
    }

    fn listening_loop(
        mut server: Server,
        new_clients: Sender<(SocketAddr, Result<UdpStream, uflow::server::ErrorType>)>,
        disconnected: Arc<AtomicBool>,
        handle: tokio::runtime::Handle,
    ) {
        let (packet_tx, packet_rx) = flume::bounded::<(SocketAddr, RawPacket)>(MAX_PACKET_SIZE);

        let clients = DashMap::new();

        let mut sleep_time = MIN_THREAD_SLEEP_TIME;

        while !disconnected.load(Ordering::SeqCst) {
            let mut worked = false;

            for event in server.step() {
                worked = true;

                match event {
                    uflow::server::Event::Connect(addr) => {
                        let (event_tx, stream) =
                            Self::create_new_client(addr, packet_tx.clone(), handle.clone());

                        clients.insert(addr, event_tx);

                        if let Err(e) = new_clients.send((addr, Ok(stream))) {
                            clients.remove(&addr);
                        }
                    }
                    uflow::server::Event::Disconnect(addr) => {
                        if let Some(event_sender) = clients.get(&addr) {
                            let _ = event_sender.send(uflow::client::Event::Disconnect);
                            clients.remove(&addr);
                        }
                    }
                    uflow::server::Event::Receive(addr, buf) => {
                        if let Some(event_sender) = clients.get(&addr) {
                            let _ = event_sender.send(uflow::client::Event::Receive(buf));
                        }
                    }
                    uflow::server::Event::Error(addr, error) => {
                        let e = match error {
                            uflow::server::ErrorType::Config => uflow::client::ErrorType::Config,
                            uflow::server::ErrorType::ServerFull => {
                                uflow::client::ErrorType::ServerFull
                            }
                            uflow::server::ErrorType::Version => uflow::client::ErrorType::Version,
                            uflow::server::ErrorType::Timeout => uflow::client::ErrorType::Timeout,
                        };

                        if let Some(event_sender) = clients.get(&addr) {
                            let _ = event_sender.send(uflow::client::Event::Error(e));
                        }

                        let _ = new_clients.send((addr, Err(error)));
                    }
                }
            }

            while let Ok((addr, packet)) = packet_rx.try_recv() {
                worked = true;

                if let Some(client) = server.client(&addr) {
                    client.borrow_mut().send(
                        packet.payload.to_vec().into_boxed_slice(),
                        packet.channel_id,
                        packet.send_mode,
                    );
                }
            }

            if !worked {
                thread::sleep(sleep_time);
                sleep_time = (sleep_time * 2).min(MAX_THREAD_SLEEP_TIME);
            } else {
                sleep_time = MIN_THREAD_SLEEP_TIME;
            }
        }
    }

    fn create_new_client(
        addr: SocketAddr,
        packet_tx: Sender<(SocketAddr, RawPacket)>,
        handle: Handle,
    ) -> (Sender<uflow::client::Event>, UdpStream) {
        let (event_tx, event_rx) = flume::unbounded();

        (event_tx, UdpStream::new(addr, packet_tx, event_rx, handle))
    }

    pub async fn get_new_client(&mut self) -> (SocketAddr, Result<UdpStream, UdpServerError>) {
        while let Ok((addr, result)) = self.new_clients.recv_async().await {
            return (
                addr,
                result.map_err(UdpServerError::UflowError),
            );
        }

        (
            "0.0.0.0:0".parse().unwrap(),
            Err(UdpServerError::ChannelClosed),
        )
    }

    pub fn is_disconnected(&self) -> bool {
        self.disconnected.load(SeqCst)
    }
}
