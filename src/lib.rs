use std::io;
use std::rc::Rc;
use std::time::{Instant, Duration};
use std::net::{SocketAddr, IpAddr};
use std::collections::{BTreeMap, VecDeque};

use yamux::{
    config::Config,
    session::Session,
    stream::StreamHandle,
};
use fnv::{FnvHashMap, FnvHashSet};
use futures::{
    try_ready,
    Async,
    AsyncSink,
    Poll,
    Sink,
    Stream,
    sync::mpsc::{channel, Sender, Receiver},
};
use bytes::{BufMut, Bytes, BytesMut};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_codec::{Framed, Decoder, Encoder};
use tokio_timer::{self, Interval};
use multiaddr::{Multiaddr};
use log::debug;
use bincode::{serialize, deserialize};
use serde_derive::{Serialize, Deserialize};


// See: bitcoin/netaddress.cpp pchIPv4[12]
const PCH_IPV4: [u8; 18] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xff, 0xff,
    // ipv4 part
    0, 0, 0, 0,
    // port part
    0, 0
];
const DEFAULT_MAX_KNOWN: usize = 5000;
// FIXME: should be a more high level version number
const VERSION: u32 = 0;
// The maximum number of new addresses to accumulate before announcing.
const MAX_ADDR_TO_SEND: usize = 1000;
// Every 24 hours send announce nodes message
const ANNOUNCE_INTERVAL: u64 = 3600 * 24;
const ANNOUNCE_THRESHOLD: usize = 10;


#[derive(Clone, Debug, PartialOrd, Ord, Eq, PartialEq, Hash, Serialize, Deserialize)]
struct AddrRaw([u8; 18]);

impl From<SocketAddr> for AddrRaw {
    // CService::GetKey()
    fn from(addr: SocketAddr) -> AddrRaw {
        let mut data = PCH_IPV4;
        match addr.ip() {
            IpAddr::V4(ipv4) => {
                data[12..16].copy_from_slice(&ipv4.octets());
            }
            IpAddr::V6(ipv6) => {
                data[0..16].copy_from_slice(&ipv6.octets());
            }
        }
        let port = addr.port();
        data[16] = (port / 0x100) as u8;
        data[17] = (port & 0x0FF) as u8;
        AddrRaw(data)
    }
}

impl AddrRaw {
    pub fn socket_addr(&self) -> SocketAddr {
        let mut is_ipv4 = true;
        for i in 0..12 {
            if self.0[i] != PCH_IPV4[i] {
                is_ipv4 = false;
                break;
            }
        }
        let ip: IpAddr = if is_ipv4 {
            let mut buf = [0u8; 4];
            buf.copy_from_slice(&self.0[12..16]);
            From::from(buf)
        } else {
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&self.0[0..16]);
            From::from(buf)
        };
        let port = 0x100 * self.0[16] as u16 + self.0[17] as u16;
        SocketAddr::new(ip, port)
    }

    // Copy from std::net::IpAddr::is_global
    pub fn is_reachable(&self) -> bool {
        match self.socket_addr().ip() {
            IpAddr::V4(ipv4) => {
                !ipv4.is_private() && !ipv4.is_loopback() && !ipv4.is_link_local() &&
                    !ipv4.is_broadcast() && !ipv4.is_documentation() && !ipv4.is_unspecified()
            }
            IpAddr::V6(ipv6) => {
                let scope = if ipv6.is_multicast() {
                    match ipv6.segments()[0] & 0x000f {
                        1 => Some(false),
                        2 => Some(false),
                        3 => Some(false),
                        4 => Some(false),
                        5 => Some(false),
                        8 => Some(false),
                        14 => Some(true),
                        _ => None
                    }
                } else {
                    None
                };
                match scope {
                    Some(true) => true,
                    None => {
                        !ipv6.is_multicast()
                            && !ipv6.is_loopback()
                        // && !ipv6.is_unicast_link_local()
                            && !((ipv6.segments()[0] & 0xffc0) == 0xfe80)
                        // && !ipv6.is_unicast_site_local()
                            && !((ipv6.segments()[0] & 0xffc0) == 0xfec0)
                        // && !ipv6.is_unique_local()
                            && !((ipv6.segments()[0] & 0xfe00) == 0xfc00)
                            && !ipv6.is_unspecified()
                        // && !ipv6.is_documentation()
                            && !((ipv6.segments()[0] == 0x2001) && (ipv6.segments()[1] == 0xdb8))
                    },
                    _ => false
                }
            }
        }
    }
}

// bitcoin: bloom.h, bloom.cpp => CRollingBloomFilter
pub struct AddrKnown {
    max_known: usize,
    addrs: FnvHashSet<AddrRaw>,
    addr_times: FnvHashMap<AddrRaw, Instant>,
    time_addrs: BTreeMap<Instant, AddrRaw>,
}

impl AddrKnown {
    fn new(max_known: usize) -> AddrKnown {
        AddrKnown {
            max_known,
            addrs: FnvHashSet::default(),
            addr_times: FnvHashMap::default(),
            time_addrs: BTreeMap::default(),
        }
    }

    fn insert(&mut self, key: AddrRaw) {
        let now = Instant::now();
        self.addrs.insert(key.clone());
        self.time_addrs.insert(now.clone(), key.clone());
        self.addr_times.insert(key, now);

        if self.addrs.len() > self.max_known {
            let first_time = {
                let (first_time, first_key) = self.time_addrs.iter().next().unwrap();
                self.addrs.remove(&first_key);
                self.addr_times.remove(&first_key);
                first_time.clone()
            };
            self.time_addrs.remove(&first_time);
        }
    }

    fn contains(&self, addr: &AddrRaw) -> bool {
        self.addrs.contains(addr)
    }

    fn reset(&mut self) {
        self.addrs.clear();
        self.time_addrs.clear();
        self.addr_times.clear();
    }
}

impl Default for AddrKnown {
    fn default() -> AddrKnown {
        AddrKnown::new(DEFAULT_MAX_KNOWN)
    }
}

pub struct Discovery {
    // Default: 5000
    max_known: usize,

    // The Nodes not yet been yield
    pending_nodes: VecDeque<(SubstreamKey, Nodes)>,

    // For manage those substreams
    substreams: FnvHashMap<SubstreamKey, SubstreamValue>,

    // For add new substream to Discovery
    substream_sender: Sender<Substream>,
    // For add new substream to Discovery
    substream_receiver: Receiver<Substream>,
}

pub struct DiscoveryHandle {
    pub substream_sender: Sender<Substream>,
}

impl Discovery {
    pub fn new(max_known: usize) -> Discovery {
        let (substream_sender, substream_receiver) = channel(8);
        Discovery {
            max_known,
            pending_nodes: VecDeque::default(),
            substreams: FnvHashMap::default(),
            substream_sender,
            substream_receiver,
        }
    }

    pub fn handle(&self) -> DiscoveryHandle {
        DiscoveryHandle {
            substream_sender: self.substream_sender.clone()
        }
    }

    // fn bootstrap() {}
    // fn query_dns() {}
    // fn get_builtin_addresses() {}

    fn get_nodes(&mut self) {}
    fn handle_nodes(&mut self) {}
}

impl Default for Discovery {
    fn default() -> Discovery {
        Discovery::new(DEFAULT_MAX_KNOWN)
    }
}

impl Stream for Discovery {
    type Item = Nodes;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match self.substream_receiver.poll() {
            Ok(Async::Ready(Some(substream))) => {
                let key = substream.key();
                let value = SubstreamValue::new(key.direction, substream.stream, self.max_known);
                self.substreams.insert(key, value);
            }
            Ok(Async::Ready(None)) => unreachable!(),
            Ok(Async::NotReady) => {}
            Err(err) => {
                debug!("receive substream error: {:?}", err);
                return Err(io::ErrorKind::Other.into());
            },
        }

        let mut err_keys = FnvHashSet::default();
        // TODO: should optmize later
        let addrs = self.substreams
            .keys()
            .filter(|key| {
                // FIXME: filter out with known address list
                true
            })
            .map(|key| AddrRaw::from(key.remote_addr))
            .collect::<Vec<_>>();

        for (key, value) in self.substreams.iter_mut() {
            if let Err(err) = value.check_timer(&addrs) {
                debug!("substream {:?} poll timer_future error: {:?}", key, err);
                err_keys.insert(key.clone());
            }

            match value.receive_messages() {
                Ok(Some(nodes_list)) => {
                    for nodes in nodes_list {
                        self.pending_nodes.push_back((key.clone(), nodes));
                    }
                },
                Ok(None) => {
                    // TODO: EOF => remote closed the connection
                }
                Err(err) => {
                    debug!("substream {:?} receive messages error: {:?}", key, err);
                    // remove the substream
                    err_keys.insert(key.clone());
                }
            }

            match value.send_messages() {
                Ok(_) => {},
                Err(err) => {
                    debug!("substream {:?} send messages error: {:?}", key, err);
                    // remove the substream
                    err_keys.insert(key.clone());
                }
            }
        }

        for key in err_keys {
            self.substreams.remove(&key);
        }

        match self.pending_nodes.pop_front() {
            Some((_key, nodes)) => Ok(Async::Ready(Some(nodes))),
            None => Ok(Async::NotReady)
        }
    }
}

#[derive(Eq, PartialEq, Hash, Debug, Clone)]
pub struct SubstreamKey {
    remote_addr: SocketAddr,
    direction: Direction,
    substream_id: u32,
}

pub struct SubstreamValue {
    framed_stream: Framed<StreamHandle, DiscoveryCodec>,
    pending_messages: VecDeque<DiscoveryMessage>,
    addr_known: AddrKnown,
    timer_future: Interval,
    received_get_nodes: bool,
    received_nodes: bool,
    received_first_announce_nodes: bool,
    remote_closed: bool,
}

impl SubstreamValue {
    fn new(direction: Direction, stream: StreamHandle, max_known: usize) -> SubstreamValue {
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
            received_get_nodes: false,
            received_nodes: false,
            received_first_announce_nodes: false,
            remote_closed: false,
        }
    }

    fn check_timer(&mut self, addrs: &Vec<AddrRaw>) -> Result<(), tokio_timer::Error> {
        match self.timer_future.poll()? {
            Async::Ready(Some(announce_at)) => {
                // announce Nodes
                let message = DiscoveryMessage::Nodes(Nodes {
                    announce: true,
                    items: addrs
                        .iter()
                        .filter(|addr| !self.addr_known.contains(addr))
                        .map(|addr| Node { node_id: None, addresses: vec![addr.clone()] })
                        .collect()
                });
                self.pending_messages.push_back(message);
            }
            Async::Ready(None) => unreachable!(),
            Async::NotReady => {},
        }
        Ok(())
    }

    fn send_messages(&mut self) -> Result<(), io::Error> {
        while let Some(message) = self.pending_messages.pop_front() {
            match self.framed_stream.start_send(message)? {
                AsyncSink::NotReady(message) => {
                    self.pending_messages.push_front(message);
                    return Ok(());
                }
                AsyncSink::Ready => {},
            }
        }
        self.framed_stream.poll_complete()?;
        Ok(())
    }

    fn handle_message(&mut self, message: DiscoveryMessage) -> Result<Option<Nodes>, io::Error> {
        match message {
            DiscoveryMessage::GetNodes { version, count } => {
                if self.received_get_nodes {
                    // TODO: misbehavior
                } else {
                    // TODO: fill items by AddressManager
                    let nodes = Nodes { announce: false, items: vec![] };
                    self.pending_messages.push_back(DiscoveryMessage::Nodes(nodes));
                    self.received_get_nodes = true;
                }
            }
            DiscoveryMessage::Nodes(nodes) => {
                if nodes.announce {
                    if self.received_first_announce_nodes && nodes.items.len() > ANNOUNCE_THRESHOLD {
                        // TODO: misbehavior
                    } else  {
                        if !self.received_first_announce_nodes {
                            self.received_first_announce_nodes = true;
                        }
                        return Ok(Some(nodes));
                    }
                } else {
                    if self.received_nodes {
                        // TODO: misbehavior
                    } else if nodes.items.len() > MAX_ADDR_TO_SEND {
                        // TODO: misbehavior
                    } else {
                        self.received_nodes = true;
                        return Ok(Some(nodes));
                    }
                }
            }
        }
        Ok(None)
    }

    fn receive_messages(&mut self) -> Result<Option<Vec<Nodes>>, io::Error> {
        if self.remote_closed {
            return Ok(None);
        }

        let mut nodes_list = Vec::new();
        loop {
            match self.framed_stream.poll()? {
                Async::Ready(Some(message)) => {
                    if let Some(nodes) = self.handle_message(message)? {
                        // Add to known address list
                        for node in &nodes.items {
                            for addr in &node.addresses {
                                self.addr_known.insert(AddrRaw::from(addr.clone()));
                            }
                        }
                        nodes_list.push(nodes);
                    }
                },
                Async::Ready(None) => {
                    debug!("remote closed");
                    self.remote_closed = true;
                    break;
                },
                Async::NotReady => {
                    break;
                },
            }
        }
        Ok(Some(nodes_list))
    }
}

pub struct Substream {
    remote_addr: SocketAddr,
    direction: Direction,
    stream: StreamHandle,
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

#[derive(Default)]
struct DiscoveryCodec {}

impl Decoder for DiscoveryCodec {
    type Item = DiscoveryMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        Ok(None)
    }
}

impl Encoder for DiscoveryCodec {
    type Item = DiscoveryMessage;
    type Error = io::Error;

    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[derive(Eq, PartialEq, Hash, Debug, Clone, Copy)]
pub enum Direction {
    // The connection(session) is open by other peer
    Inbound,
    // The connection(session) is open by current peer
    Outbound,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub enum DiscoveryMessage {
    GetNodes {
        version: u32,
        count: u32,
    },
    Nodes(Nodes),
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct Nodes {
    announce: bool,
    items: Vec<Node>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct Node {
    // The address from DNS and seed don't have `node_id`
    node_id: Option<String>,
    addresses: Vec<AddrRaw>,
}
