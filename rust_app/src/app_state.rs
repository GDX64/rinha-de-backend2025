use std::sync::{Arc, Mutex};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    PaymentGet,
    database::{self, PaymentPost, PaymentsDb, Stats},
};

struct AppState {
    db: PaymentsDb,
    sibling_service_url: Option<String>,
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

    pub fn add_payment_cheap(&self, payment: PaymentPost) -> anyhow::Result<()> {
        return self
            .state
            .lock()
            .unwrap()
            .db
            .insert_payment(&payment, database::PaymentKind::Default);
    }

    pub fn add_payment_fallback(&self, payment: PaymentPost) -> anyhow::Result<()> {
        return self
            .state
            .lock()
            .unwrap()
            .db
            .insert_payment(&payment, database::PaymentKind::Fallback);
    }

    pub fn get_state(&self, query_data: &SummaryQuery) -> anyhow::Result<Stats> {
        let start = query_data.from.as_ref().map(|s| s.as_str());
        let end = query_data.to.as_ref().map(|s| s.as_str());
        let values = self.state.lock().unwrap().db.get_stats(start, end)?;
        Ok(values)
    }

    pub async fn merge_state_with_sibling(
        &self,
        query_data: &SummaryQuery,
        values: &mut Stats,
    ) -> anyhow::Result<()> {
        let sibling_service_url = self.state.lock().unwrap().sibling_service_url.clone();

        if let Some(url) = sibling_service_url {
            let client = self.client.clone();
            let res = client.get(format!("{}/siblings-summary", url));

            let res = if let (Some(from), Some(to)) = (&query_data.from, &query_data.to) {
                res.query(&[("from", from), ("to", to)])
            } else {
                res
            };

            let res = res.send().await?.error_for_status()?;
            let json: Value = res.json().await?;
            // tracing::info!("Received sibling summary: {:?}", json);
            let def = &json["default"];
            let def_total = def["totalAmount"].as_f64().unwrap_or(0.0);
            let def_count = def["totalRequests"].as_u64().unwrap_or(0) as usize;
            let fallback = &json["fallback"];
            let fallback_total = fallback["totalAmount"].as_f64().unwrap_or(0.0);
            let fallback_count = fallback["totalRequests"].as_u64().unwrap_or(0) as usize;

            values.fallback_total += fallback_total;
            values.fallback_count += fallback_count;
            values.default_total += def_total;
            values.default_count += def_count;
            return Ok(());
        };
        return Err(anyhow::anyhow!("Sibling service URL is not set"));
    }
}
