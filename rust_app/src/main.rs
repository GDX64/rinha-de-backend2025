use axum::{
    Json, Router, debug_handler,
    extract::{Query, State},
    http::StatusCode,
    routing::{get, post},
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tokio::select;
use tracing::{Instrument, instrument};

use crate::database::{PaymentPost, PaymentsDb, Stats};

mod database;

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
    tracing::info!("Returning summary: {:?}", json);
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
    tracing::info!("Returning summary: {:?}", json);
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

async fn try_process_payment(payload: PaymentGet, state: WrappedState) -> PaymentTryResult {
    let payment_post = payload.to_payment_post();
    let mut is_retry = false;
    loop {
        let res = send_to_service(
            payment_post.clone(),
            &default_service_url(),
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
            &fallback_service_url(),
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

fn fallback_service_url() -> String {
    std::env::var("FALLBACK_PAYMENT").unwrap_or_else(|_| "http://localhost:8002".to_string())
}

fn default_service_url() -> String {
    std::env::var("DEFAULT_PAYMENT").unwrap_or_else(|_| "http://localhost:8001".to_string())
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
    sibling_service_url: Option<String>,
}

#[derive(Clone)]
struct WrappedState {
    state: Arc<Mutex<AppState>>,
    client: Client,
    sender: tokio::sync::mpsc::Sender<PaymentGet>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
struct SummaryQuery {
    from: Option<String>,
    to: Option<String>,
}

impl WrappedState {
    fn new(
        sender: tokio::sync::mpsc::Sender<PaymentGet>,
        sibling_service_url: Option<String>,
    ) -> Self {
        WrappedState {
            state: Arc::new(Mutex::new(AppState {
                db: PaymentsDb::new().expect("Failed to initialize database"),
                sibling_service_url,
            })),
            client: Client::new(),
            sender,
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

    fn get_state(&self, query_data: &SummaryQuery) -> anyhow::Result<Stats> {
        let start = query_data
            .from
            .clone()
            .unwrap_or("1970-01-01T00:00:00.000Z".to_string());
        let end = query_data
            .to
            .clone()
            .unwrap_or("9999-12-31T23:59:59.999Z".to_string());

        let values = self.state.lock().unwrap().db.get_stats(&start, &end)?;

        Ok(values)
    }

    async fn merge_state_with_sibling(
        &self,
        query_data: &SummaryQuery,
        values: &mut Stats,
    ) -> anyhow::Result<()> {
        let sibling_service_url = self.state.lock().unwrap().sibling_service_url.clone();
        let start = query_data
            .from
            .clone()
            .unwrap_or("1970-01-01T00:00:00.000Z".to_string());
        let end = query_data
            .to
            .clone()
            .unwrap_or("9999-12-31T23:59:59.999Z".to_string());

        if let Some(url) = sibling_service_url {
            let client = self.client.clone();
            let res = client
                .get(format!(
                    "{}/siblings-summary?from={}&to={}",
                    url, start, end
                ))
                .send()
                .await?
                .error_for_status()?;
            let json: Value = res.json().await?;
            let def = &json["default"];
            let def_total = def["total"].as_f64().unwrap_or(0.0);
            let def_count = def["count"].as_u64().unwrap_or(0) as usize;
            let fallback = &json["fallback"];
            let fallback_total = fallback["total"].as_f64().unwrap_or(0.0);
            let fallback_count = fallback["count"].as_u64().unwrap_or(0) as usize;

            values.fallback_total += def_total;
            values.fallback_count += def_count;
            values.default_total += fallback_total;
            values.default_count += fallback_count;
            return Ok(());
        };
        return Err(anyhow::anyhow!("Sibling service URL is not set"));
    }
}

fn create_worker(mut receiver: tokio::sync::mpsc::Receiver<PaymentGet>, state: WrappedState) {
    tokio::spawn(async move {
        while let Some(payload) = receiver.recv().await {
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
        }
    });
}

enum PaymentTryResult {
    CheapOk(PaymentPost),
    FallbackOk(PaymentPost),
}
