use reqwest::{Client, StatusCode};
use tokio::select;
use tracing::instrument;

use crate::{
    PaymentGet, WrappedState,
    database::{PaymentKind, PaymentPost},
};

struct RequestWorker {
    state: WrappedState,
    last_default_failure: chrono::DateTime<chrono::Utc>,
    last_fallback_failure: chrono::DateTime<chrono::Utc>,
    default_service_url: String,
    fallback_service_url: String,
}

const COOL_DOWN_MILLI: i64 = 1_000;
const TIME_BEFORE_RETRY_MILLI: u64 = 100;

impl RequestWorker {
    fn new(state: WrappedState) -> Self {
        fn fallback_service_url() -> String {
            std::env::var("FALLBACK_PAYMENT")
                .unwrap_or_else(|_| "http://localhost:8002".to_string())
        }

        fn default_service_url() -> String {
            std::env::var("DEFAULT_PAYMENT").unwrap_or_else(|_| "http://localhost:8001".to_string())
        }
        RequestWorker {
            state,
            last_default_failure: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(0)
                .unwrap(),
            last_fallback_failure: chrono::DateTime::<chrono::Utc>::from_timestamp_millis(0)
                .unwrap(),
            default_service_url: format!("{}/payments", default_service_url()),
            fallback_service_url: format!("{}/payments", fallback_service_url()),
        }
    }

    fn can_try_on_default(&self) -> bool {
        let now = chrono::Utc::now();
        let diff = now - self.last_default_failure;
        diff.num_milliseconds() > COOL_DOWN_MILLI
    }

    fn can_try_on_fallback(&self) -> bool {
        let now = chrono::Utc::now();
        let diff = now - self.last_fallback_failure;
        diff.num_milliseconds() > COOL_DOWN_MILLI
    }

    async fn try_process_payment(&mut self, payload: PaymentGet) -> PaymentTryResult {
        let payment_post = payload.to_payment_post();
        let mut is_retry = false;
        loop {
            if self.can_try_on_default() {
                let res = send_to_service(
                    &payment_post,
                    &self.default_service_url,
                    self.state.client.clone(),
                    is_retry,
                )
                .await;

                match res {
                    SendToServiceResult::Ok => {
                        return PaymentTryResult::CheapOk(payment_post);
                    }
                    SendToServiceResult::AlreadyProcessed => {
                        // tracing::info!("Payment already processed by cheap service");
                        return PaymentTryResult::CheapOk(payment_post);
                    }
                    SendToServiceResult::ErrRetry(e) => {
                        is_retry = true;
                        self.last_default_failure = chrono::Utc::now();
                        // tracing::warn!("Retrying payment processing due to error {}", e);
                    }
                };
            }

            if self.can_try_on_fallback() {
                let res = send_to_service(
                    &payment_post,
                    &self.fallback_service_url,
                    self.state.client.clone(),
                    is_retry,
                )
                .await;

                match res {
                    SendToServiceResult::Ok => {
                        return PaymentTryResult::FallbackOk(payment_post);
                    }
                    SendToServiceResult::AlreadyProcessed => {
                        // tracing::info!("Payment already processed by fallback service");
                        return PaymentTryResult::FallbackOk(payment_post);
                    }
                    SendToServiceResult::ErrRetry(e) => {
                        self.last_fallback_failure = chrono::Utc::now();
                        // tracing::warn!("Retrying payment processing due to error {}", e);
                    }
                };
            }

            //cooldown before retrying
            tokio::time::sleep(std::time::Duration::from_millis(TIME_BEFORE_RETRY_MILLI)).await;
        }
    }

    async fn payment_loop(mut self, mut recv: tokio::sync::mpsc::Receiver<PaymentGet>) {
        while let Some(payload) = recv.recv().await {
            let res = self.try_process_payment(payload).await;
            let payment = match res {
                PaymentTryResult::CheapOk(mut payment) => {
                    payment.processed_on = Some(PaymentKind::Default);
                    payment
                }
                PaymentTryResult::FallbackOk(mut payment) => {
                    payment.processed_on = Some(PaymentKind::Fallback);
                    payment
                }
            };
            let state = self.state.clone();
            tokio::spawn(async move {
                let result = state.send_payment_to_db(&payment).await;
                if let Err(e) = result {
                    tracing::error!("Failed to save payment on db: {:?}", e);
                }
            });
        }
    }
}

pub fn create_worker(receiver: tokio::sync::mpsc::Receiver<PaymentGet>, state: WrappedState) {
    let worker = RequestWorker::new(state);
    tokio::spawn(worker.payment_loop(receiver));
}

enum PaymentTryResult {
    CheapOk(PaymentPost),
    FallbackOk(PaymentPost),
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

pub enum SendToServiceResult {
    Ok,
    AlreadyProcessed,
    ErrRetry(anyhow::Error),
}

#[instrument(skip(payload, client))]
async fn send_to_service(
    payload: &PaymentPost,
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

    let res = client.post(url).json(&payload).send();
    let timeout = tokio::time::sleep(std::time::Duration::from_millis(1_000));
    let res = select! {
        res = res => res,
        _ = timeout => {
            tracing::warn!("the service took too long to respond");
            return SendToServiceResult::ErrRetry(anyhow::anyhow!("timeout"));
        }
    };

    let res = res.and_then(|res| res.error_for_status());
    match res {
        Ok(_res) => {
            // tracing::info!("payment service success");
            return SendToServiceResult::Ok;
        }
        Err(err) => {
            return SendToServiceResult::ErrRetry(anyhow::anyhow!(err));
        }
    }
}
