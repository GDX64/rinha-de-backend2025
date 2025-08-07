use axum::http::{self, method};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rinha::PaymentGet;
use rinha::app_state::WrappedState;
use std::net::SocketAddr;
use std::sync::LazyLock;
use tokio::net::TcpListener;

type BoxBodyType = Response<BoxBody<Bytes, hyper::Error>>;
type IncomingRequest = Request<hyper::body::Incoming>;

static GLOBAL_STATE: LazyLock<WrappedState> = LazyLock::new(|| return WrappedState::new(false));

async fn hello(req: IncomingRequest) -> Result<BoxBodyType, hyper::Error> {
    let path = req.uri().path();
    println!("Received request for path: {}", path);
    match path {
        "/payments" => Ok(handle_payments(req).await),
        "/payments-summary" => Ok(Response::new(full(Bytes::from("Hello, World!")))),
        _ => {
            let mut res = Response::new(empty());
            *res.status_mut() = http::StatusCode::NOT_FOUND;

            Ok(res)
        }
    }
}

async fn handle_payments(req: IncomingRequest) -> BoxBodyType {
    if req.method() != method::Method::POST {
        let mut res = Response::new(empty());
        *res.status_mut() = http::StatusCode::METHOD_NOT_ALLOWED;
        return res;
    }
    let s = &GLOBAL_STATE;

    let c = req.into_body().collect().await;
    match c {
        Ok(body) => {
            let json: PaymentGet = serde_json::from_slice(&body.to_bytes()).unwrap();
            println!("Received payment: {:?}", json);
            let res = Response::new(empty());
            return res;
        }
        Err(err) => {
            eprintln!("Error reading request body: {}", err);
            let res = Response::new(empty());
            return res;
        }
    }
}

#[derive(Clone)]
pub struct TokioExecutor;

impl<F> hyper::rt::Executor<F> for TokioExecutor
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    fn execute(&self, fut: F) {
        tokio::task::spawn(fut);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], 3000));

    let listener = TcpListener::bind(addr).await?;

    loop {
        let (stream, _) = listener.accept().await?;
        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(io, service_fn(hello))
                .await
            {
                eprintln!("Error serving connection: {}", err);
            }
        });
    }
}

fn empty() -> BoxBody<Bytes, hyper::Error> {
    Empty::<Bytes>::new()
        .map_err(|never| match never {})
        .boxed()
}
fn full<T: Into<Bytes>>(chunk: T) -> BoxBody<Bytes, hyper::Error> {
    Full::new(chunk.into())
        .map_err(|never| match never {})
        .boxed()
}
