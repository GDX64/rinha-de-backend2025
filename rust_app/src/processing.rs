use reqwest::{Client, StatusCode};
use tokio::select;
use tracing::{Instrument, instrument};

use crate::{PaymentGet, WrappedState, database::PaymentPost};

pub fn create_worker(mut receiver: tokio::sync::mpsc::Receiver<PaymentGet>, state: WrappedState) {
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
                        // tracing::info!("Payment processed by cheap service");
                    };
                }
                PaymentTryResult::FallbackOk(payment) => {
                    let result = state.add_payment_fallback(payment);
                    if let Err(e) = result {
                        tracing::error!("Failed to insert payment: {:?}", e);
                    } else {
                        // tracing::info!("Payment processed by fallback service");
                    };
                }
            }
        }
    });
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
                // tracing::info!("Payment already processed by cheap service");
                return PaymentTryResult::CheapOk(payment_post);
            }
            SendToServiceResult::ErrRetry(e) => {
                is_retry = true;
                tracing::warn!("Retrying payment processing due to error {}", e);
            }
        };

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
                // tracing::info!("Payment already processed by fallback service");
                return PaymentTryResult::FallbackOk(payment_post);
            }
            SendToServiceResult::ErrRetry(e) => {
                tracing::warn!("Retrying payment processing due to error {}", e);
            }
        };

        //cooldown before retrying
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
    }
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

fn fallback_service_url() -> String {
    std::env::var("FALLBACK_PAYMENT").unwrap_or_else(|_| "http://localhost:8002".to_string())
}

fn default_service_url() -> String {
    std::env::var("DEFAULT_PAYMENT").unwrap_or_else(|_| "http://localhost:8001".to_string())
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
