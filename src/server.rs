pub(crate) trait Server {
    type StreamType;

    async fn new_client(&mut self) -> Result<Self::StreamType,std::io::Error>;
}