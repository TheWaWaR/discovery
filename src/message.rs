use std::io;

use bytes::{BufMut, Bytes, BytesMut};
use log::debug;
use serde_derive::{Deserialize, Serialize};
use tokio::codec::{Decoder, Encoder, Framed};
use tokio::codec::length_delimited::{LengthDelimitedCodec};

use crate::addr::AddrRaw;


pub(crate) struct DiscoveryCodec {
    inner: LengthDelimitedCodec,
}

impl Default for DiscoveryCodec {
    fn default() -> DiscoveryCodec {
        DiscoveryCodec { inner: LengthDelimitedCodec::new() }
    }
}

impl Decoder for DiscoveryCodec {
    type Item = DiscoveryMessage;
    type Error = io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        match self.inner.decode(src) {
            Ok(Some(frame)) => {
                // TODO: more error information
                bincode::deserialize(&frame)
                    .map(Some)
                    .map_err(|err| io::ErrorKind::InvalidData.into())
            }
            Ok(None) => Ok(None),
            // TODO: more error information
            Err(err) => Err(io::ErrorKind::InvalidData.into()),
        }
    }
}

impl Encoder for DiscoveryCodec {
    type Item = DiscoveryMessage;
    type Error = io::Error;

    fn encode(&mut self, item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // TODO: more error information
        bincode::serialize(&item)
            .map_err(|err| io::ErrorKind::InvalidData.into())
            .and_then(|frame| self.inner.encode(Bytes::from(frame), dst))
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub enum DiscoveryMessage {
    GetNodes { version: u32, count: u32 },
    Nodes(Nodes),
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct Nodes {
    pub(crate) announce: bool,
    pub(crate) items: Vec<Node>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct Node {
    // The address from DNS and seed don't have `node_id`
    pub(crate) node_id: Option<String>,
    pub(crate) addresses: Vec<AddrRaw>,
}
