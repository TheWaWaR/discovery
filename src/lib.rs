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

mod addr;
mod message;
mod substream;

pub use crate::{
    addr::{AddrKnown, AddrRaw, AddressManager, DemoAddressManager},
    message::{DiscoveryMessage, Nodes, Node},
    substream::{SubstreamKey, SubstreamValue, Substream},
};

use crate::message::{DiscoveryCodec};
use crate::addr::{DEFAULT_MAX_KNOWN};

pub struct Discovery<M> {
    // Default: 5000
    max_known: usize,

    // Address Manager
    addr_mgr: M,

    // The Nodes not yet been yield
    pending_nodes: VecDeque<(SubstreamKey, Nodes)>,

    // For manage those substreams
    substreams: FnvHashMap<SubstreamKey, SubstreamValue>,

    // For add new substream to Discovery
    substream_sender: Sender<Substream>,
    // For add new substream to Discovery
    substream_receiver: Receiver<Substream>,

    err_keys: FnvHashSet<SubstreamKey>,
}

pub struct DiscoveryHandle {
    pub substream_sender: Sender<Substream>,
}

impl<M: AddressManager> Discovery<M> {
    pub fn new(addr_mgr: M) -> Discovery<M> {
        let (substream_sender, substream_receiver) = channel(8);
        Discovery {
            max_known: DEFAULT_MAX_KNOWN,
            addr_mgr,
            pending_nodes: VecDeque::default(),
            substreams: FnvHashMap::default(),
            substream_sender,
            substream_receiver,
            err_keys: FnvHashSet::default(),
        }
    }

    pub fn handle(&self) -> DiscoveryHandle {
        DiscoveryHandle {
            substream_sender: self.substream_sender.clone(),
        }
    }

    // fn bootstrap() {}
    // fn query_dns() {}
    // fn get_builtin_addresses() {}

    fn get_nodes(&mut self) {}
    fn handle_nodes(&mut self) {}
}

impl<M: AddressManager> Stream for Discovery<M> {
    type Item = Nodes;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.substream_receiver.poll() {
            Ok(Async::Ready(Some(substream))) => {
                let key = substream.key();
                let value = SubstreamValue::new(
                    key.direction,
                    substream.stream,
                    self.max_known,
                    key.remote_addr,
                );
                self.substreams.insert(key, value);
            }
            Ok(Async::Ready(None)) => unreachable!(),
            Ok(Async::NotReady) => {}
            Err(err) => {
                debug!("receive substream error: {:?}", err);
                return Err(io::ErrorKind::Other.into());
            }
        }

        let mut announce_addrs = Vec::new();
        for (key, value) in self.substreams.iter_mut() {
            if let Err(err) = value.check_timer() {
                debug!("substream {:?} poll timer_future error: {:?}", key, err);
                self.err_keys.insert(key.clone());
            }

            match value.receive_messages(&mut self.addr_mgr) {
                Ok(Some(nodes_list)) => {
                    for nodes in nodes_list {
                        self.pending_nodes.push_back((key.clone(), nodes));
                    }
                }
                Ok(None) => {
                    // TODO: EOF => remote closed the connection
                }
                Err(err) => {
                    debug!("substream {:?} receive messages error: {:?}", key, err);
                    // remove the substream
                    self.err_keys.insert(key.clone());
                }
            }

            match value.send_messages() {
                Ok(_) => {}
                Err(err) => {
                    debug!("substream {:?} send messages error: {:?}", key, err);
                    // remove the substream
                    self.err_keys.insert(key.clone());
                }
            }

            if value.announce {
                announce_addrs.push(value.remote_addr);
            }
        }

        for key in self.err_keys.drain() {
            self.substreams.remove(&key);
        }

        let mut rng = rand::thread_rng();
        let mut remain_keys = self.substreams.keys().cloned().collect::<Vec<_>>();
        for announce_addr in announce_addrs.into_iter() {
            remain_keys.shuffle(&mut rng);
            for i in 0..2 {
                if let Some(key) = remain_keys.get(i) {
                    if let Some(value) = self.substreams.get_mut(key) {
                        if value.announce_addrs.len() < 10 {
                            value.announce_addrs.push(announce_addr);
                        }
                    }
                }
            }
        }

        for (key, value) in self.substreams.iter_mut() {
            let announce_addrs = value.announce_addrs.split_off(0);
            if !announce_addrs.is_empty() {
                let items = announce_addrs
                    .into_iter()
                    .map(|addr| AddrRaw::from(addr))
                    .filter(|addr| !value.addr_known.contains(addr))
                    .map(|addr| Node {
                        node_id: None,
                        addresses: vec![addr],
                    })
                    .collect::<Vec<_>>();
                let nodes = Nodes {
                    announce: false,
                    items,
                };
                value
                    .pending_messages
                    .push_back(DiscoveryMessage::Nodes(nodes));
            }

            match value.send_messages() {
                Ok(_) => {}
                Err(err) => {
                    debug!("substream {:?} send messages error: {:?}", key, err);
                    // remove the substream
                    self.err_keys.insert(key.clone());
                }
            }
        }

        match self.pending_nodes.pop_front() {
            Some((_key, nodes)) => Ok(Async::Ready(Some(nodes))),
            None => Ok(Async::NotReady),
        }
    }
}

