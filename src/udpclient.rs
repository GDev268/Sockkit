use crate::bytechannel::{ByteMode, ByteReader, ByteWriter, byte_channel};
use crate::utils::{RawPacket, ReadError, WriteError};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use flume::{Receiver, Sender};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::thread::JoinHandle as SyncJoinHandle;
use std::time::Duration;
use crossbeam::queue::SegQueue;
use tokio::task::JoinHandle as AsyncJoinHandle;
use uflow::SendMode;
use uflow::client::{Client as UflowClient, ErrorType};
use crate::backpressurebuffer::BackpressureBuffer;

const MAX_CHANNEL_BOUND_SIZE: usize = 1024;
const BUFFER_START_CAPACITY: usize = 1024;
const MIN_THREAD_SLEEP_TIME: Duration = Duration::from_micros(500);
const MAX_THREAD_SLEEP_TIME: Duration = Duration::from_millis(5);
const MAX_PAYLOAD_SIZE: usize = 16 * 1024;
const MAX_BACKPRESSURE: usize = 4096;
const HEADER_DATA_SIZE: usize = 6;
const PAYLOAD_DATA_SIZE:usize = 4;


pub struct UdpClient {
    packets_in_recv: ByteReader,
    packets_out_send: ByteWriter,
    send_buf: Arc<SegQueue<BytesMut>>,
    disconnected: Arc<AtomicBool>,
    error_rx: Receiver<ErrorType>,
    blocking_handle: Option<SyncJoinHandle<()>>,
}

impl UdpClient {
    pub async fn new(
        server_addr: SocketAddr,
        config: uflow::client::Config,
    ) -> Result<Self, std::io::Error> {
        let stream = UflowClient::connect(server_addr, config)?;

        let send_buf = Arc::new(SegQueue::new());
        send_buf.push(BytesMut::with_capacity(BUFFER_START_CAPACITY));

        let (incoming_send, incoming_recv) = byte_channel(ByteMode::Restricted(MAX_CHANNEL_BOUND_SIZE));
        let (outgoing_send, outgoing_recv) = byte_channel(ByteMode::Restricted(MAX_CHANNEL_BOUND_SIZE));

        let disconnected = Arc::new(AtomicBool::new(false));
        let disconnected_clone = disconnected.clone();

        let (error_tx, error_rx) = flume::bounded(1);

        let (handle_tx, handle_rx) = flume::bounded(1);

        tokio::spawn(async move {
            Self::listening_loop(
                stream,
                incoming_send,
                outgoing_recv,
                disconnected_clone,
                error_tx,
                handle_tx
            ).await
        });

        Ok(Self {
            packets_in_recv: incoming_recv,
            packets_out_send: outgoing_send,
            send_buf,
            disconnected,
            error_rx,
            blocking_handle: handle_rx.try_recv().ok(),
        })
    }

    async fn listening_loop(
        mut stream: UflowClient,
        mut packet_in_sender: ByteWriter,
        packet_out_recv: ByteReader,
        disconnect_flag: Arc<AtomicBool>,
        error_tx: Sender<ErrorType>,
        handle_tx: Sender<SyncJoinHandle<()>>,
    ) {
        let (event_tx, event_rx) = flume::unbounded();
        let (send_tx, send_rx) = flume::bounded::<RawPacket>(MAX_CHANNEL_BOUND_SIZE);
        let disconnected = disconnect_flag.clone();

        let handle = Self::spawn_blocking_send_thread(stream, send_rx, event_tx.clone(), disconnected.clone());
        let _ = handle_tx.send(handle);

        let mut backpressure_buffer: BackpressureBuffer<RawPacket> = BackpressureBuffer::new(MAX_BACKPRESSURE);

        while !disconnect_flag.load(SeqCst) {
            while let Some(packet) = backpressure_buffer.pop() {
                if send_tx.try_send(packet.clone()).is_err() {
                    backpressure_buffer.push(packet);
                    break;
                }
            }

            tokio::select! {
                Ok(event) = event_rx.recv_async() => {
                    match event {
                        uflow::client::Event::Disconnect => {
                            disconnect_flag.store(true, Ordering::Relaxed);
                        }
                        uflow::client::Event::Error(error) => {
                            if error == ErrorType::Version || error == ErrorType::Config {
                                let _ = error_tx.try_send(error);
                                disconnect_flag.store(true, Ordering::Relaxed);
                            } else {
                                let _ = error_tx.try_send(error);
                            }
                        }
                        uflow::client::Event::Receive(packet_data) => {
                            let bytes = Bytes::copy_from_slice(&packet_data[..]);
                            let _ = packet_in_sender.write_async(BytesMut::from(bytes)).await;
                        }
                        _ => {}
                    }
                },
                Ok(mut bytes) = packet_out_recv.read_async() => {
                    while let Some(packet) = Self::decode_packet(&mut bytes) {
                        match packet.send_mode {
                            SendMode::Reliable | SendMode::Persistent => {
                                if send_tx.try_send(packet.clone()).is_err() {
                                    backpressure_buffer.push(packet);
                                }
                            },
                            SendMode::TimeSensitive | SendMode::Unreliable => {
                                if send_tx.send(packet).is_err() {
                                    continue;
                                };
                            },
                        }
                    }
                }
            }
        }

        while let Some(packet) = backpressure_buffer.pop() {
            let _ = send_tx.send(packet);
        }
    }


    fn spawn_blocking_send_thread(
        mut stream: UflowClient,
        send_rx: flume::Receiver<RawPacket>,
        event_tx: flume::Sender<uflow::client::Event>,
        disconnected: Arc<AtomicBool>,
    ) -> SyncJoinHandle<()> {
        thread::spawn(move || {
            let mut sleep_time = MIN_THREAD_SLEEP_TIME;

            while !disconnected.load(SeqCst) {
                let mut worked = false;

                for event in stream.step() {
                    worked = true;
                    let _ = event_tx.send(event);
                }

                while let Ok(packet) = send_rx.try_recv() {
                    worked = true;
                    stream.send(packet.payload.to_vec().into_boxed_slice(), packet.channel_id, packet.send_mode);
                }

                if !worked {
                    thread::sleep(sleep_time);
                    sleep_time = (sleep_time * 2).min(MAX_THREAD_SLEEP_TIME);
                } else {
                    sleep_time = MIN_THREAD_SLEEP_TIME;
                }
            }
        })
    }

    fn decode_packet(bytes: &mut BytesMut) -> Option<RawPacket> {
        if bytes.len() < HEADER_DATA_SIZE { return None; }

        let payload_size = Self::read_payload_size_header(bytes);

        if payload_size > MAX_PAYLOAD_SIZE || bytes.len() < HEADER_DATA_SIZE + payload_size { return None; }

        let channel_id = bytes.get_u8() as usize;

        let send_mode = match bytes.get_u8() {
            0 => SendMode::TimeSensitive,
            1 => SendMode::Unreliable,
            2 => SendMode::Persistent,
            3 => SendMode::Reliable,
            _ => return None,
        };

        bytes.advance(PAYLOAD_DATA_SIZE);

        let payload: Arc<[u8]> = Arc::from(bytes.split_to(payload_size).to_vec());

        Some(RawPacket { channel_id, send_mode, payload })
    }

    pub async fn recv(&mut self) -> Result<BytesMut, ReadError> {
        match self.packets_in_recv.read_async().await {
            Ok(bytes) => Ok(bytes),
            Err(crate::bytechannel::ReadError::Empty) => Err(ReadError::EmptyBuffer),
            Err(crate::bytechannel::ReadError::Disconnected) => Err(ReadError::FailedToRead(
                std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "disconnected"),
            )),
        }
    }

    pub async fn send(
        &mut self,
        bytes: Bytes,
        channel_id: u8,
        send_mode: SendMode,
    ) -> Result<(), WriteError> {
        let mut buffer = self.send_buf.pop().unwrap_or_else(|| BytesMut::with_capacity(HEADER_DATA_SIZE + bytes.len()));
        buffer.clear();
        buffer.reserve(6 + bytes.len());
        buffer.put_u8(channel_id);
        buffer.put_u8(Self::encode_send_mode(send_mode));
        buffer.put_u32(bytes.len() as u32);
        buffer.put_slice(&*bytes);

        let result = match self.packets_out_send.write_async(buffer.clone()).await {
            Ok(_) => Ok(()),
            Err(crate::bytechannel::WriteError::Full(..)) => Err(WriteError::FailedToWrite(std::io::Error::new(
                std::io::ErrorKind::StorageFull,
                "full",
            ))),
            Err(crate::bytechannel::WriteError::Disconnected(..)) => Err(WriteError::FailedToWrite(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "disconnected",
            ))),
        };

        self.send_buf.push(buffer);

        result
    }

    pub(crate) fn is_disconnected(&self) -> bool {
        self.disconnected.load(SeqCst)
    }

    fn encode_send_mode(mode: SendMode) -> u8 {
        match mode {
            SendMode::TimeSensitive => 0,
            SendMode::Unreliable => 1,
            SendMode::Persistent => 2,
            SendMode::Reliable => 3,
        }
    }

    fn read_payload_size_header(bytes: &mut BytesMut) -> usize {
        return u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;
    }

}

impl Drop for UdpClient {
    fn drop(&mut self) {
        self.disconnected.store(true, Ordering::SeqCst);

        if let Some(handle) = self.blocking_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::net::SocketAddr;
    use tokio::time::{Duration, sleep, timeout};
    use uflow::SendMode;

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn test_udpclient_echo() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let addr: SocketAddr = "127.0.0.1:9002".parse().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = running.clone();

        // Start server
        let server_handle = std::thread::spawn(move || {
            let mut server =
                uflow::server::Server::bind(addr, uflow::server::Config::default()).unwrap();
            while running_clone.load(Ordering::Relaxed) {
                for event in server.step() {
                    match event {
                        uflow::server::Event::Connect(client_id) => {
                            println!("Client {} connected", client_id);
                        }
                        uflow::server::Event::Receive(client_id, data) => {
                            println!("Server received: {:?}", data);
                            if let Some(client) = server.client(&client_id) {
                                let mut remote = client.borrow_mut();
                                let _ = remote.send(data, 0, SendMode::Reliable);
                            }
                        }
                        _ => {}
                    }
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            println!("Server thread exiting");
        });

        // Give server a moment to start
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Connect client
        let mut client = UdpClient::new(addr, uflow::client::Config::default()).await.unwrap();

        // Send a test message
        let msg = Bytes::from("Hello, test server!");
        client
            .send(msg.clone(), 0, SendMode::Reliable)
            .await
            .unwrap();

        // Receive echo
        let echoed = timeout(Duration::from_secs(5), client.recv())
            .await
            .expect("timeout")
            .unwrap();

        // Stop server
        client.disconnected.store(true, Ordering::Relaxed);
        running.store(false, Ordering::Relaxed);
        server_handle.join().unwrap(); // wait for server thread to exit

        println!("Client received echo: {:?}", echoed);
        assert_eq!(echoed, BytesMut::from(&msg[..]));
    }
}
