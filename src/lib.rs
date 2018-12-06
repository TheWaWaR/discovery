extern crate yamux;
extern crate bytes;
extern crate byteorder;
extern crate fnv;
extern crate futures;
extern crate tokio;
extern crate tokio_io;
extern crate tokio_codec;
extern crate tokio_timer;
extern crate log;
extern crate multiaddr;
extern crate trust_dns;

use std::io;
use yamux::{
    config::Config,
    session::Session,
    stream::StreamHandle,
};
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

pub struct Discovery {}

impl Discovery {
    pub fn start() {}
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
    node_id: String,
    addresses: Vec<Multiaddr>,
}
