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
use tracing::instrument;

mod app_state;
mod database;
mod processing;

#[tokio::main()]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::ERROR)
        .with_ansi(false)
        .pretty()
        .init();

    let (sender, receiver) = tokio::sync::mpsc::channel(100_000);
    let state = WrappedState::new(sender, is_db_service());

    if !is_db_service() {
        create_worker(receiver, state.clone());
    }

    let app = Router::new()
        .route("/payments-summary", get(summary))
        .route("/payments", post(payments))
        .route("/db-save", post(db_save))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn db_save(
    State(state): State<WrappedState>,
    Json(payload): Json<PaymentPost>,
) -> StatusCode {
    match state.add_on_db(payload) {
        Ok(_) => StatusCode::CREATED,
        Err(e) => {
            tracing::error!("Failed to save payment: {:?}", e);
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[instrument(skip(state))]
async fn summary(
    State(state): State<WrappedState>,
    Query(query_data): Query<SummaryQuery>,
) -> (StatusCode, Json<Value>) {
    if is_db_service() {
        let stats = state.get_state(&query_data);
        let Ok(stats) = stats else {
            tracing::error!("Failed to get stats: {:?}", stats.err());
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::Null));
        };

        let json = stats.to_json();
        return (StatusCode::OK, Json(json));
    } else {
        let db_url = std::env::var("DB_URL").expect("DB_URL environment variable is not set");
        let value = state.get_from_db_service(&query_data, &db_url).await;
        let Ok(value) = value else {
            tracing::error!("Failed to get summary from DB service: {:?}", value.err());
            return (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::Null));
        };
        return (StatusCode::OK, Json(value));
    }
}

#[debug_handler]
async fn payments(
    State(state): State<WrappedState>,
    Json(payload): Json<PaymentGet>,
) -> StatusCode {
    let Ok(_) = state.sender.try_send(payload) else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
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
            processed_on: None,
        }
    }
}

fn is_db_service() -> bool {
    std::env::var("IS_DB_SERVICE")
        .map(|v| v == "true")
        .unwrap_or(false)
}
