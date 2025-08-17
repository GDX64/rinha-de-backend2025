use axum::{
    Json, Router,
    body::Bytes,
    debug_handler,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use rinha::{
    app_state::{SummaryQuery, WrappedState},
    database::PaymentPost,
    internal_socket::create_server,
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
    let uds_db_socket = std::env::var("DB_UDS_PATH").expect("DB_UDS_PATH not set");
    let other = state.clone();
    create_server(&uds_db_socket, move |ws| {
        other.clone().on_websocket(ws);
    });
    state.init(true);

    let app = Router::new()
        .route("/payments-summary", get(summary))
        .route("/payments", post(payments))
        .route("/db-save", post(db_save))
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
