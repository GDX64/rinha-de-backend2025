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
}

#[derive(Clone)]
pub struct WrappedState {
    state: Option<Arc<Mutex<AppState>>>,
    pub client: Client,
    pub sender: tokio::sync::mpsc::Sender<PaymentGet>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SummaryQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl WrappedState {
    pub fn new(sender: tokio::sync::mpsc::Sender<PaymentGet>, is_db_service: bool) -> Self {
        if is_db_service {
            WrappedState {
                state: Some(Arc::new(Mutex::new(AppState {
                    db: PaymentsDb::new().expect("Failed to initialize database"),
                }))),
                client: Client::new(),
                sender,
            }
        } else {
            WrappedState {
                state: None,
                client: Client::new(),
                sender,
            }
        }
    }

    fn with_state<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&AppState) -> R,
    {
        let state = self.state.as_ref().expect("State is not initialized");
        let state = state.lock().unwrap();
        f(&state)
    }

    pub fn add_on_db(&self, payment: PaymentPost) -> anyhow::Result<()> {
        return self.with_state(|state| {
            return state.db.insert_payment(&payment);
        });
    }

    pub fn get_state(&self, query_data: &SummaryQuery) -> anyhow::Result<Stats> {
        let start = query_data.from.as_ref().map(|s| s.as_str());
        let end = query_data.to.as_ref().map(|s| s.as_str());
        let values = self.with_state(|state| state.db.get_stats(start, end))?;
        Ok(values)
    }

    pub async fn send_payment_to_db(&self, payment: &PaymentPost, url: &str) -> anyhow::Result<()> {
        self.client
            .post(url)
            .json(payment)
            .send()
            .await?
            .error_for_status()?;
        return Ok(());
    }

    pub async fn get_from_db_service(
        &self,
        query_data: &SummaryQuery,
        url: &str,
    ) -> anyhow::Result<Value> {
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
