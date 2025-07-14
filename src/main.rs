use std::sync::{Arc, Mutex};

use axum::{
    Json, Router, debug_handler,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::select;
use tracing::{Instrument, instrument};

use crate::database::{PaymentPost, PaymentsDb};

mod database;

const CHEAP_SERVICE_URL: &str = "http://localhost:8001/payments";
const FALLBACK_SERVICE_URL: &str = "http://localhost:8002/payments";

#[tokio::main]
async fn main() {
    // initialize tracing
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .pretty()
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
async fn summary(
    State(state): State<WrappedState>,
    Query(query_data): Query<SummaryQuery>,
) -> (StatusCode, Json<Value>) {
    let state = state.get_state(query_data);
    let json = serde_json::json!({
        // "default":{
        //     "totalRequests": state.total_requests_cheap,
        //     "totalAmount": state.total_amount_cheap,
        // },
        // "fallback":{
        //     "totalRequests": state.total_requests_fallback,
        //     "totalAmount": state.total_amount_fallback,
        // }
    });
    return (StatusCode::OK, Json(json));
}

#[debug_handler]
async fn payments(
    State(state): State<WrappedState>,
    Json(payload): Json<PaymentGet>,
) -> StatusCode {
    let span = tracing::info_span!("payments", correlationId = payload.correlation_id);
    let main_process = try_process_payment(payload.clone(), state.clone()).instrument(span.clone());
    let timeout = tokio::time::sleep(std::time::Duration::from_millis(1400));
    let res = select! {
        res = main_process => res,
        _ = timeout => {
            span.in_scope(||tracing::warn!("the request timed out"));
            return StatusCode::REQUEST_TIMEOUT;
        }
    };
    let _guard = span.enter();
    match res {
        PaymentTryResult::CheapOk(payment) => {
            state.add_payment_cheap(payment);
            tracing::info!("Payment processed by cheap service");
            return StatusCode::CREATED;
        }
        PaymentTryResult::FallbackOk(payment) => {
            state.add_payment_fallback(payment);
            tracing::info!("Payment processed by fallback service");
            return StatusCode::CREATED;
        }
    }
}

async fn try_process_payment(payload: PaymentGet, state: WrappedState) -> PaymentTryResult {
    let payment_post = payload.to_payment_post();
    loop {
        let res = send_to_service(
            payment_post.clone(),
            CHEAP_SERVICE_URL,
            state.client.clone(),
        )
        .await;

        let Err(_res) = res else {
            return PaymentTryResult::CheapOk(payment_post);
        };

        let res = send_to_service(
            payment_post.clone(),
            FALLBACK_SERVICE_URL,
            state.client.clone(),
        )
        .await;

        let Err(_res) = res else {
            return PaymentTryResult::FallbackOk(payment_post);
        };

        //cooldown before retrying
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

#[instrument(skip(payload, client))]
async fn send_to_service(
    payload: PaymentPost,
    url: &str,
    client: reqwest::Client,
) -> anyhow::Result<StatusCode> {
    let res = client.post(url).json(&payload).send();
    let timeout = tokio::time::sleep(std::time::Duration::from_millis(500));
    let res = select! {
        res = res => res,
        _ = timeout => {
            tracing::warn!("the service took too long to respond");
            return Err(anyhow::anyhow!("Request to {} timed out", url));
        }
    };

    let res = res.and_then(|res| res.error_for_status());
    match res {
        Ok(_res) => {
            tracing::info!("payment service success");
            // println!("Response from external service: {:?}", res);
            return Ok(StatusCode::CREATED);
        }
        Err(err) => {
            tracing::warn!("payment service error: {:?}", err);
            anyhow::bail!("Failed to send request to external service");
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct PaymentGet {
    #[serde(rename = "correlationId")]
    correlation_id: String,
    amount: f64,
}

impl PaymentGet {
    fn to_payment_post(&self) -> PaymentPost {
        PaymentPost {
            correlation_id: self.correlation_id.clone(),
            amount: self.amount,
            requested_at: chrono::Utc::now()
                .to_utc()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }
    }
}

struct AppState {
    db: PaymentsDb,
}

#[derive(Clone)]
struct WrappedState {
    state: Arc<Mutex<AppState>>,
    client: Client,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct SummaryQuery {
    from: Option<String>,
    to: Option<String>,
}

impl WrappedState {
    fn new() -> Self {
        WrappedState {
            state: Arc::new(Mutex::new(AppState {
                db: PaymentsDb::new().expect("Failed to initialize database"),
            })),
            client: Client::new(),
        }
    }

    fn add_payment_cheap(&self, payment: PaymentPost) {}

    fn add_payment_fallback(&self, payment: PaymentPost) {}

    fn get_state(&self, query_data: SummaryQuery) -> () {
        let start = query_data
            .from
            .unwrap_or("1970-01-01T00:00:00.000Z".to_string());
        let end = query_data
            .to
            .unwrap_or("9999-12-31T23:59:59.999Z".to_string());

        let start = chrono::DateTime::parse_from_rfc3339(&start).unwrap();
        let end = chrono::DateTime::parse_from_rfc3339(&end).unwrap();
    }
}

enum PaymentTryResult {
    CheapOk(PaymentPost),
    FallbackOk(PaymentPost),
}
