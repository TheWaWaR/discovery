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
use tokio_codec::{Decoder, Encoder, Framed};
use tokio_io::{AsyncRead, AsyncWrite};
use tokio_timer::{self, Interval};

use discovery::{Discovery, DiscoveryHandle};

fn main() {
    println!("Starting ......");
    start_discovery();
}

fn start_discovery() {
    let discovery = Discovery::default();
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
