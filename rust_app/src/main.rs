use crate::{
    app_state::{SummaryQuery, WrappedState},
    database::PaymentPost,
};
use axum::{
    Json, Router,
    body::Bytes,
    debug_handler,
    extract::{Query, State},
    http::StatusCode,
    routing::{any, get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::instrument;

mod app_state;
mod database;
mod processing;
mod stealing_queue;

#[tokio::main()]
async fn main() {
    // initialize tracing
    // tracing_subscriber::fmt()
    //     .with_max_level(tracing::Level::ERROR)
    //     .with_ansi(false)
    //     .pretty()
    //     .init();

    let state = WrappedState::new(is_db_service())
        .await
        .expect("Failed to create WrappedState");

    let app = Router::new()
        .route("/payments-summary", get(summary))
        .route("/payments", post(payments))
        .route("/db-save", post(db_save))
        .route("/ws", any(websocket_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            tokio::signal::ctrl_c()
                .await
                .expect("Failed to listen for shutdown signal");
        })
        .await
        .unwrap();
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

async fn websocket_handler(
    State(state): State<WrappedState>,
    ws: axum::extract::ws::WebSocketUpgrade,
) -> axum::response::Response {
    println!("WebSocket connection requested");
    return ws.on_upgrade(move |socket| {
        return state.on_websocket(socket);
    });
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
async fn payments(State(state): State<WrappedState>, bytes: Bytes) -> StatusCode {
    let Ok(_) = state.ws.try_send(bytes) else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    return StatusCode::CREATED;
}

fn is_db_service() -> bool {
    std::env::var("IS_DB_SERVICE")
        .map(|v| v == "true")
        .unwrap_or(false)
}
