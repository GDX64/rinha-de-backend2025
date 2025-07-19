use crate::{
    app_state::{SummaryQuery, WrappedState},
    database::PaymentPost,
    processing::create_worker,
};
use axum::{
    Json, Router, debug_handler,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::Instrument;

mod app_state;
mod database;
mod processing;

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .pretty()
        .init();

    let (sender, receiver) = tokio::sync::mpsc::channel(100_000);
    let sibling_service_url = std::env::var("SIBLING_SERVICE_URL").ok();
    let state = WrappedState::new(sender, sibling_service_url);
    create_worker(receiver, state.clone());

    // build our application with a route
    let app = Router::new()
        // `GET /` goes to `root`
        .route("/payments-summary", get(summary))
        .route("/siblings-summary", get(sibling_summary))
        // `POST /users` goes to `create_user`
        .route("/payments", post(payments))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9999").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn sibling_summary(
    State(state): State<WrappedState>,
    Query(query_data): Query<SummaryQuery>,
) -> (StatusCode, Json<Value>) {
    let span = tracing::info_span!("sibling_summary", from = ?query_data.from, to = ?query_data.to);
    let _guard = span.enter();
    let stats = state.get_state(&query_data);

    let Ok(stats) = stats else {
        tracing::error!("Failed to get stats: {:?}", stats.err());
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::Null));
    };

    let json = stats.to_json();
    // tracing::info!("Returning summary: {:?}", json);
    return (StatusCode::OK, Json(json));
}

async fn summary(
    State(state): State<WrappedState>,
    Query(query_data): Query<SummaryQuery>,
) -> (StatusCode, Json<Value>) {
    let span = tracing::info_span!("summary", from = ?query_data.from, to = ?query_data.to);
    let stats = span.in_scope(|| state.get_state(&query_data));

    let Ok(mut stats) = stats else {
        tracing::error!("Failed to get stats: {:?}", stats.err());
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::Null));
    };

    let merge_result = state
        .merge_state_with_sibling(&query_data, &mut stats)
        .instrument(span)
        .await;

    if let Err(err) = merge_result {
        tracing::error!("Failed to merge state with sibling: {:?}", err);
    }

    let json = stats.to_json();
    // tracing::info!("Returning summary: {:?}", json);
    return (StatusCode::OK, Json(json));
}

#[debug_handler]
async fn payments(
    State(state): State<WrappedState>,
    Json(payload): Json<PaymentGet>,
) -> StatusCode {
    state.sender.send(payload).await.unwrap();
    return StatusCode::CREATED;
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct PaymentGet {
    #[serde(rename = "correlationId")]
    correlation_id: String,
    amount: f64,
}

impl PaymentGet {
    fn to_payment_post(&self) -> PaymentPost {
        let unix_now = chrono::Local::now();
        PaymentPost {
            correlation_id: self.correlation_id.clone(),
            amount: self.amount,
            requested_at: unix_now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            requested_at_ts: unix_now.timestamp_millis(),
        }
    }
}
