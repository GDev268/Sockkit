use std::sync::Arc;
use uflow::SendMode;

#[derive(Debug, Clone)]
pub(crate) struct RawPacket {
    pub(crate) channel_id: usize,
    pub(crate) send_mode: SendMode,
    pub(crate) payload: Arc<[u8]>,
}