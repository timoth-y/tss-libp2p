use crate::peerset::Peerset;

use futures::channel::oneshot;
use mpc_p2p::RoomId;

pub struct IncomingMessage {
    /// Index of party who sent the message.
    pub from: u16,

    /// Message sent by the remote.
    pub body: Vec<u8>,

    pub to: MessageRouting,
}

pub struct OutgoingMessage {
    /// Message sent by the remote.
    pub body: Vec<u8>,

    pub to: MessageRouting,

    pub sent: Option<oneshot::Sender<()>>,
}

#[derive(Copy, Clone, Debug)]
pub enum MessageRouting {
    Broadcast,
    PointToPoint(u16),
}

pub trait ProtocolAgentFactory {
    fn make(&self, protocol_id: u64) -> crate::Result<Box<dyn ComputeAgentAsync>>;
}

#[async_trait::async_trait]
pub trait ComputeAgentAsync: Send + Sync {
    fn session_id(&self) -> u64;

    fn protocol_id(&self) -> u64;

    async fn compute(
        self: Box<Self>,
        parties: Peerset,
        args: Vec<u8>,
        incoming: async_channel::Receiver<IncomingMessage>,
        outgoing: async_channel::Sender<OutgoingMessage>,
    ) -> anyhow::Result<Vec<u8>>;
}

pub trait PeersetCacher {
    fn read_peerset(&self, room_id: &RoomId) -> anyhow::Result<Peerset>;

    fn write_peerset(&mut self, room_id: &RoomId, peerset: Peerset) -> anyhow::Result<()>;
}
