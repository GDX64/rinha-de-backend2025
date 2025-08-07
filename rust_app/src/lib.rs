use serde::{Deserialize, Serialize};

pub mod app_state;
pub mod database;
pub mod processing;
pub mod stealing_queue;

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PaymentGet {
    #[serde(rename = "correlationId")]
    pub correlation_id: String,
    pub amount: f64,
}

impl PaymentGet {
    pub fn to_payment_post(&self) -> database::PaymentPost {
        let unix_now = chrono::Local::now();
        database::PaymentPost {
            correlation_id: self.correlation_id.clone(),
            amount: self.amount,
            requested_at: unix_now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            requested_at_ts: unix_now.timestamp_millis(),
            processed_on: None,
        }
    }
}
