use axum::http::{self, method};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response};
use hyper_util::rt::TokioIo;
use rinha::app_state::{SummaryQuery, WrappedState};
use std::net::SocketAddr;
use std::sync::LazyLock;
use tokio::net::TcpListener;

type BoxBodyType = Response<BoxBody<Bytes, hyper::Error>>;
type IncomingRequest = Request<hyper::body::Incoming>;

static GLOBAL_STATE: LazyLock<WrappedState> = LazyLock::new(|| WrappedState::default());

async fn hello(req: IncomingRequest) -> Result<BoxBodyType, hyper::Error> {
    let path = req.uri().path();
    println!("Received request for path: {}", path);
    match path {
        "/payments" => Ok(handle_payments(req).await),
        "/payments-summary" => Ok(handle_summary(req).await),
        _ => {
            let mut res = Response::new(empty());
            *res.status_mut() = http::StatusCode::NOT_FOUND;

            Ok(res)
        }
    }
}

async fn handle_summary(req: IncomingRequest) -> BoxBodyType {
    if req.method() != method::Method::GET {
        let mut res = Response::new(empty());
        *res.status_mut() = http::StatusCode::METHOD_NOT_ALLOWED;
        return res;
    }
    let db_url = std::env::var("DB_URL").expect("DB_URL environment variable is not set");
    let s = &GLOBAL_STATE;
    let query_data: SummaryQuery = req.uri().query().expect("Query data is required").into();
    let value = s.get_from_db_service(&query_data, &db_url).await;
    let Ok(value) = value else {
        tracing::error!("Failed to get summary from DB service: {:?}", value.err());
        let mut res = Response::new(empty());
        *res.status_mut() = http::StatusCode::INTERNAL_SERVER_ERROR;
        return res;
    };
    let body = full(value.to_string());
    let res = Response::new(body);
    return res;
}

async fn handle_payments(req: IncomingRequest) -> BoxBodyType {
    if req.method() != method::Method::POST {
        let mut res = Response::new(empty());
        *res.status_mut() = http::StatusCode::METHOD_NOT_ALLOWED;
        return res;
    }

    let c = req.into_body().collect().await;
    match c {
        Ok(body) => {
            let bytes = body.to_bytes();
            let s = &GLOBAL_STATE;
            s.on_payment_received(bytes);
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
    {
        let s = &GLOBAL_STATE;
        s.init(false).await;
    }

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
