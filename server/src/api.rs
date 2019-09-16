use actix_web::web;
use futures::future;
use futures::future::Future;

use std::collections::VecDeque;

use crate::api_types;
use crate::in_mem_types;
use crate::ipfs_types;

use std::sync::Arc;

use tokio;

use crate::api_types::ClientSideHash;
use crate::cache::CacheCapability;
use crate::capabilities::{HasCacheCap, HasIPFSCap};
use crate::ipfs_api::IPFSCapability;
use crate::lib::BoxFuture;
use std::convert::AsRef;
use tracing::{info, span, Level};

use futures::sync::oneshot;

pub fn get<C: 'static + HasIPFSCap + HasCacheCap>(
    caps: web::Data<C>,
    k: web::Path<(ipfs_types::IPFSHash)>,
) -> Box<dyn Future<Item = web::Json<api_types::get::Resp>, Error = api_types::DagCacheError>> {
    let caps = caps.into_inner(); // just copy arc, lmao

    let span = span!(Level::TRACE, "dag cache get handler");
    let _enter = span.enter();
    info!("attempt cache get");
    let k = k.into_inner();
    match caps.cache_caps().get(k.clone()) {
        Some(dag_node) => {
            info!("cache hit");
            // see if have any of the referenced subnodes in the local cache
            let resp = extend(caps.as_ref(), dag_node);
            Box::new(future::ok(web::Json(resp)))
        }
        None => {
            info!("cache miss");
            let f =
                // caps.as_ref()
                //     .ipfs_get(k.clone())
                caps
                .ipfs_caps()
                .get(k.clone())
                    .and_then(move |dag_node: ipfs_types::DagNode| {
                        info!("writing result of post cache miss lookup to cache");
                        caps.cache_caps().put(k.clone(), dag_node.clone());
                        // see if have any of the referenced subnodes in the local cache
                        let resp = extend(caps.as_ref(), dag_node);
                        Ok(web::Json(resp))
                    });
            Box::new(f)
        }
    }
}

// TODO: figure out traversal termination strategy - don't want to return whole cache in one resp
fn extend<C: 'static + HasCacheCap>(caps: &C, node: ipfs_types::DagNode) -> api_types::get::Resp {
    let mut frontier = VecDeque::new();
    let mut res = Vec::new();

    for hp in node.links.iter() {
        // iter over ref
        frontier.push_back(hp.clone());
    }

    // explore the frontier of potentially cached hash pointers
    while let Some(hp) = frontier.pop_front() {
        // if a hash pointer is in the cache, grab the associated node and continue traversal
        if let Some(dn) = caps.cache_caps().get(hp.hash.clone()) {
            // clone :(
            for hp in dn.links.iter() {
                // iter over ref
                frontier.push_back(hp.clone());
            }
            res.push(ipfs_types::DagNodeWithHeader {
                header: hp,
                node: dn,
            });
        }
    }

    api_types::get::Resp {
        requested_node: node,
        extra_node_count: res.len(),
        extra_nodes: res,
    }
}

pub fn put<C: 'static + HasCacheCap + HasIPFSCap>(
    caps: web::Data<C>,
    node: web::Json<ipfs_types::DagNode>,
) -> Box<dyn Future<Item = web::Json<ipfs_types::IPFSHash>, Error = api_types::DagCacheError>> {
    info!("dag cache put handler");
    let node = node.into_inner();

    let f = caps
        .ipfs_caps()
        .put(node.clone())
        .and_then(move |hp: ipfs_types::IPFSHash| {
            caps.cache_caps().put(hp.clone(), node);
            Ok(web::Json(hp))
        });
    Box::new(f)
}

pub fn put_many<C: 'static + HasCacheCap + HasIPFSCap + Sync + Send>(
    // TODO: figure out exactly what sync does
    caps: web::Data<C>,
    req: web::Json<api_types::bulk_put::Req>,
) -> BoxFuture<web::Json<ipfs_types::IPFSHeader>, api_types::DagCacheError> {
    // let caps = caps.into_inner(); // just copy arc, lmao

    info!("dag cache put handler");
    let api_types::bulk_put::Req { entry_point, nodes } = req.into_inner();
    let (csh, dctp) = entry_point;

    let mut node_map = std::collections::HashMap::with_capacity(nodes.len());

    for (k, v) in nodes.into_iter() {
        node_map.insert(k, v);
    }

    let in_mem = in_mem_types::DagNode::build(dctp, &mut node_map)
        .expect("todo: handle malformed req case here"); // FIXME

    // let (send, receive) = oneshot::channel();
    // tokio::spawn((send, caps.into_inner(), csh, in_mem));

    // let f = receive
    //     .map_err(|_| api_types::DagCacheError::UnexpectedError)
    //     .and_then(|res| match res {
    //         Ok(res) => future::ok(web::Json(res)),
    //         Err(err) => future::err(err),
    //     });
    let f = ipfs_publish_cata(caps.into_inner(), csh, in_mem).map(web::Json);

    Box::new(f)
}

// catamorphism - a consuming change
// recursively publish DAG node tree to IPFS, starting with leaf nodes
fn ipfs_publish_cata<C: 'static + HasCacheCap + HasIPFSCap + Sync + Send>(
    caps: Arc<C>,
    hash: ClientSideHash,
    node: in_mem_types::DagNode,
) -> impl Future<Item = ipfs_types::IPFSHeader, Error = api_types::DagCacheError> + 'static + Send {
    let (send, receive) = oneshot::channel();

    tokio::spawn(ipfs_publish_worker(caps, send, hash, node));

    receive
        .map_err(|_| api_types::DagCacheError::UnexpectedError) // one-shot channel cancelled
        .and_then(|res| match res {
            Ok(res) => future::ok(res),
            Err(err) => future::err(err),
        })
}

// worker thread - uses one-shot channel to return result to avoid unbounded stack growth
fn ipfs_publish_worker<C: 'static + HasCacheCap + HasIPFSCap + Sync + Send>(
    caps: Arc<C>,
    chan: oneshot::Sender<Result<ipfs_types::IPFSHeader, api_types::DagCacheError>>,
    hash: ClientSideHash,
    node: in_mem_types::DagNode,
) -> impl Future<Item = (), Error = ()> + 'static + Send {
    let in_mem_types::DagNode { data, links } = node;

    let link_fetches: Vec<_> = links
        .into_iter()
        .map({
            |x| -> BoxFuture<ipfs_types::IPFSHeader, api_types::DagCacheError> {
                match x {
                    in_mem_types::DagNodeLink::Local(hp, sn) => {
                        Box::new(ipfs_publish_cata(caps.clone(), hp, *sn))
                    }
                    in_mem_types::DagNodeLink::Remote(nh) => Box::new(futures::future::ok(nh)),
                }
            }
        })
        .collect();

    let joined_link_fetches = futures::future::join_all(link_fetches);

    joined_link_fetches
        .and_then(|links: Vec<ipfs_types::IPFSHeader>| {
            // might be a bit of an approximation, but w/e
            let size = data.0.len() as u64 + links.iter().map(|x| x.size).sum::<u64>();

            let dag_node = ipfs_types::DagNode { data, links };

            caps.as_ref()
                .ipfs_caps()
                .put(dag_node.clone())
                .then(move |res| match res {
                    Ok(hp) => {
                        caps.as_ref().cache_caps().put(hp.clone(), dag_node);
                        let hdr = ipfs_types::IPFSHeader {
                            name: hash.to_string(),
                            hash: hp,
                            size: size,
                        };

                        let chan_send_res = chan.send(Ok(hdr));
                        if let Err(err) = chan_send_res {
                            info!("failed oneshot channel send {:?}", err);
                        };
                        futures::future::ok(())
                    }
                    Err(err) => {
                        let chan_send_res = chan.send(Err(err));
                        if let Err(err) = chan_send_res {
                            info!("failed oneshot channel send {:?}", err);
                        };
                        futures::future::ok(())
                    }
                })
        })
        .map_err(|_| ())
}