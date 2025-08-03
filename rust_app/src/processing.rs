use std::thread::JoinHandle;

use axum::body::Bytes;
use reqwest::StatusCode;
use tokio::{select, sync::mpsc::Receiver};
use tracing::instrument;

use crate::{
    PaymentGet, WrappedState,
    database::{PaymentKind, PaymentPost},
};

struct RequestWorker {
    state: WrappedState,
    default_service_url: String,
    fallback_service_url: String,
}

const TIME_BEFORE_RETRY_MILLI: u64 = 500;

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
            default_service_url: format!("{}/payments", default_service_url()),
            fallback_service_url: format!("{}/payments", fallback_service_url()),
        }
    }

    async fn try_process_payment(&mut self, payload: PaymentGet) -> PaymentTryResult {
        let payment_post = payload.to_payment_post();
        // let strikes_on_default
        loop {
            let res = send_to_service(
                &payment_post,
                &self.default_service_url,
                self.state.client.clone(),
            )
            .await;

            match res {
                SendToServiceResult::Ok => {
                    return PaymentTryResult::CheapOk(payment_post);
                }
                SendToServiceResult::ErrRetry(_e) => {
                    // tracing::error!("Failed to send payment to default service: {:?}", e);
                    tokio::time::sleep(std::time::Duration::from_millis(TIME_BEFORE_RETRY_MILLI))
                        .await;
                }
                SendToServiceResult::ErrRepeated(e) => {
                    tracing::error!("Payment already processed: {:?}", e);
                    return PaymentTryResult::CheapOk(payment_post);
                }
            };
        }
    }

    async fn payment_loop(mut self, mut recv: Receiver<Bytes>) {
        while let Some(payload) = recv.recv().await {
            let payload = serde_json::from_slice::<PaymentGet>(&payload);
            let Ok(payload) = payload else {
                tracing::error!("Failed to deserialize payment: {:?}", payload.err());
                continue;
            };
            let res = self.try_process_payment(payload).await;
            let payment = match res {
                PaymentTryResult::CheapOk(mut payment) => {
                    payment.processed_on = Some(PaymentKind::Default);
                    payment
                }
            };
            self.state
                .add_on_db(payment)
                .expect("Failed to send payment to DB");
        }
    }
}

pub fn create_worker(receiver: Receiver<Bytes>, state: WrappedState) -> JoinHandle<()> {
    let handle = std::thread::spawn(|| {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");
        rt.block_on(async move {
            let worker = RequestWorker::new(state);
            worker.payment_loop(receiver).await;
        })
    });
    return handle;
}

enum PaymentTryResult {
    CheapOk(PaymentPost),
}

pub enum SendToServiceResult {
    Ok,
    ErrRetry(anyhow::Error),
    ErrRepeated(anyhow::Error),
}

#[instrument(skip(payload, client))]
async fn send_to_service(
    payload: &PaymentPost,
    url: &str,
    client: reqwest::Client,
) -> SendToServiceResult {
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
            return SendToServiceResult::Ok;
        }
        Err(err) => {
            match err.status() {
                Some(StatusCode::UNPROCESSABLE_ENTITY) => {
                    SendToServiceResult::ErrRepeated(anyhow::anyhow!(err))
                }
                _ => {
                    return SendToServiceResult::ErrRetry(anyhow::anyhow!(err));
                }
            }
        }
    }
}
