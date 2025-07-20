use std::sync::{Arc, Mutex};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    PaymentGet,
    database::{PaymentPost, PaymentsDb, Stats},
};

struct AppState {
    db: PaymentsDb,
    db_service_url: Option<String>,
}

#[derive(Clone)]
pub struct WrappedState {
    state: Arc<Mutex<AppState>>,
    pub client: Client,
    pub sender: tokio::sync::mpsc::Sender<PaymentGet>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SummaryQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl WrappedState {
    pub fn new(
        sender: tokio::sync::mpsc::Sender<PaymentGet>,
        db_service_url: Option<String>,
    ) -> Self {
        WrappedState {
            state: Arc::new(Mutex::new(AppState {
                db: PaymentsDb::new().expect("Failed to initialize database"),
                db_service_url,
            })),
            client: Client::new(),
            sender,
        }
    }

    pub fn add_on_db(&self, payment: PaymentPost) -> anyhow::Result<()> {
        return self.state.lock().unwrap().db.insert_payment(&payment);
    }

    pub fn get_state(&self, query_data: &SummaryQuery) -> anyhow::Result<Stats> {
        let start = query_data.from.as_ref().map(|s| s.as_str());
        let end = query_data.to.as_ref().map(|s| s.as_str());
        let values = self.state.lock().unwrap().db.get_stats(start, end)?;
        Ok(values)
    }

    pub async fn send_payment_to_db(&self, payment: &PaymentPost) -> anyhow::Result<()> {
        let url = self.state.lock().unwrap().db_service_url.clone();
        let url = url.ok_or_else(|| anyhow::anyhow!("DB service URL is not set"))?;
        self.client
            .post(format!("{}/db-save", url))
            .json(payment)
            .send()
            .await?
            .error_for_status()?;
        return Ok(());
    }

    pub async fn get_from_db_service(&self, query_data: &SummaryQuery) -> anyhow::Result<Value> {
        let url = self.state.lock().unwrap().db_service_url.clone();
        let url = url.ok_or_else(|| anyhow::anyhow!("DB service URL is not set"))?;
        let response = self
            .client
            .get(format!("{}/payments-summary", url))
            .query(&[
                ("from", query_data.from.as_ref()),
                ("to", query_data.to.as_ref()),
            ])
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(response)
    }
}
