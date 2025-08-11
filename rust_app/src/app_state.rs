use axum::body::Bytes;
use futures_util::SinkExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::{Arc, Mutex};

use crate::{
    database::{PaymentPost, PaymentsDb, Stats},
    processing::create_worker,
};

pub struct AppState {
    db: Option<PaymentsDb>,
    pub client: Client,
    pub sender: PaymentSenderChannel,
    pub ws: WsChannel,
}

impl AppState {
    pub async fn new_async(is_db_service: bool) -> anyhow::Result<(Self, PaymentReceiverChannel)> {
        let (ws_sender, mut ws_receiver) = tokio::sync::mpsc::channel(100_000);
        let (sender, receiver) = tokio::sync::mpsc::channel(100_000);

        let state = if is_db_service {
            let state = AppState {
                db: Some(PaymentsDb::new().expect("Failed to initialize database")),
                client: Client::new(),
                sender,
                ws: ws_sender,
            };
            state
        } else {
            let db_url = std::env::var("DB_URL")?;
            let db_url = format!("ws://{db_url}/ws");
            println!("Connecting to WebSocket at: {}", db_url);
            let mut websocket = reqwest_websocket::websocket(db_url).await?;

            tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
                loop {
                    interval.tick().await;
                    let mut v = Vec::new();
                    while let Ok(msg) = ws_receiver.try_recv() {
                        v.push(msg.to_vec());
                    }
                    let msg = WebsocketMessage::Payment(v);
                    let bin_message = reqwest_websocket::Message::Binary(msg.to_bytes());
                    websocket
                        .send(bin_message)
                        .await
                        .expect("Failed to send message");
                }
            });

            AppState {
                db: None,
                client: Client::new(),
                sender,
                ws: ws_sender,
            }
        };
        return Ok((state, receiver));
    }

    fn default() -> Self {
        AppState {
            db: None,
            client: Client::new(),
            sender: tokio::sync::mpsc::channel(100_000).0,
            ws: tokio::sync::mpsc::channel(100_000).0,
        }
    }
}

pub type PaymentSenderChannel = tokio::sync::mpsc::Sender<Bytes>;
type PaymentReceiverChannel = tokio::sync::mpsc::Receiver<Bytes>;
pub type WsChannel = tokio::sync::mpsc::Sender<Bytes>;

#[derive(Clone)]
pub struct WrappedState {
    state: Arc<Mutex<AppState>>,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SummaryQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl Default for SummaryQuery {
    fn default() -> Self {
        SummaryQuery {
            from: None,
            to: None,
        }
    }
}

impl From<&str> for SummaryQuery {
    fn from(query: &str) -> Self {
        return serde_json::from_str(query).unwrap_or_default();
    }
}

impl WrappedState {
    pub fn default() -> Self {
        WrappedState {
            state: Arc::new(Mutex::new(AppState::default())),
        }
    }

    pub async fn init(&self, is_db_service: bool) {
        let (state, receiver) = AppState::new_async(is_db_service)
            .await
            .expect("Failed to initialize state");
        *self.state.lock().unwrap() = state;
        if is_db_service {
            create_worker(receiver, self.clone());
        }
    }

    pub fn with_state<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&AppState) -> R,
    {
        let state = self.state.lock().unwrap();
        f(&state)
    }

    pub fn add_on_db(&self, payment: PaymentPost) -> anyhow::Result<()> {
        return self.with_state(|state| {
            return state.db.as_ref().unwrap().insert_payment(&payment);
        });
    }

    pub fn get_state(&self, query_data: &SummaryQuery) -> anyhow::Result<Stats> {
        let start = query_data.from.as_ref().map(|s| s.as_str());
        let end = query_data.to.as_ref().map(|s| s.as_str());
        let values = self.with_state(|state| state.db.as_ref().unwrap().get_stats(start, end))?;
        Ok(values)
    }

    pub fn get_client(&self) -> Client {
        self.with_state(|state| state.client.clone())
    }

    pub fn on_payment_received(&self, bytes: Bytes) {
        self.with_state(|state| {
            state
                .ws
                .try_send(bytes)
                .expect("Failed to send payment bytes");
        });
    }

    pub async fn get_from_db_service(
        &self,
        query_data: Option<&str>,
        url: &str,
    ) -> anyhow::Result<Value> {
        let url = if let Some(query_data) = query_data {
            format!("http://{}/payments-summary?{}", url, query_data)
        } else {
            format!("http://{}/payments-summary", url)
        };
        let request = self.get_client().get(url);
        let response = request
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await?;
        Ok(response)
    }

    fn on_payment_bytes(&self, bytes: Bytes) {
        self.with_state(|state| {
            return state
                .sender
                .try_send(bytes.clone())
                .expect("Failed to send payment bytes");
        });
    }

    pub async fn on_websocket(self, mut socket: axum::extract::ws::WebSocket) {
        let res = tokio::spawn(async move {
            while let Some(Ok(msg)) = socket.recv().await {
                let data = msg.into_data();
                let ws_message = WebsocketMessage::from_bytes(data);
                match ws_message {
                    WebsocketMessage::Payment(v) => {
                        v.into_iter()
                            .for_each(|bytes| self.on_payment_bytes(bytes.into()));
                    }
                    WebsocketMessage::None => {}
                }
            }
        })
        .await;
        if let Err(e) = res {
            tracing::error!("WebSocket handler failed: {:?}", e);
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum WebsocketMessage {
    Payment(Vec<Vec<u8>>),
    None,
}

impl WebsocketMessage {
    pub fn from_bytes(bytes: Bytes) -> Self {
        let Ok((me, _)) = bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
        else {
            tracing::error!("Failed to decode bytes");
            return WebsocketMessage::None;
        };
        return me;
    }

    pub fn to_bytes(&self) -> Bytes {
        let Ok(bytes) = bincode::serde::encode_to_vec(self, bincode::config::standard()) else {
            tracing::error!("Failed to encode WebSocket message");
            return Bytes::new();
        };
        return Bytes::from(bytes);
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct PaymentGet {
    #[serde(rename = "correlationId")]
    pub correlation_id: String,
    pub amount: f64,
}

impl PaymentGet {
    pub fn to_payment_post(&self) -> crate::database::PaymentPost {
        let unix_now = chrono::Local::now();
        crate::database::PaymentPost {
            correlation_id: self.correlation_id.clone(),
            amount: self.amount,
            requested_at: unix_now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            requested_at_ts: unix_now.timestamp_millis(),
            processed_on: None,
        }
    }
}
