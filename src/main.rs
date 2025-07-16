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

use crate::database::{PaymentPost, PaymentsDb, Stats};

mod database;

const DEFAULT_SERVICE_URL: &str = "http://localhost:8001";
const FALLBACK_SERVICE_URL: &str = "http://localhost:8002";

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
    let span = tracing::info_span!("summary", from = ?query_data.from, to = ?query_data.to);
    let _guard = span.enter();
    let state = state.get_state(query_data);
    let Ok(stats) = state else {
        tracing::error!("Failed to get stats: {:?}", state.err());
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(Value::Null));
    };
    let json = serde_json::json!({
        "default":{
            "totalRequests": stats.default_count,
            "totalAmount": stats.default_total,
        },
        "fallback":{
            "totalRequests": stats.fallback_count,
            "totalAmount": stats.fallback_total,
        }
    });
    tracing::info!("Returning summary: {:?}", json);
    return (StatusCode::OK, Json(json));
}

#[debug_handler]
async fn payments(
    State(state): State<WrappedState>,
    Json(payload): Json<PaymentGet>,
) -> StatusCode {
    tokio::spawn(async move {
        let span = tracing::info_span!("payments", correlationId = payload.correlation_id);
        let res = try_process_payment(payload.clone(), state.clone())
            .instrument(span.clone())
            .await;
        let _guard = span.enter();
        match res {
            PaymentTryResult::CheapOk(payment) => {
                let result = state.add_payment_cheap(payment);
                if let Err(e) = result {
                    tracing::error!("Failed to insert payment: {:?}", e);
                } else {
                    tracing::info!("Payment processed by cheap service");
                };
            }
            PaymentTryResult::FallbackOk(payment) => {
                let result = state.add_payment_fallback(payment);
                if let Err(e) = result {
                    tracing::error!("Failed to insert payment: {:?}", e);
                } else {
                    tracing::info!("Payment processed by fallback service");
                };
            }
        }
    });
    return StatusCode::CREATED;
}

async fn try_process_payment(payload: PaymentGet, state: WrappedState) -> PaymentTryResult {
    let payment_post = payload.to_payment_post();
    let mut is_retry = true;
    loop {
        let res = send_to_service(
            payment_post.clone(),
            DEFAULT_SERVICE_URL,
            state.client.clone(),
            is_retry,
        )
        .await;

        match res {
            SendToServiceResult::Ok => {
                return PaymentTryResult::CheapOk(payment_post);
            }
            SendToServiceResult::AlreadyProcessed => {
                tracing::info!("Payment already processed by cheap service");
                return PaymentTryResult::CheapOk(payment_post);
            }
            SendToServiceResult::ErrRetry => {
                tracing::warn!("Retrying payment processing due to error");
            }
        };
        is_retry = true;

        let res = send_to_service(
            payment_post.clone(),
            FALLBACK_SERVICE_URL,
            state.client.clone(),
            is_retry,
        )
        .await;

        match res {
            SendToServiceResult::Ok => {
                return PaymentTryResult::FallbackOk(payment_post);
            }
            SendToServiceResult::AlreadyProcessed => {
                tracing::info!("Payment already processed by fallback service");
                return PaymentTryResult::FallbackOk(payment_post);
            }
            SendToServiceResult::ErrRetry => {
                tracing::warn!("Retrying payment processing due to error");
            }
        };

        //cooldown before retrying
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }
}

enum SendToServiceResult {
    Ok,
    AlreadyProcessed,
    ErrRetry,
}

#[instrument(skip(payload, client))]
async fn send_to_service(
    payload: PaymentPost,
    url: &str,
    client: reqwest::Client,
    is_retry: bool,
) -> SendToServiceResult {
    if is_retry {
        match get_post(&payload, client.clone(), url).await {
            GetPaymentResult::IsCreated => {
                return SendToServiceResult::AlreadyProcessed;
            }
            GetPaymentResult::Unknown => {}
        }
    };

    let post_url = format!("{}/payments", url);
    let res = client.post(post_url).json(&payload).send();
    let timeout = tokio::time::sleep(std::time::Duration::from_millis(1_000));
    let res = select! {
        res = res => res,
        _ = timeout => {
            tracing::warn!("the service took too long to respond");
            return SendToServiceResult::ErrRetry;
        }
    };

    let res = res.and_then(|res| res.error_for_status());
    match res {
        Ok(_res) => {
            tracing::info!("payment service success");
            return SendToServiceResult::Ok;
        }
        Err(err) => {
            tracing::warn!("payment service error: {:?}", err.status());
            return SendToServiceResult::ErrRetry;
        }
    }
}

enum GetPaymentResult {
    IsCreated,
    Unknown,
}
async fn get_post(post: &PaymentPost, client: Client, url: &str) -> GetPaymentResult {
    let url = format!("{}/payments/{}", url, post.correlation_id);
    let Ok(res) = client.get(&url).send().await else {
        return GetPaymentResult::Unknown;
    };
    match res.status() {
        StatusCode::OK => {
            return GetPaymentResult::IsCreated;
        }
        _ => return GetPaymentResult::Unknown,
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
        let unix_now = chrono::Local::now();
        PaymentPost {
            correlation_id: self.correlation_id.clone(),
            amount: self.amount,
            requested_at: unix_now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            requested_at_ts: unix_now.timestamp_millis(),
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

    fn add_payment_cheap(&self, payment: PaymentPost) -> anyhow::Result<()> {
        return self
            .state
            .lock()
            .unwrap()
            .db
            .insert_payment(&payment, database::PaymentKind::Default);
    }

    fn add_payment_fallback(&self, payment: PaymentPost) -> anyhow::Result<()> {
        return self
            .state
            .lock()
            .unwrap()
            .db
            .insert_payment(&payment, database::PaymentKind::Fallback);
    }

    fn get_state(&self, query_data: SummaryQuery) -> anyhow::Result<Stats> {
        let start = query_data
            .from
            .unwrap_or("1970-01-01T00:00:00.000Z".to_string());
        let end = query_data
            .to
            .unwrap_or("9999-12-31T23:59:59.999Z".to_string());

        let values = self.state.lock().unwrap().db.get_stats(&start, &end)?;
        Ok(values)
    }
}

enum PaymentTryResult {
    CheapOk(PaymentPost),
    FallbackOk(PaymentPost),
}
