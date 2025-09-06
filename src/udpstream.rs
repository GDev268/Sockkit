use crate::bytechannel::{ByteMode, ByteReader, ByteWriter, byte_channel};
use crate::client::{RawPacket, ReadError, WriteError};
use bytes::{Buf, BufMut, Bytes, BytesMut};
use crossbeam::queue::SegQueue;
use flume::{Receiver, Sender};
use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;
use uflow::client::{ErrorType, Event};
use uflow::SendMode;
use crate::backpressurebuffer::BackpressureBuffer;
use crate::udpclient::UdpClient;

const BUFFER_START_CAPACITY: usize = 1024;
const THREAD_SLEEP_TIME: Duration = Duration::from_millis(1);
const MAX_PAYLOAD_SIZE: usize = 16 * 1024;
const MAX_BACKPRESSURE: usize = 4096;
const HEADER_DATA_SIZE: usize = 6;
const PAYLOAD_DATA_SIZE: usize = 4;

#[derive(Debug)]
pub(crate) struct UdpStream {
    packets_in_recv: ByteReader,
    packets_out_send: ByteWriter,
    send_buf: Arc<SegQueue<BytesMut>>,
    disconnected: Arc<AtomicBool>,
    error_rx: Receiver<ErrorType>,

}

impl UdpStream {
    pub(crate) fn new(
        addr: SocketAddr,
        stream_tx: Sender<(SocketAddr,RawPacket)>,
        stream_rx: Receiver<Event>,
        handle: tokio::runtime::Handle
    ) -> Self {
        let send_buf = Arc::new(SegQueue::new());
        send_buf.push(BytesMut::with_capacity(BUFFER_START_CAPACITY));

        let (incoming_send, incoming_recv) = byte_channel(ByteMode::Restricted(1024));
        let (outgoing_send, outgoing_recv) = byte_channel(ByteMode::Restricted(1024));

        let disconnected = Arc::new(AtomicBool::new(false));
        let disconnected_clone = disconnected.clone();

        let (error_tx, error_rx) = flume::bounded(1);

        handle.spawn(async move {
            Self::listening_loop(
                addr,
                stream_tx,
                stream_rx,
                incoming_send,
                outgoing_recv,
                disconnected_clone,
                error_tx,
            )
            .await
        });

        Self {
            packets_in_recv: incoming_recv,
            packets_out_send: outgoing_send,
            send_buf,
            disconnected,
            error_rx,
        }
    }

    async fn listening_loop(
        addr: SocketAddr,
        stream_tx: Sender<(SocketAddr,RawPacket)>,
        stream_rx: Receiver<Event>,
        mut packet_in_sender: ByteWriter,
        packet_out_recv: ByteReader,
        disconnect_flag: Arc<AtomicBool>,
        error_tx: Sender<ErrorType>,
    ) {
        let mut backpressure_buffer: BackpressureBuffer<RawPacket> = BackpressureBuffer::new(MAX_BACKPRESSURE);

        while !disconnect_flag.load(Ordering::SeqCst) {
            tokio::select! {
                Ok(event) = stream_rx.recv_async() => {
                    match event {
                        Event::Disconnect => {
                            disconnect_flag.store(true, Ordering::Relaxed);
                        }
                        Event::Error(error) => {
                            if error == ErrorType::Version || error == ErrorType::Config {
                                let _ = error_tx.try_send(error);
                                disconnect_flag.store(true, Ordering::Relaxed);
                            } else {
                                let _ = error_tx.try_send(error);
                            }
                        }
                        Event::Receive(packet_data) => {
                            let bytes = Bytes::copy_from_slice(&packet_data[..]);
                            let _ = packet_in_sender.write_async(BytesMut::from(bytes)).await;
                        }
                        _ => {}
                    }
                },
                Ok(mut bytes) = packet_out_recv.read_async() => {
                    while bytes.has_remaining() {
                        if bytes.len() < HEADER_DATA_SIZE { break; }

                        let payload_size = u32::from_be_bytes([bytes[2], bytes[3], bytes[4], bytes[5]]) as usize;

                        if payload_size > MAX_PAYLOAD_SIZE || bytes.len() < HEADER_DATA_SIZE + payload_size { break; }

                        let channel_id = bytes.get_u8() as usize;
                        let send_mode = match bytes.get_u8() {
                            0 => SendMode::TimeSensitive,
                            1 => SendMode::Unreliable,
                            2 => SendMode::Persistent,
                            3 => SendMode::Reliable,
                            _ => continue,
                        };

                        bytes.advance(PAYLOAD_DATA_SIZE);

                        let payload: Arc<[u8]> = Arc::from(bytes.split_to(payload_size).to_vec());

                        let packet = RawPacket { channel_id, send_mode, payload: payload.clone() };

                        match send_mode {
                            SendMode::Reliable | SendMode::Persistent => {
                                if stream_tx.try_send((addr,packet.clone())).is_err() {
                                    backpressure_buffer.push(packet);
                                }
                            },
                            SendMode::TimeSensitive | SendMode::Unreliable => {
                                if stream_tx.send((addr,packet)).is_err() {
                                    continue;
                                };
                            },
                        }
                    }
                }
            }
        }

        while let Some(packet) = backpressure_buffer.pop() {
            let _ = stream_tx.send((addr,packet));
        }
    }

    pub(crate) async fn recv(&mut self) -> Result<BytesMut, ReadError> {
        match self.packets_in_recv.read_async().await {
            Ok(bytes) => Ok(bytes),
            Err(crate::bytechannel::ReadError::Empty) => Err(ReadError::EmptyBuffer),
            Err(crate::bytechannel::ReadError::Disconnected) => Err(ReadError::FailedToRead(
                std::io::Error::new(std::io::ErrorKind::ConnectionAborted, "disconnected"),
            )),
        }
    }

    pub(crate) async fn send(
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

        // Send
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

        // Return buffer to queue for reuse
        self.send_buf.push(buffer);

        result
    }

    fn encode_send_mode(mode: SendMode) -> u8 {
        match mode {
            SendMode::TimeSensitive => 0,
            SendMode::Unreliable => 1,
            SendMode::Persistent => 2,
            SendMode::Reliable => 3,
        }
    }
}

impl Drop for UdpStream {
    fn drop(&mut self) {
        self.disconnected.store(true, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};
    use super::*;
    use tokio::time::{timeout, Duration};
    use bytes::Bytes;
    use flume::unbounded;
    use tokio::runtime;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_udpstream_send_and_receive() {
        // Setup dummy channels
        let (stream_tx, stream_rx) = unbounded::<(SocketAddr,RawPacket)>();
        let (event_tx, event_rx) = unbounded::<Event>();

        let handle = runtime::Handle::current();

        // Create UdpStream
        let mut udp = UdpStream::new(SocketAddr::new(IpAddr::V4(Ipv4Addr::new(0,0,0,0)),324), stream_tx.clone(), event_rx,handle);

        // Send a packet
        let payload = Bytes::from_static(b"hello");
        udp.send(payload.clone(), 1, SendMode::Reliable).await.unwrap();

        // Simulate receiving the same packet back via the internal channel
        let mut buf = BytesMut::with_capacity(HEADER_DATA_SIZE + payload.len());
        buf.put_u8(1); // channel_id
        buf.put_u8(UdpStream::encode_send_mode(SendMode::Reliable));
        buf.put_u32(payload.len() as u32);
        buf.put_slice(&*payload);

        // Write it to the packets_in_recv so we can read it
        event_tx.send(Event::Receive(payload.to_vec().into_boxed_slice())).unwrap();

        // Try to receive it
        let received = timeout(Duration::from_millis(50), udp.packets_in_recv.read_async())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(&received[..], &payload[..]);
    }
}