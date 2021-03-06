use crate::Params;
use async_std::task;
use futures::prelude::*;

use libp2p::swarm::DialError;
use libp2p::{
    core::{
        connection::{ConnectionId, ListenerId},
        ConnectedPoint, Multiaddr, PeerId, PublicKey,
    },
    kad::{handler::KademliaHandlerProto, Kademlia, KademliaConfig, KademliaEvent, QueryId},
    mdns::MdnsEvent,
    swarm::{
        toggle::{Toggle, ToggleIntoProtoHandler},
        IntoProtocolsHandler, NetworkBehaviour, NetworkBehaviourAction, PollParameters,
        ProtocolsHandler,
    },
};
use libp2p::{kad::record::store::MemoryStore, mdns::Mdns};
use log::{debug, error, info, trace, warn};

use std::collections::HashMap;
use std::{
    collections::{HashSet, VecDeque},
    io,
    task::{Context, Poll},
};

/// Event generated by the `DiscoveryBehaviour`.
#[derive(Debug)]
pub enum DiscoveryOut {
    /// Event that notifies that we connected to the node with the given peer id.
    Connected(PeerId),

    /// Event that notifies that we disconnected with the node with the given peer id.
    Disconnected(PeerId),
}

/// Implementation of `NetworkBehaviour` that discovers the nodes on the network.
pub struct DiscoveryBehaviour {
    /// User-defined list of nodes and their addresses. Typically includes bootstrap nodes and
    /// reserved nodes.
    user_defined: Vec<(PeerId, Multiaddr)>,
    /// Kademlia discovery.
    kademlia: Toggle<Kademlia<MemoryStore>>,
    /// Discovers nodes on the local network.
    mdns: Toggle<Mdns>,
    /// Events to return in priority when polled.
    pending_events: VecDeque<DiscoveryOut>,
    /// Number of nodes we're currently connected to.
    num_connections: u64,
    /// Keeps hash set of peers connected.
    peers: HashSet<PeerId>,
    /// Keeps hash map of peers and their multiaddresses
    peer_addresses: HashMap<PeerId, Vec<Multiaddr>>,
}

impl DiscoveryBehaviour {
    pub fn new(local_public_key: PublicKey, params: Params) -> Self {
        let local_peer_id = local_public_key.to_peer_id();
        let mut peers = HashSet::new();
        let peer_addresses = HashMap::new();

        let user_defined: Vec<_> = params
            .rooms
            .iter()
            .flat_map(|ra| ra.boot_peers.clone())
            .map(|mwp| (mwp.peer_id, mwp.multiaddr))
            .collect();

        let kademlia_opt = {
            // Kademlia config
            let store = MemoryStore::new(local_peer_id.to_owned());
            let kad_config = KademliaConfig::default();

            if params.kademlia {
                let mut kademlia = Kademlia::with_config(local_peer_id, store, kad_config);
                for (peer_id, addr) in user_defined.iter() {
                    kademlia.add_address(peer_id, addr.clone());
                    peers.insert(*peer_id);
                }
                info!("kademlia peers: {:?}", peers);
                if let Err(e) = kademlia.bootstrap() {
                    warn!("Kademlia bootstrap failed: {}", e);
                }
                Some(kademlia)
            } else {
                None
            }
        };

        let mdns_opt = if params.mdns {
            Some(task::block_on(async {
                Mdns::new(Default::default())
                    .await
                    .expect("Could not start mDNS")
            }))
        } else {
            None
        };

        DiscoveryBehaviour {
            user_defined,
            kademlia: kademlia_opt.into(),
            pending_events: VecDeque::new(),
            num_connections: 0,
            mdns: mdns_opt.into(),
            peers,
            peer_addresses,
        }
    }

    /// Returns reference to peer set.
    pub fn peers(&self) -> &HashSet<PeerId> {
        &self.peers
    }

    /// Returns a map of peer ids and their multiaddresses
    pub fn peer_addresses(&self) -> &HashMap<PeerId, Vec<Multiaddr>> {
        &self.peer_addresses
    }

    /// Bootstrap Kademlia network
    pub fn bootstrap(&mut self) -> Result<QueryId, String> {
        if let Some(active_kad) = self.kademlia.as_mut() {
            active_kad.bootstrap().map_err(|e| e.to_string())
        } else {
            Err("Kademlia is not activated".to_string())
        }
    }
}

impl NetworkBehaviour for DiscoveryBehaviour {
    type ProtocolsHandler = ToggleIntoProtoHandler<KademliaHandlerProto<QueryId>>;
    type OutEvent = DiscoveryOut;

    fn new_handler(&mut self) -> Self::ProtocolsHandler {
        self.kademlia.new_handler()
    }

    fn addresses_of_peer(&mut self, peer_id: &PeerId) -> Vec<Multiaddr> {
        let mut list = self
            .user_defined
            .iter()
            .filter_map(|(p, a)| if p == peer_id { Some(a.clone()) } else { None })
            .collect::<Vec<_>>();

        {
            let mut list_to_filter = Vec::new();
            if let Some(k) = self.kademlia.as_mut() {
                list_to_filter.extend(k.addresses_of_peer(peer_id))
            }

            list_to_filter.extend(self.mdns.addresses_of_peer(peer_id));

            list.extend(list_to_filter);
        }

        trace!("Addresses of {:?}: {:?}", peer_id, list);

        list
    }

    fn inject_connected(&mut self, peer_id: &PeerId) {
        let multiaddr = self.addresses_of_peer(peer_id);
        self.peer_addresses.insert(*peer_id, multiaddr);
        self.peers.insert(*peer_id);
        self.pending_events
            .push_back(DiscoveryOut::Connected(*peer_id));

        self.kademlia.inject_connected(peer_id)
    }

    fn inject_disconnected(&mut self, peer_id: &PeerId) {
        self.pending_events
            .push_back(DiscoveryOut::Disconnected(*peer_id));

        self.kademlia.inject_disconnected(peer_id)
    }

    fn inject_connection_established(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        failed_addresses: Option<&Vec<Multiaddr>>,
    ) {
        self.num_connections += 1;

        self.kademlia
            .inject_connection_established(peer_id, conn, endpoint, failed_addresses)
    }

    fn inject_connection_closed(
        &mut self,
        peer_id: &PeerId,
        conn: &ConnectionId,
        endpoint: &ConnectedPoint,
        handler: <Self::ProtocolsHandler as IntoProtocolsHandler>::Handler,
    ) {
        self.num_connections -= 1;

        self.kademlia
            .inject_connection_closed(peer_id, conn, endpoint, handler)
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        connection: ConnectionId,
        event: <<Self::ProtocolsHandler as IntoProtocolsHandler>::Handler as ProtocolsHandler>::OutEvent,
    ) {
        if let Some(kad) = self.kademlia.as_mut() {
            return kad.inject_event(peer_id, connection, event);
        }
        error!("inject_node_event: no kademlia instance registered for protocol")
    }

    fn inject_dial_failure(
        &mut self,
        peer_id: Option<PeerId>,
        handler: Self::ProtocolsHandler,
        err: &DialError,
    ) {
        self.kademlia.inject_dial_failure(peer_id, handler, err)
    }

    fn inject_new_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        self.kademlia.inject_new_listen_addr(id, addr)
    }

    fn inject_expired_listen_addr(&mut self, id: ListenerId, addr: &Multiaddr) {
        self.kademlia.inject_expired_listen_addr(id, addr);
    }

    fn inject_listener_error(&mut self, id: ListenerId, err: &(dyn std::error::Error + 'static)) {
        self.kademlia.inject_listener_error(id, err)
    }

    fn inject_listener_closed(&mut self, id: ListenerId, reason: Result<(), &io::Error>) {
        self.kademlia.inject_listener_closed(id, reason)
    }

    fn inject_new_external_addr(&mut self, addr: &Multiaddr) {
        self.kademlia.inject_new_external_addr(addr)
    }

    #[allow(clippy::type_complexity)]
    fn poll(
        &mut self,
        cx: &mut Context,
        params: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ProtocolsHandler>> {
        // Immediately process the content of `discovered`.
        if let Some(ev) = self.pending_events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
        }

        // Poll Kademlia.
        while let Poll::Ready(ev) = self.kademlia.poll(cx, params) {
            match ev {
                NetworkBehaviourAction::GenerateEvent(ev) => match ev {
                    KademliaEvent::RoutingUpdated { .. } => {}
                    KademliaEvent::RoutablePeer { .. } => {}
                    KademliaEvent::PendingRoutablePeer { .. } => {}
                    other => {
                        debug!("Kademlia event: {:?}", other)
                    }
                },
                NetworkBehaviourAction::DialAddress { address, handler } => {
                    return Poll::Ready(NetworkBehaviourAction::DialAddress { address, handler })
                }
                NetworkBehaviourAction::DialPeer {
                    peer_id,
                    condition,
                    handler,
                } => {
                    return Poll::Ready(NetworkBehaviourAction::DialPeer {
                        peer_id,
                        condition,
                        handler,
                    })
                }
                NetworkBehaviourAction::NotifyHandler {
                    peer_id,
                    handler,
                    event,
                } => {
                    return Poll::Ready(NetworkBehaviourAction::NotifyHandler {
                        peer_id,
                        handler,
                        event,
                    })
                }
                NetworkBehaviourAction::ReportObservedAddr { address, score } => {
                    return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr {
                        address,
                        score,
                    })
                }
                NetworkBehaviourAction::CloseConnection {
                    peer_id,
                    connection,
                } => {
                    return Poll::Ready(NetworkBehaviourAction::CloseConnection {
                        peer_id,
                        connection,
                    })
                }
            }
        }

        // Poll mdns.
        while let Poll::Ready(ev) = self.mdns.poll(cx, params) {
            match ev {
                NetworkBehaviourAction::GenerateEvent(event) => match event {
                    MdnsEvent::Discovered(list) => {
                        // Add any discovered peers to Kademlia
                        for (peer_id, multiaddr) in list {
                            if let Some(kad) = self.kademlia.as_mut() {
                                kad.add_address(&peer_id, multiaddr);
                            }
                        }
                    }
                    MdnsEvent::Expired(_) => {}
                },
                NetworkBehaviourAction::DialAddress { .. } => {}
                NetworkBehaviourAction::DialPeer { .. } => {}
                // Nothing to notify handler
                NetworkBehaviourAction::NotifyHandler { event, .. } => match event {
                    _ => {}
                },
                NetworkBehaviourAction::ReportObservedAddr { address, score } => {
                    return Poll::Ready(NetworkBehaviourAction::ReportObservedAddr {
                        address,
                        score,
                    })
                }
                NetworkBehaviourAction::CloseConnection {
                    peer_id,
                    connection,
                } => {
                    return Poll::Ready(NetworkBehaviourAction::CloseConnection {
                        peer_id,
                        connection,
                    })
                }
            }
        }

        // Poll pending events
        if let Some(ev) = self.pending_events.pop_front() {
            return Poll::Ready(NetworkBehaviourAction::GenerateEvent(ev));
        }

        Poll::Pending
    }
}
