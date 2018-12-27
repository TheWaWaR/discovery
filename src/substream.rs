use std::collections::{BTreeMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::rc::Rc;
use std::time::{Duration, Instant};

use bincode::{deserialize, serialize};
use bytes::{BufMut, Bytes, BytesMut};
use fnv::{FnvHashMap, FnvHashSet};
use futures::{
    sync::mpsc::{channel, Receiver, Sender},
    try_ready, Async, AsyncSink, Poll, Sink, Stream,
};
use log::debug;
use multiaddr::Multiaddr;
use rand::seq::SliceRandom;
use serde_derive::{Deserialize, Serialize};
use tokio::codec::{Decoder, Encoder, Framed};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::timer::{self, Interval};
use yamux::{config::Config, session::Session, stream::StreamHandle};

use crate::message::{DiscoveryCodec, DiscoveryMessage, Nodes, Node};
use crate::addr::{AddrKnown, AddrRaw, AddressManager};

// FIXME: should be a more high level version number
const VERSION: u32 = 0;
// The maximum number of new addresses to accumulate before announcing.
const MAX_ADDR_TO_SEND: usize = 1000;
// Every 24 hours send announce nodes message
const ANNOUNCE_INTERVAL: u64 = 3600 * 24;
const ANNOUNCE_THRESHOLD: usize = 10;

#[derive(Eq, PartialEq, Hash, Debug, Clone)]
pub struct SubstreamKey {
    pub(crate) remote_addr: SocketAddr,
    pub(crate) direction: Direction,
    pub(crate) substream_id: u32,
}

pub struct SubstreamValue {
    framed_stream: Framed<StreamHandle, DiscoveryCodec>,
    // received pending messages
    pub(crate) pending_messages: VecDeque<DiscoveryMessage>,
    pub(crate) addr_known: AddrKnown,
    // FIXME: Remote listen address, resolved by id protocol
    pub(crate) remote_addr: SocketAddr,
    pub(crate) announce: bool,
    pub(crate) announce_addrs: Vec<SocketAddr>,
    timer_future: Interval,
    received_get_nodes: bool,
    received_nodes: bool,
    remote_closed: bool,
}

impl SubstreamValue {
    pub(crate) fn new(
        direction: Direction,
        stream: StreamHandle,
        max_known: usize,
        remote_addr: SocketAddr,
    ) -> SubstreamValue {
        let mut pending_messages = VecDeque::default();
        if direction == Direction::Outbound {
            pending_messages.push_back(DiscoveryMessage::GetNodes {
                version: VERSION,
                count: MAX_ADDR_TO_SEND as u32,
            });
        }
        SubstreamValue {
            framed_stream: Framed::new(stream, DiscoveryCodec::default()),
            timer_future: Interval::new_interval(Duration::from_secs(ANNOUNCE_INTERVAL)),
            pending_messages,
            addr_known: AddrKnown::new(max_known),
            remote_addr,
            announce: false,
            announce_addrs: Vec::new(),
            received_get_nodes: false,
            received_nodes: false,
            remote_closed: false,
        }
    }

    pub(crate) fn check_timer(&mut self) -> Result<(), tokio::timer::Error> {
        match self.timer_future.poll()? {
            Async::Ready(Some(_announce_at)) => {
                self.announce = true;
            }
            Async::Ready(None) => unreachable!(),
            Async::NotReady => {}
        }
        Ok(())
    }

    pub(crate) fn send_messages(&mut self) -> Result<(), io::Error> {
        while let Some(message) = self.pending_messages.pop_front() {
            match self.framed_stream.start_send(message)? {
                AsyncSink::NotReady(message) => {
                    self.pending_messages.push_front(message);
                    return Ok(());
                }
                AsyncSink::Ready => {}
            }
        }
        self.framed_stream.poll_complete()?;
        Ok(())
    }

    pub(crate) fn handle_message<M: AddressManager>(
        &mut self,
        message: DiscoveryMessage,
        addr_mgr: &mut M,
    ) -> Result<Option<Nodes>, io::Error> {
        match message {
            DiscoveryMessage::GetNodes { version, count } => {
                if self.received_get_nodes {
                    // TODO: misbehavior
                    addr_mgr.misbehave(self.remote_addr, 111);
                } else {
                    // TODO: magic number
                    let mut items = addr_mgr.get_random(2500);
                    while items.len() > 1000 {
                        if let Some(last_item) = items.pop() {
                            let idx = rand::random::<usize>() % 1000;
                            items[idx] = last_item;
                        }
                    }
                    let items = items
                        .into_iter()
                        .map(|addr| Node {
                            node_id: None,
                            addresses: vec![AddrRaw::from(addr)],
                        })
                        .collect::<Vec<_>>();
                    let nodes = Nodes {
                        announce: false,
                        items,
                    };
                    self.pending_messages
                        .push_back(DiscoveryMessage::Nodes(nodes));
                    self.received_get_nodes = true;
                }
            }
            DiscoveryMessage::Nodes(nodes) => {
                if nodes.announce {
                    if nodes.items.len() > ANNOUNCE_THRESHOLD {
                        // TODO: misbehavior
                        addr_mgr.misbehave(self.remote_addr, 222);
                    } else {
                        return Ok(Some(nodes));
                    }
                } else {
                    if self.received_nodes {
                        // TODO: misbehavior
                        addr_mgr.misbehave(self.remote_addr, 333);
                    } else if nodes.items.len() > MAX_ADDR_TO_SEND {
                        // TODO: misbehavior
                        addr_mgr.misbehave(self.remote_addr, 444);
                    } else {
                        self.received_nodes = true;
                        return Ok(Some(nodes));
                    }
                }
            }
        }
        Ok(None)
    }

    pub(crate) fn receive_messages<M: AddressManager>(
        &mut self,
        addr_mgr: &mut M,
    ) -> Result<Option<Vec<Nodes>>, io::Error> {
        if self.remote_closed {
            return Ok(None);
        }

        let mut nodes_list = Vec::new();
        loop {
            match self.framed_stream.poll()? {
                Async::Ready(Some(message)) => {
                    if let Some(nodes) = self.handle_message(message, addr_mgr)? {
                        // Add to known address list
                        for node in &nodes.items {
                            for addr in &node.addresses {
                                self.addr_known.insert(AddrRaw::from(addr.clone()));
                            }
                        }
                        nodes_list.push(nodes);
                    }
                }
                Async::Ready(None) => {
                    debug!("remote closed");
                    self.remote_closed = true;
                    break;
                }
                Async::NotReady => {
                    break;
                }
            }
        }
        Ok(Some(nodes_list))
    }
}

pub struct Substream {
    pub(crate) remote_addr: SocketAddr,
    pub(crate) direction: Direction,
    pub(crate) stream: StreamHandle,
}

impl Substream {
    pub fn key(&self) -> SubstreamKey {
        SubstreamKey {
            remote_addr: self.remote_addr,
            direction: self.direction,
            substream_id: self.stream.id(),
        }
    }
}

#[derive(Eq, PartialEq, Hash, Debug, Clone, Copy)]
pub enum Direction {
    // The connection(session) is open by other peer
    Inbound,
    // The connection(session) is open by current peer
    Outbound,
}
