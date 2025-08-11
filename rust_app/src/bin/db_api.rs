use axum::{
    Json, Router,
    body::Bytes,
    debug_handler,
    extract::{Query, State},
    http::StatusCode,
    routing::{any, get, post},
};
use rinha::{
    app_state::{SummaryQuery, WrappedState},
    database::PaymentPost,
};
use serde_json::Value;
use tracing::instrument;

#[tokio::main()]
async fn main() {
    // initialize tracing
    // tracing_subscriber::fmt()
    //     .with_max_level(tracing::Level::ERROR)
    //     .with_ansi(false)
    //     .pretty()
    //     .init();

    let state = WrappedState::default();
    state.init(true).await;

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
    let stats = state.get_state(&query_data);
    let Ok(stats) = stats else {
        tracing::error!("Failed to get stats: {:?}", stats.err());
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::Null));
    };

    let json = stats.to_json();
    return (StatusCode::OK, Json(json));
}

#[debug_handler]
async fn payments(State(state): State<WrappedState>, bytes: Bytes) -> StatusCode {
    let Ok(_) = state.with_state(|state| state.ws.try_send(bytes)) else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    return StatusCode::CREATED;
}
