use std::fmt;
use std::sync::{Arc, Mutex};
use bytes::BytesMut;
use tokio::sync::Notify;

#[derive(Debug, Clone,  PartialEq, Eq)]
pub(crate) enum ByteMode {
    Restricted(usize),
    Unrestricted,
}

pub(crate) fn byte_channel(byte_mode: ByteMode) -> (ByteWriter, ByteReader) {
    let shared = Arc::new(SharedArray {
        array: Mutex::new(ByteArray {
            bytes: BytesMut::new(),
            disconnected: false,
        }),
        notify: Notify::new(),
        byte_mode
    });

    (ByteWriter(shared.clone()), ByteReader(shared))
}

#[derive(Debug)]
struct ByteArray {
    bytes: BytesMut,
    disconnected: bool,
}

#[derive(Debug)]
struct SharedArray {
    array: Mutex<ByteArray>,
    notify: Notify,
    byte_mode: ByteMode
}

#[derive(Clone, Debug)]
pub(crate) struct ByteReader(Arc<SharedArray>);

impl ByteReader {
    pub(crate) fn read_sync(&self) -> Result<BytesMut, ReadError> {
        let mut lock = self.0.array.lock().unwrap();

        if lock.disconnected {
            return Err(ReadError::Disconnected);
        }

        if !lock.bytes.is_empty() {
            self.0.notify.notify_waiters();
            return Ok(lock.bytes.split());
        }

        Err(ReadError::Empty)
    }

    pub(crate) async fn read_async(&self) -> Result<BytesMut, ReadError> {
        loop {
            {
                let mut lock = self.0.array.lock().unwrap();

                if lock.disconnected {
                    return Err(ReadError::Disconnected)
                }

                if !lock.bytes.is_empty() {
                    self.0.notify.notify_waiters();
                    return Ok(lock.bytes.split());
                }
            }

            self.0.notify.notified().await;
        }
    }

    pub(crate) fn is_disconnected(&self) -> bool {
        self.0.array.lock().unwrap().disconnected
    }

    pub(crate) fn disconnect(&mut self) {
        let mut lock = self.0.array.lock().unwrap();
        lock.disconnected = true;
        self.0.notify.notify_waiters();
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ByteWriter(Arc<SharedArray>);

impl ByteWriter {
    pub(crate) fn take_capacity(&mut self,additional_data: usize) -> BytesMut {
        let mut lock = self.0.array.lock().unwrap();

        lock.bytes.reserve(additional_data);

        let len = lock.bytes.len();
        lock.bytes.split_off(len)
    }
    pub(crate) fn write_sync(&mut self, mut data: BytesMut) -> Result<(), WriteError> {
        let mut lock = self.0.array.lock().unwrap();

        if lock.disconnected {
            return Err(WriteError::Disconnected(data));
        }

        if data.is_empty() {
            return Ok(());
        }

        match self.0.byte_mode {
            ByteMode::Unrestricted => {
                lock.bytes.unsplit(data);
                self.0.notify.notify_waiters();
                return Ok(());
            },
            ByteMode::Restricted(limit) => {
                let available = limit - lock.bytes.len();

                if available < data.len() {
                    if available > 0 {
                        lock.bytes.unsplit(data.split_to(available));
                        self.0.notify.notify_waiters();
                    }

                    return Err(WriteError::Full(data));
                }

                lock.bytes.unsplit(data);
                self.0.notify.notify_waiters();

                Ok(())
            }
        }
    }

    pub(crate) async fn write_async(&mut self, mut data: BytesMut) -> Result<(), WriteError> {
        loop {
            {
                let mut lock = self.0.array.lock().unwrap();

                if lock.disconnected {
                    return Err(WriteError::Disconnected(data));
                }

                if data.is_empty() {
                    return Ok(());
                }

                match self.0.byte_mode {
                    ByteMode::Unrestricted => {
                        lock.bytes.unsplit(data);
                        self.0.notify.notify_waiters();
                        return Ok(());
                    },
                    ByteMode::Restricted(limit) => {
                        let available = limit - lock.bytes.len();

                        if available >= data.len() {
                            lock.bytes.unsplit(data);
                            self.0.notify.notify_waiters();
                            return Ok(());
                        }

                        if available > 0 {
                            lock.bytes.unsplit(data.split_to(available));
                            self.0.notify.notify_waiters();
                        }
                    }
                }
            }

            self.0.notify.notified().await;
        }
    }

    pub(crate) fn is_disconnected(&self) -> bool {
        self.0.array.lock().unwrap().disconnected
    }

    pub(crate) fn disconnect(&mut self) {
        let mut lock = self.0.array.lock().unwrap();
        lock.disconnected = true;
        self.0.notify.notify_waiters();
    }
}

impl Drop for ByteReader {
    fn drop(&mut self) {
        self.0.array.lock().unwrap().disconnected = true;
    }
}

impl Drop for ByteWriter {
    fn drop(&mut self) {
        self.0.array.lock().unwrap().disconnected = true;
    }
}

#[derive(Debug, Clone,  PartialEq, Eq)]
pub(crate) enum WriteError {
    Disconnected(BytesMut),
    Full(BytesMut),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteError::Disconnected(BytesMut) => write!(f,"Byte Channel Disconnected!"),
            WriteError::Full(BytesMut) => write!(f,"Byte Channel is Full!"),
        }
    }
}

#[derive(Debug, Clone,  PartialEq, Eq)]
pub(crate) enum ReadError {
    Disconnected,
    Empty,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::Disconnected => write!(f,"Byte Channel Disconnected!"),
            ReadError::Empty => write!(f,"Byte Channel is Empty!"),
        }
    }
}

#[cfg(test)]

mod tests {
    use super::*;

    #[test]
    fn byte_channel_try() {
        let (mut sender, mut receiver) = byte_channel(ByteMode::Restricted(4));

        assert_eq!(
            sender.write_sync("hello".as_bytes().into()),
            Err(WriteError::Full("o".as_bytes().into()))
        );

        assert_eq!(
            receiver.read_sync().unwrap(),
            BytesMut::from("hell".as_bytes())
        );
    }

    #[tokio::test]
    async fn byte_channel_async() {
        let (mut sender, mut receiver) = byte_channel(ByteMode::Restricted(4));

        let t = tokio::spawn(async move {
            let bytes = receiver.read_async().await.unwrap();
            assert_eq!(&bytes[..], b"hell");
            let bytes = receiver.read_async().await.unwrap();
            assert_eq!(&bytes[..], b"o");

            assert_eq!(receiver.read_sync(), Err(ReadError::Empty));
        });

        sender.write_async("hello".as_bytes().into()).await.unwrap();

        t.await.unwrap();

        assert!(sender.is_disconnected());
    }

    #[tokio::test]
    async fn concurrent_read_write() {
        let (mut writer, mut reader) = byte_channel(ByteMode::Restricted(10));

        let write_task = tokio::spawn(async move {
            writer.write_async(BytesMut::from("abc")).await?;
            Ok::<(), WriteError>(())
        });

        let read_task = tokio::spawn(async move {
            let data = reader.read_async().await?;
            assert_eq!(data, BytesMut::from("abc"));
            Ok::<(), ReadError>(())
        });

        match write_task.await {
            Ok(Ok(())) => {},
            Ok(Err(WriteError::Disconnected(_))) => {},
            other => panic!("Unexpected write_task result: {:?}", other),
        }

        match read_task.await {
            Ok(Ok(())) => {},
            Ok(Err(ReadError::Disconnected)) => {},
            other => panic!("Unexpected read_task result: {:?}", other),
        }
    }


}