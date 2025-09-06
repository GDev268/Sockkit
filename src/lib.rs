#![feature(inherent_str_constructors)]

use bytes::{Bytes, BytesMut};

pub(crate) mod utils;
pub mod tcpclient;
pub mod tcpstream;
pub mod tcpserver;
pub mod udpclient;
mod bytechannel;
pub mod udpstream;
mod backpressurebuffer;
pub mod udpserver;
mod errors;

#[derive(Debug)]
pub enum ReadError {
    NotEnoughBytes { needed: usize, found: usize },
    InvalidPacket,
    PacketAheadOfTime
}

pub(crate) trait Reader: Sized {
    fn read_bytes(bytes: Bytes) -> Result<Self, ReadError>;
}

pub(crate) trait Writer {
    fn write_bytes(&self,bytes: &mut BytesMut);
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;
    use std::thread;
    use bytes::Bytes;
    use tokio::time::{timeout, Duration};
    use crate::tcpclient::TcpClient;
    use crate::tcpserver::TcpServer;
    use std::net::{IpAddr, Ipv4Addr};
    use uflow::SendMode;
    use uflow::server::Config;
    use crate::udpclient::UdpClient;
    use crate::udpserver::UdpServer;
    use crate::udpstream::UdpStream;

    #[tokio::test]
    async fn tcp_test_client_server_connection() {
        // Pick an ephemeral port
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();

        // Start server
        let mut server = TcpServer::new(addr).await.unwrap();
        let server_addr = server.tcp_listener.local_addr().unwrap();

        // Spawn server task to accept one client and echo back
        let server_task = tokio::spawn(async move {
            let mut stream = server.get_new_client().await.unwrap();

            let received = stream.recv().await.unwrap();
            assert_eq!(received, "hello".as_bytes());

            stream.send(Bytes::from("world")).await.unwrap();
        });

        // Start client
        let mut client = TcpClient::new(server_addr).await.unwrap();

        // Send "hello"
        client.send(Bytes::from("hello")).await.unwrap();

        // Read "world"
        let response = client.recv().await.unwrap();
        assert_eq!(response, "world".as_bytes());

        // Ensure server finishes
        timeout(Duration::from_secs(1), server_task).await.unwrap().unwrap();
    }


    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn tcp_test_multi_client_server() {
        const CLIENTS: usize = 10;

        // Bind ephemeral server
        let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut server = TcpServer::new(addr).await.unwrap();
        let server_addr = server.tcp_listener.local_addr().unwrap();
        println!("[MAIN] server listening on {server_addr}");

        // Spawn server task handling multiple clients concurrently
        let server_task = tokio::spawn(async move {
            for id in 0..CLIENTS {
                println!("[SERVER] waiting for client {id}");
                let mut stream = server.get_new_client().await.unwrap();
                println!("[SERVER] client {id} connected");

                tokio::spawn(async move {
                    loop {
                        match stream.recv().await {
                            Ok(msg) => {
                                if msg.is_empty() {
                                    println!("[SERVER] client {id} sent empty buffer, closing");
                                    break;
                                }
                                let text = String::from_utf8_lossy(&msg);
                                println!("[SERVER] received from client {id}: {text}");
                                if let Err(e) = stream.send(Bytes::from(msg)).await {
                                    println!("[SERVER] failed to send to client {id}: {e:?}");
                                } else {
                                    println!("[SERVER] echoed back to client {id}");
                                }
                            }
                            Err(e) => {
                                println!("[SERVER] recv error from client {id}: {e:?}");
                                break;
                            }
                        }
                    }
                    println!("[SERVER] connection with client {id} closed");
                });
            }
        });

        // Give server a moment to bind
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Spawn many client threads
        let mut handles = Vec::new();
        for i in 0..CLIENTS {
            let server_addr = server_addr.clone();
            let handle = thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    println!("[CLIENT {i}] connecting to {server_addr}");
                    let mut client = TcpClient::new(server_addr).await.unwrap();
                    println!("[CLIENT {i}] connected");

                    let msg = format!("hello from client {i}");
                    println!("[CLIENT {i}] sending: {msg}");
                    client.send(Bytes::from(msg.clone())).await.unwrap();

                    let resp = client.recv().await.unwrap();
                    println!(
                        "[CLIENT {i}] received echo: {}",
                        String::from_utf8_lossy(&resp)
                    );
                    assert_eq!(resp, msg.as_bytes());

                    println!("[CLIENT {i}] done");
                });
            });
            handles.push(handle);
        }

        // Wait for clients
        for handle in handles {
            handle.join().unwrap();
        }
        println!("[MAIN] all clients finished");

        // Ensure server finishes
        timeout(Duration::from_secs(2), server_task).await.unwrap().unwrap();
        println!("[MAIN] server task finished");
    }



    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn udp_test_client_server_connection() {
        // 1. Start a server on localhost
        let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20225);
        let server_config = Config {
            max_total_connections: 10,
            max_active_connections: 10,
            enable_handshake_errors: true,
            endpoint_config: Default::default(),
        };

        let mut server = UdpServer::new(server_addr, server_config).await;

        // 2. Spawn a client connecting to the server
        let client_config = uflow::client::Config::default();

        let mut client = UdpClient::new(server_addr, client_config).await.unwrap();


        // 3. Retrieve the new client from the server
        let mut server_stream: UdpStream = loop {
            if let Some(client) = server.get_new_client() {
                break client;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };

        // 4. Client sends a reliable packet
        let payload = Bytes::from_static(b"hello server");
        client.send(payload.clone(), 1, SendMode::Reliable).await.unwrap();

        // 5. Server should receive the packet via its UdpStream
        let received = timeout(Duration::from_millis(100), server_stream.recv())
            .await
            .expect("Timed out waiting for server to receive")
            .expect("Failed to read packet");

        assert_eq!(&received[..], &payload[..]);

        // 6. Server sends a reply back
        let reply_payload = Bytes::from_static(b"hello client");
        server_stream.send(reply_payload.clone(), 2, SendMode::Reliable).await.unwrap();

        // 7. Client receives the reply
        let client_received = timeout(Duration::from_millis(100), client.recv())
            .await
            .expect("Timed out waiting for client to receive")
            .expect("Failed to read packet");

        println!("Received: {:?} from replyed: {:?}", &client_received[..], &reply_payload[..]);

        assert_eq!(&client_received[..], &reply_payload[..]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn udp_test_multi_client_server() {
        use std::net::{IpAddr, Ipv4Addr, SocketAddr};
        use tokio::time::{sleep, timeout, Duration};
        use bytes::Bytes;

        const NUM_CLIENTS: usize = 10;

        // 1. Start server
        let server_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 20225);
        let server_config = Config {
            max_total_connections: NUM_CLIENTS,
            max_active_connections: NUM_CLIENTS,
            enable_handshake_errors: true,
            endpoint_config: Default::default(),
        };

        let mut server = UdpServer::new(server_addr, server_config).await;

        // 2. Spawn clients concurrently
        let mut client_handles = Vec::new();

        for i in 0..NUM_CLIENTS {
            let server_addr = server_addr.clone();
            client_handles.push(tokio::spawn(async move {
                let mut client = UdpClient::new(server_addr, uflow::client::Config::default())
                    .await
                    .expect("Failed to create UDP client");

                // Each client sends a message
                let msg = format!("hello from client {}", i);
                client.send(Bytes::from(msg.clone()), 1, SendMode::Reliable)
                    .await
                    .expect("Failed to send message");

                // Client receives echo back
                let response = timeout(Duration::from_secs(1), client.recv())
                    .await
                    .expect("Timed out waiting for server reply")
                    .expect("Failed to receive from server");

                assert_eq!(response, msg.as_bytes());
                println!("[CLIENT {i}] got echo: {:?}", String::from_utf8_lossy(&response));
            }));
        }

        // 3. Server accepts clients and echoes back
        let mut server_streams = Vec::new();
        while server_streams.len() < NUM_CLIENTS {
            if let Some(stream) = server.get_new_client() {
                server_streams.push(stream);
            } else {
                sleep(Duration::from_millis(10)).await;
            }
        }

        // 4. Spawn server tasks to echo messages from each client
        let mut server_tasks = Vec::new();
        for (id, mut stream) in server_streams.into_iter().enumerate() {
            server_tasks.push(tokio::spawn(async move {
                loop {
                    match timeout(Duration::from_secs(1), stream.recv()).await {
                        Ok(Ok(msg)) => {
                            if msg.is_empty() { break; }
                            // Echo back
                            stream.send(Bytes::from(msg.clone()), 1, SendMode::Reliable)
                                .await
                                .expect("Failed to send reply");
                            println!("[SERVER STREAM] echoed back to client {}", id);
                        }
                        _ => break,
                    }
                }
                println!("[SERVER] client {} done", id);
            }));
        }

        // 5. Wait for all clients
        for handle in client_handles {
            handle.await.unwrap();
        }

        // 6. Wait for all server tasks
        for task in server_tasks {
            task.await.unwrap();
        }

        println!("[MAIN] multi-client UDP test completed");
        assert!(true)
    }
}
