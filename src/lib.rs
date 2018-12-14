use std::io;

use yamux::{
    StreamId,
    config::Config,
    session::Session,
    stream::StreamHandle,
};
use fnv::FnvHashMap;
use futures::{
    try_ready,
    Async,
    AsyncSink,
    Poll,
    Sink,
    Stream,
    sync::mpsc::{channel, Sender, Receiver},
};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_codec::{Framed};
use multiaddr::{Multiaddr};

pub trait AddressManager {
    // Add to known address list
    fn add_known(&mut self);
    // Add new addresses
    fn add_new(&mut self);
    // Remember feleer connection
    fn add_tried(&mut self);
    // Get addresses for startup connections
    fn get_addresses(&self, max: usize);
    // When connected connection is not enough,
    // get some random addresses to connnect to.
    fn get_random(&self, n: usize);
}


pub struct Discovery {
    // For manage those substreams
    substreams: FnvHashMap<SubstreamKey, Substream>,

    // For add new substream to Discovery
    substream_sender: Sender<Substream>,
    // For add new substream to Discovery
    substream_receiver: Receiver<Substream>,
}

impl Discovery {
    pub fn new() -> Discovery {
        let (substream_sender, substream_receiver) = channel(8);
        Discovery {
            substreams: FnvHashMap::default(),
            substream_sender,
            substream_receiver,
        }
    }
    fn bootstrap() {}
    fn query_dns() {}
    fn get_builtin_addresses() {}
}

impl Stream for Discovery {
    type Item = Nodes;
    type Error = io::Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        Ok(Async::NotReady)
    }
}

#[derive(Eq, PartialEq, Hash, Debug)]
pub struct SubstreamKey {
    session_id: u64,
    substream_id: u32,
    direction: Direction,
}

pub struct Substream {
    session_id: u64,
    direction: Direction,
    stream: StreamHandle,
}

#[derive(Eq, PartialEq, Hash, Debug)]
pub enum Direction {
    // The connection(session) is open by other peer
    Inbound,
    // The connection(session) is open by current peer
    Outbound,
}

pub enum DiscoveryMessage {
    GetNodes {
        version: u32,
        count: u32,
    },
    Nodes(Nodes),
}

pub struct Nodes {
    announce: bool,
    items: Vec<Node>,
}

pub struct Node {
    // The addresses from DNS and seed don't have `node_id`
    node_id: Option<String>,
    addresses: Vec<Multiaddr>,
}
