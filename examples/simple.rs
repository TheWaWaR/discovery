use bincode::{deserialize, serialize};
use bytes::{BufMut, Bytes, BytesMut};
use fnv::{FnvHashMap, FnvHashSet};
use futures::{
    sync::mpsc::{channel, Receiver, Sender},
    try_ready, Async, AsyncSink, Poll, Sink, Stream,
};
use log::debug;
use multiaddr::Multiaddr;
use serde_derive::{Deserialize, Serialize};
use tokio::codec::{Decoder, Encoder, Framed};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::timer::{self, Interval};

use discovery::{
    Discovery,
    DiscoveryHandle,
    DemoAddressManager,
};

fn main() {
    println!("Starting ......");
    start_discovery();
}

fn start_discovery() {
    let addr_mgr = DemoAddressManager::default();
    let discovery = Discovery::new(addr_mgr);
    let handle = discovery.handle();
    let fut = discovery
        .map_err(|err| {
            println!("Receive nodes error: {:?}", err);
            ()
        })
        .for_each(|nodes| {
            println!("Got nodes: {:?}", nodes);
            Ok(())
        });
    tokio::run(fut);
}
