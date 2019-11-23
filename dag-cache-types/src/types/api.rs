#[cfg(feature = "grpc")]
use crate::types::errors::ProtoDecodingError;
#[cfg(feature = "grpc")]
use crate::types::grpc;
use serde::{Deserialize, Serialize};
#[cfg(feature = "grpc")]
use std::collections::HashMap;

#[derive(PartialEq, Hash, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct ClientId(pub String); // string? u128? idk
impl ClientId {
    pub fn new(x: String) -> ClientId {
        ClientId(x)
    }

    #[cfg(feature = "grpc")]
    pub fn from_proto(p: grpc::ClientId) -> Result<Self, ProtoDecodingError> {
        Ok(ClientId(p.hash)) // TODO: validation?
    }

    #[cfg(feature = "grpc")]
    pub fn into_proto(self) -> grpc::ClientId {
        grpc::ClientId { hash: self.0 }
    }
}

impl std::fmt::Display for ClientId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

pub mod bulk_put {
    use super::*;
    use crate::types::encodings::Base64;
    use crate::types::ipfs;
    use crate::types::validated_tree::ValidatedTree;

    #[derive(PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
    pub struct Resp {
        pub root_hash: ipfs::IPFSHash,
        pub additional_uploaded: Vec<(ClientId, ipfs::IPFSHash)>,
    }

    #[cfg(feature = "grpc")]
    impl Resp {
        pub fn into_proto(self) -> grpc::BulkPutResp {
            grpc::BulkPutResp {
                root_hash: Some(self.root_hash.into_proto()),
                additional_uploaded: self
                    .additional_uploaded
                    .into_iter()
                    .map(|x| grpc::BulkPutRespPair {
                        client_id: Some(x.0.into_proto()),
                        hash: Some(x.1.into_proto()),
                    })
                    .collect(),
            }
        }

        pub fn from_proto(p: grpc::BulkPutResp) -> Result<Self, ProtoDecodingError> {
            let root_hash = p.root_hash.ok_or(ProtoDecodingError {
                cause: "root hash not present on Bulk Put Resp proto".to_string(),
            })?;
            let root_hash = ipfs::IPFSHash::from_proto(root_hash)?;

            let additional_uploaded: Result<Vec<(ClientId, ipfs::IPFSHash)>, ProtoDecodingError> =
                p.additional_uploaded
                    .into_iter()
                    .map(|bp| {
                        let client_id = bp.client_id.ok_or(ProtoDecodingError {
                            cause: "client_id not present on Bulk Put Resp proto pair".to_string(),
                        })?;
                        let client_id = ClientId::from_proto(client_id)?;

                        let hash = bp.hash.ok_or(ProtoDecodingError {
                            cause: "hash not present on Bulk Put Resp proto pair".to_string(),
                        })?;
                        let hash = ipfs::IPFSHash::from_proto(hash)?;
                        Ok((client_id, hash))
                    })
                    .collect();
            let additional_uploaded = additional_uploaded?;

            Ok(Resp {
                root_hash,
                additional_uploaded,
            })
        }
    }

    // idea is that a put req will contain some number of nodes, with only client-side blake hashing performed.
    // all hash links in body will solely use blake hash. ipfs is then treated as an implementation detail
    // with parsing-time traverse op to pair each blake hash with the full ipfs hash from the links field
    // of the dag node - dag node link fields would use name = blake2 hash (base58) (same format, why not, eh?)
    // goal of all this is to be able to send fully packed large requests as lists of many nodes w/ just blake2 pointers
    #[derive(Debug)]
    pub struct Req {
        pub validated_tree: ValidatedTree,
    }

    #[cfg(feature = "grpc")]
    impl Req {
        pub fn into_proto(self) -> grpc::BulkPutReq {
            let root_node = self.validated_tree.root_node.into_proto();

            let nodes = self
                .validated_tree
                .nodes
                .into_iter()
                .map(|(id, n)| grpc::BulkPutIpfsNodeWithHash {
                    node: Some(n.into_proto()),
                    client_side_hash: Some(id.into_proto()),
                })
                .collect();

            grpc::BulkPutReq {
                root_node: Some(root_node),
                nodes,
            }
        }

        pub fn from_proto(p: grpc::BulkPutReq) -> Result<Self, ProtoDecodingError> {
            let root_node = p.root_node.ok_or(ProtoDecodingError {
                cause: "root node not present on Bulk Put Req proto".to_string(),
            })?;
            let root_node = DagNode::from_proto(root_node)?;

            let nodes: Result<Vec<DagNodeWithHash>, ProtoDecodingError> = p
                .nodes
                .into_iter()
                .map(DagNodeWithHash::from_proto)
                .collect();
            let nodes = nodes?;

            let mut node_map = HashMap::with_capacity(nodes.len());

            for DagNodeWithHash { hash, node } in nodes.into_iter() {
                node_map.insert(hash, node);
            }

            let validated_tree =
                ValidatedTree::validate(root_node, node_map).map_err(|e| ProtoDecodingError {
                    cause: format!("invalid tree provided in Bulk Put Req proto, {:?}", e),
                })?;

            Ok(Req { validated_tree })
        }
    }

    #[derive(Clone, Debug)]
    pub struct DagNodeWithHash {
        pub hash: ClientId,
        pub node: DagNode,
    }

    impl DagNodeWithHash {
        #[cfg(feature = "grpc")]
        pub fn from_proto(p: grpc::BulkPutIpfsNodeWithHash) -> Result<Self, ProtoDecodingError> {
            let hash = p.client_side_hash.ok_or(ProtoDecodingError {
                cause: "client side hash not present on BulkPutIpfsNodeWithHash proto".to_string(),
            })?;

            let hash = ClientId::from_proto(hash)?;

            let node = p.node.ok_or(ProtoDecodingError {
                cause: "node not present on BulkPutIpfsNodeWithHash proto".to_string(),
            })?;
            let node = DagNode::from_proto(node)?;
            Ok(DagNodeWithHash { hash, node })
        }
    }

    #[derive(Clone, Debug)]
    pub struct DagNode {
        pub links: Vec<DagNodeLink>, // list of pointers - either to elems in this bulk req or already-uploaded
        pub data: Base64,            // this node's data
    }

    #[cfg(feature = "grpc")]
    impl DagNode {
        pub fn from_proto(p: grpc::BulkPutIpfsNode) -> Result<Self, ProtoDecodingError> {
            let data = Base64(p.data);

            let links: Result<Vec<DagNodeLink>, ProtoDecodingError> =
                p.links.into_iter().map(DagNodeLink::from_proto).collect();
            let links = links?;
            Ok(DagNode { links, data })
        }

        pub fn into_proto(self) -> grpc::BulkPutIpfsNode {
            grpc::BulkPutIpfsNode {
                data: self.data.0,
                links: self.links.into_iter().map(|x| x.into_proto()).collect(),
            }
        }
    }

    #[derive(Clone, Debug)]
    pub enum DagNodeLink {
        Local(ClientId),
        Remote(ipfs::IPFSHeader),
    }

    #[cfg(feature = "grpc")]
    impl DagNodeLink {
        pub fn into_proto(self) -> grpc::BulkPutLink {
            let link = match self {
                DagNodeLink::Local(id) => grpc::bulk_put_link::Link::InReq(id.into_proto()),

                DagNodeLink::Remote(hdr) => grpc::bulk_put_link::Link::InIpfs(hdr.into_proto()),
            };
            grpc::BulkPutLink { link: Some(link) }
        }

        pub fn from_proto(p: grpc::BulkPutLink) -> Result<Self, ProtoDecodingError> {
            match p.link {
                Some(grpc::bulk_put_link::Link::InIpfs(hdr)) => {
                    ipfs::IPFSHeader::from_proto(hdr).map(DagNodeLink::Remote)
                }
                Some(grpc::bulk_put_link::Link::InReq(csh)) => {
                    let csh = ClientId::from_proto(csh)?;
                    Ok(DagNodeLink::Local(csh))
                }
                None => Err(ProtoDecodingError {
                    cause: "no value for bulk put link oneof".to_string(),
                }),
            }
        }
    }
}

pub mod get {
    use super::*;
    use crate::types::ipfs;

    // ~= NonEmptyList (head, rest struct)
    #[derive(Serialize, Deserialize, Clone, Debug)]
    pub struct Resp {
        pub requested_node: ipfs::DagNode,
        pub extra_node_count: u64,
        pub extra_nodes: Vec<ipfs::DagNodeWithHeader>,
    }

    impl Resp {
        #[cfg(feature = "grpc")]
        pub fn from_proto(p: grpc::GetResp) -> Result<Self, ProtoDecodingError> {
            let extra_nodes: Result<Vec<ipfs::DagNodeWithHeader>, ProtoDecodingError> = p
                .extra_nodes
                .into_iter()
                .map(|n| ipfs::DagNodeWithHeader::from_proto(n))
                .collect();
            let extra_nodes = extra_nodes?;

            let requested_node = p.requested_node.ok_or(ProtoDecodingError {
                cause: "missing requested_node".to_string(),
            })?;
            let requested_node = ipfs::DagNode::from_proto(requested_node)?;

            let res = Self {
                extra_node_count: p.extra_node_count,
                requested_node,
                extra_nodes,
            };
            Ok(res)
        }

        #[cfg(feature = "grpc")]
        pub fn into_proto(self) -> grpc::GetResp {
            grpc::GetResp {
                requested_node: Some(self.requested_node.into_proto()),
                extra_node_count: self.extra_node_count,
                extra_nodes: self
                    .extra_nodes
                    .into_iter()
                    .map(|x| x.into_proto())
                    .collect(),
            }
        }
    }
}