// #![deny(warnings)]

use dag_cache_types::types::ipfs;
use dag_cache_types::types::api::get;
use dag_cache_types::types::grpc::{self, client::IpfsCacheClient};
use futures::future::FutureExt;
use serde::Serialize;
use warp::{reject, Filter};

// TODO: struct w/ domain types & etc
#[derive(Debug)]
struct Error(Box<dyn std::error::Error + Send + Sync + 'static>);

impl reject::Reject for Error {}

/// A serialized message to report in JSON format.
#[derive(Serialize)]
struct ErrorMessage<'a> {
    code: u16,
    message: &'a str,
}

#[tokio::main]
async fn main() {
    let get_route = warp::path("node")
        .and(warp::path::param::<String>())
        .and_then(|raw_hash: String| {
            let f = async move {
                println!("parsed hash {} from path", raw_hash);

                let mut client = IpfsCacheClient::connect("http://localhost:8088")
                    .await
                    .map_err(|e| Box::new(e))?;

                // TODO: validate base58 here
                let request = tonic::Request::new(grpc::IpfsHash { hash: raw_hash });

                let response = client.get_node(request).await.map_err(|e| Box::new(e))?;

                let response =
                    get::Resp::from_proto(response.into_inner()).map_err(|e| Box::new(e))?;

                let response = notes_types::GetResp::from_generic(response)?;

                let resp = warp::reply::json(&response);
                Ok::<_, Box<dyn std::error::Error + Send + Sync + 'static>>(resp)
            };

            f.map(|x| x.map_err(|e| reject::custom::<Error>(Error(e))))
        });

    // TODO: impl put via the above w/ addition of parsing body as json - also wire into frontend

    // note: first path segment duplicated
    let post_route = warp::post()
        .and(warp::path("node"))
        .and(warp::body::content_length_limit(1024 * 16)) // arbitrary?
        .and(warp::body::json())
        .and_then(|put_req: notes_types::PutReq| {
            let f = async move {
                println!("got req {:?} body", put_req);

                let put_req = put_req.into_generic()?;

                // TODO: better mgmt for grpc port/host
                let mut client = IpfsCacheClient::connect("http://localhost:8088")
                    .await
                    .map_err(|e| Box::new(e))?;

                // TODO: validate base58 here
                let request = tonic::Request::new(put_req.into_proto());

                let response = client.put_nodes(request).await.map_err(|e| Box::new(e))?;

                let response =
                    ipfs::IPFSHash::from_proto(response.into_inner()).map_err(|e| Box::new(e))?;

                let resp = warp::reply::json(&response);
                Ok::<_, Box<dyn std::error::Error + Send + Sync + 'static>>(resp)
            };

            f.map(|x| x.map_err(|e| reject::custom::<Error>(Error(e))))
        });

    let routes = get_route.or(post_route);

    warp::serve(routes).run(([127, 0, 0, 1], 3030)).await;
}
