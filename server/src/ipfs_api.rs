use futures::future::Future;
use reqwest::r#async::{multipart, Client};
use serde::{Deserialize, Serialize};
use serde_json;
use tracing::{event, span, Level};
use tracing_futures::Instrument; // todo, idk

use crate::api_types::DagCacheError;
use crate::encoding_types;
use crate::ipfs_types;
use crate::lib::BoxFuture;

pub struct IPFSNode(reqwest::Url); //base url, copy mutated to produce specific path. should have no path component

pub trait IPFSCapability {
    fn get(&self, k: ipfs_types::IPFSHash) -> BoxFuture<ipfs_types::DagNode, DagCacheError>;

    fn put(&self, v: ipfs_types::DagNode) -> BoxFuture<ipfs_types::IPFSHash, DagCacheError>;
}

impl IPFSNode {
    pub fn new(a: reqwest::Url) -> Self {
        IPFSNode(a)
    }
}

impl IPFSCapability for IPFSNode {
    fn get(&self, k: ipfs_types::IPFSHash) -> BoxFuture<ipfs_types::DagNode, DagCacheError> {
        let mut url = self.0.clone();
        url.set_path("api/v0/object/get");
        url.query_pairs_mut()
            .append_pair("data-encoding", "base64")
            .append_pair("arg", &k.to_string());

        println!("hitting url: {:?}", &url);

        let f = Client::new()
            .get(url.clone())
            .send()
            .and_then(|mut x| x.json())
            .map(|e: DagNode| ipfs_types::DagNode {
                data: e.data,
                links: e
                    .links
                    .into_iter()
                    .map(|IPFSHeader { hash, name, size }| ipfs_types::IPFSHeader {
                        hash,
                        name,
                        size,
                    })
                    .collect(),
            })
            .map_err(|e| {
                event!(Level::ERROR,  msg = "failed parsing json", response.error = ?e);
                DagCacheError::IPFSJsonError
            })
            .instrument(
                span!(Level::TRACE, "ipfs-get", hash_pointer = k.to_string().as_str(), url = ?url ),
            );

        Box::new(f)
    }

    fn put(&self, v: ipfs_types::DagNode) -> BoxFuture<ipfs_types::IPFSHash, DagCacheError> {
        let mut url = self.0.clone();
        url.set_path("api/v0/object/put");
        url.query_pairs_mut().append_pair("datafieldenc", "base64");

        println!("hitting url: {:?}", &url);

        let v = DagNode {
            data: v.data,
            links: v
                .links
                .into_iter()
                .map(|ipfs_types::IPFSHeader { hash, name, size }| IPFSHeader { hash, name, size })
                .collect(),
        };
        let bytes = serde_json::to_vec(&v).expect("json _serialize_ failed (should be impossible)");

        println!("sending bytes: {:?}", &std::str::from_utf8(&bytes));

        let part = multipart::Part::bytes(bytes).file_name("data"); // or vice versa, idk
        let form = multipart::Form::new().part("file", part);

        let f = Client::new()
            .post(url.clone())
            .multipart(form)
            .send()
            .and_then(|mut x| x.json())
            .map(|IPFSPutResp { hash }| hash)
            .map_err(|e| {
                event!(Level::ERROR,  msg = "failed parsing json", response.error = ?e);
                DagCacheError::IPFSJsonError
            })
            .instrument(span!(Level::TRACE, "ipfs-put", url = ?url ));

        Box::new(f)
    }
}

// IPFS API resp types - lives here, not a huge fan of their json format - stays here
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct IPFSPutResp {
    pub hash: ipfs_types::IPFSHash,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct IPFSHeader {
    pub name: String,
    pub hash: ipfs_types::IPFSHash,
    pub size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DagNode {
    pub links: Vec<IPFSHeader>,
    pub data: encoding_types::Base64,
}

//NOTE: why is this here? FIXME
// exists primarily to have better serialized json (tuples result in 2-elem lists)
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DagNodeWithHash {
    pub hash: IPFSHeader,
    pub node: DagNode,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding_types::Base64;
    use crate::ipfs_types::{DagNode, IPFSHash, IPFSHeader};
    use rand;
    use rand::Rng;

    #[test]
    fn test_put_and_get() {
        let mut random_bytes = vec![];

        let mut rng = rand::thread_rng(); // faster if cached locally
        for _ in 0..64 {
            random_bytes.push(rng.gen())
        }

        // invalid hash pointer, should be fine
        let header = IPFSHeader {
            name: "foo".to_string(),
            // needs to be a valid IPFS hash (length, encoding bit, etc), so just use a randomly-chosen one
            hash: IPFSHash::from_string("QmVC1ZwqPxSzs1KyrSJdgF1zfEFTNGwBGRadx5aEfJV6Q9").unwrap(),
            size: 1337,
        };
        let input = DagNode {
            links: vec![header],
            data: Base64(random_bytes),
        };

        let ipfs_node = IPFSNode::new(reqwest::Url::parse("http://localhost:5001").unwrap());

        //dag nodes should be equivalent - shows round-trip get/put using this IPFS service impl
        let f = ipfs_node
            .put(input.clone())
            .map_err(|e| format!("ipfs put error: {:?}", e))
            .and_then(move |input_hash| {
                ipfs_node
                    .get(input_hash)
                    .map_err(|e| format!("ipfs get error: {:?}", e))
            })
            .map(move |output| {
                assert_eq!(input, output);
                ()
            });

        run_test(Box::new(f))
    }

    // TODO: could take list of futures and be top level test runner.. (better than one tokio runtime per test case)
    fn run_test<E: std::fmt::Debug + Send + Sync + 'static>(f: BoxFuture<(), E>) {
        // initialize and register event/span logging subscriber
        let subscriber = tracing_subscriber::fmt::Subscriber::builder().finish();
        tracing::subscriber::set_global_default(subscriber).expect("setting global default failed");


        let (send, receive) = futures::sync::oneshot::channel();
        let f = f.then(|res| {send.send(res).unwrap(); Ok(())});

        tokio::run(f);

        if let Err(e) = receive.wait().unwrap() {
            panic!("test failed with error {:?}", e);
        };
    }

}
