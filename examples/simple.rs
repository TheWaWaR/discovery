
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

use discovery::{
    Discovery,
    DiscoveryHandle,
};


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
