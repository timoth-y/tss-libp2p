use futures::channel::{mpsc, oneshot};
use futures_util::{SinkExt, StreamExt};
use itertools::Itertools;
use libp2p::PeerId;
use log::{info, warn};

use std::io::{BufReader, Read};
use std::ops::Index;

#[derive(Clone)]
pub struct Peerset {
    local_peer_id: PeerId,
    session_peers: Vec<PeerId>,
    pub parties_indexes: Vec<usize>,
    to_runtime: mpsc::Sender<PeersetMsg>,
}

pub(crate) enum PeersetMsg {
    ReadFromCache(oneshot::Sender<anyhow::Result<Peerset>>),
    WriteToCache(Peerset, oneshot::Sender<anyhow::Result<()>>),
}

impl Peerset {
    pub(crate) fn new(
        peers: impl Iterator<Item = PeerId>,
        local_peer_id: PeerId,
    ) -> (Self, mpsc::Receiver<PeersetMsg>) {
        let (tx, rx) = mpsc::channel(1);
        let peers: Vec<_> = peers.sorted_by_key(|p| p.to_bytes()).collect();

        (
            Self {
                local_peer_id,
                parties_indexes: (0..peers.len()).collect(),
                session_peers: peers,
                to_runtime: tx,
            },
            rx,
        )
    }

    pub(crate) fn from_bytes(
        bytes: &[u8],
        local_peer_id: PeerId,
    ) -> (Self, mpsc::Receiver<PeersetMsg>) {
        let mut peers = vec![];
        let mut active_indexes = vec![];
        let mut reader = BufReader::new(bytes);

        loop {
            let mut buf = [0; 38];
            if matches!(reader.read(&mut buf), Ok(n) if n == 38) {
                peers.push(PeerId::from_bytes(&buf).unwrap())
            } else {
                break;
            }

            let mut buf = [0; 1];
            reader.read(&mut buf).unwrap();
            active_indexes.push(buf[0] as usize);
        }

        let peers: Vec<_> = peers.into_iter().sorted_by_key(|p| p.to_bytes()).collect();

        let (tx, rx) = mpsc::channel(1);
        (
            Self {
                local_peer_id,
                session_peers: peers,
                parties_indexes: active_indexes,
                to_runtime: tx,
            },
            rx,
        )
    }

    pub async fn recover_from_cache(&mut self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let _ = self.to_runtime.send(PeersetMsg::ReadFromCache(tx)).await;
        let cache = rx.await.expect("runtime expected to serve protocol")?;
        let mut parties_indexes = vec![];
        for peer_id in self.session_peers.iter().sorted_by_key(|p| p.to_bytes()) {
            match cache.index_of(peer_id) {
                Some(i) => {
                    parties_indexes.push(cache.parties_indexes[i as usize]);
                }
                None => {
                    warn!(
                        "Peer {} does not appear in the peerset cache, skipping.",
                        peer_id.to_base58()
                    )
                }
            }
        }

        self.parties_indexes = parties_indexes;
        Ok(())
    }

    pub async fn save_to_cache(&mut self) -> anyhow::Result<()> {
        let (tx, rx) = oneshot::channel();
        let _ = self
            .to_runtime
            .send(PeersetMsg::WriteToCache(self.clone(), tx))
            .await;
        rx.await.expect("runtime expected to serve protocol")
    }

    pub fn index_of(&self, peer_id: &PeerId) -> Option<u16> {
        self.session_peers
            .iter()
            .position(|elem| *elem == *peer_id)
            .map(|i| i as u16)
    }

    pub fn size(&self) -> usize {
        self.session_peers.len()
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![];

        for (i, peer_id) in self.session_peers.iter().enumerate() {
            buf.append(&mut peer_id.to_bytes());
            buf.push(self.parties_indexes[i] as u8);
        }

        buf
    }

    pub fn remotes_iter(self) -> impl Iterator<Item = PeerId> {
        self.session_peers
            .into_iter()
            .enumerate()
            .filter(move |(_i, p)| *p != self.local_peer_id)
            .map(|(_i, p)| p.clone())
    }

    pub fn local_peer_id(&self) -> &PeerId {
        return &self.local_peer_id;
    }

    pub fn len(&self) -> usize {
        self.session_peers.len()
    }
}

impl Index<u16> for Peerset {
    type Output = PeerId;

    fn index(&self, index: u16) -> &Self::Output {
        &self.session_peers[index as usize]
    }
}

impl IntoIterator for Peerset {
    type Item = PeerId;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.session_peers.into_iter()
    }
}

#[cfg(test)]
mod tests {
    use crate::peerset::Peerset;
    use libp2p::PeerId;
    use std::str::FromStr;

    #[test]
    fn peerset_encoding() {
        let peer_ids = vec![
            PeerId::from_str("12D3KooWMQmcJA5raTtuxqAguM5CiXRhEDumLNmZQ7PmKZizjFBX").unwrap(),
            PeerId::from_str("12D3KooWHYG3YsVs9hTwbgPKVrTrPQBKc8FnDhV6bsJ4W37eds8p").unwrap(),
        ];
        let local_peer_id = peer_ids[0];
        let (mut peerset, _) = Peerset::new(peer_ids.into_iter(), local_peer_id);
        peerset.parties_indexes = vec![0, 2];
        let encoded = peerset.to_bytes();
        let (decoded, _) = Peerset::from_bytes(&*encoded, local_peer_id);

        println!(
            "original: {:?}, {:?}",
            peerset.parties_indexes, peerset.session_peers
        );
        println!(
            "decoded: {:?}, {:?}",
            decoded.parties_indexes, decoded.session_peers
        );

        assert_eq!(peerset.parties_indexes, decoded.parties_indexes);
    }
}
