#[derive(Debug)]
pub(crate) enum ReadError {
    EmptyBuffer,
    FailedToRead(std::io::Error)
}

#[derive(Debug)]
pub(crate) enum WriteError {
    EmptyBuffer,
    FailedToWrite(std::io::Error)
}
