use std::sync::{Arc, Mutex};

use axum::{
    Json, Router, debug_handler,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::select;
use tracing::{Instrument, instrument};

const CHEAP_SERVICE_URL: &str = "http://localhost:8001/payments";
const FALLBACK_SERVICE_URL: &str = "http://localhost:8002/payments";

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let state = WrappedState::new();

    // build our application with a route
    let app = Router::new()
        // `GET /` goes to `root`
        .route("/payments-summary", get(summary))
        // `POST /users` goes to `create_user`
        .route("/payments", post(payments))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9999").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

// basic handler that responds with a static string
async fn summary(State(state): State<WrappedState>) -> (StatusCode, Json<Value>) {
    let state = state.get_state();
    let json = serde_json::json!({
        "default":{
            "totalRequests": state.total_requests_cheap,
            "totalAmount": state.total_amount_cheap,
        },
        "fallback":{
            "totalRequests": state.total_requests_fallback,
            "totalAmount": state.total_amount_fallback,
        }
    });
    return (StatusCode::OK, Json(json));
}

#[debug_handler]
async fn payments(State(state): State<WrappedState>, Json(payload): Json<Payment>) -> StatusCode {
    let span = tracing::info_span!("payments", correlationId = payload.correlationId);

    span.in_scope(|| tracing::info!("sending request to cheap service"));
    let res = send_to_service(payload.clone(), CHEAP_SERVICE_URL)
        .instrument(span.clone())
        .await;

    let Err(_res) = res else {
        state.add_payment_cheap(payload.amount);
        return StatusCode::CREATED;
    };

    span.in_scope(|| tracing::info!("sending request to fallback service"));
    let res = send_to_service(payload.clone(), FALLBACK_SERVICE_URL)
        .instrument(span)
        .await;

    let Err(_res) = res else {
        state.add_payment_fallback(payload.amount);
        return StatusCode::CREATED;
    };

    return StatusCode::INTERNAL_SERVER_ERROR;
}

#[instrument]
async fn send_to_service(payload: Payment, url: &str) -> anyhow::Result<StatusCode> {
    let client = reqwest::Client::new();
    tracing::info!("Sending request to {}", url);
    // Example: POST to another service
    let res = client.post(url).json(&payload).send();
    let timeout = tokio::time::sleep(std::time::Duration::from_millis(500));
    let res = select! {
        res = res => res,
        _ = timeout => {
            tracing::warn!("the request timed out");
            return Err(anyhow::anyhow!("Request to {} timed out", url));
        }
    };

    let res = res.and_then(|res| res.error_for_status());
    match res {
        Ok(_res) => {
            tracing::info!("payment success");
            // println!("Response from external service: {:?}", res);
            return Ok(StatusCode::CREATED);
        }
        Err(err) => {
            tracing::warn!("payment error: {:?}", err.status());
            // println!("Error sending request: {:?}", err);
            anyhow::bail!("Failed to send request to external service");
        }
    }
}

// the input to our `create_user` handler
#[derive(Deserialize, Serialize, Debug, Clone)]
struct Payment {
    correlationId: String,
    amount: f64,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct AppState {
    total_requests_cheap: u64,
    total_requests_fallback: u64,
    total_amount_cheap: f64,
    total_amount_fallback: f64,
}

#[derive(Clone)]
struct WrappedState {
    state: Arc<Mutex<AppState>>,
}

impl WrappedState {
    fn new() -> Self {
        WrappedState {
            state: Arc::new(Mutex::new(AppState {
                total_requests_cheap: 0,
                total_requests_fallback: 0,
                total_amount_cheap: 0.0,
                total_amount_fallback: 0.0,
            })),
        }
    }

    fn add_payment_cheap(&self, amount: f64) {
        let mut state = self.state.lock().unwrap();
        state.total_requests_cheap += 1;
        state.total_amount_cheap += amount;
    }

    fn add_payment_fallback(&self, amount: f64) {
        let mut state = self.state.lock().unwrap();
        state.total_requests_fallback += 1;
        state.total_amount_fallback += amount;
    }

    fn get_state(&self) -> AppState {
        let state = self.state.lock().unwrap();
        state.clone()
    }
}
