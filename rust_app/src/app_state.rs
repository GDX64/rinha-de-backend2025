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

struct AppState {
    db: PaymentsDb,
}

pub type PaymentSenderChannel = tokio::sync::mpsc::Sender<Bytes>;

pub type WsChannel = tokio::sync::mpsc::Sender<Bytes>;

#[derive(Clone)]
pub struct WrappedState {
    state: Option<Arc<Mutex<AppState>>>,
    pub client: Client,
    pub sender: PaymentSenderChannel,
    pub ws: WsChannel,
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct SummaryQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl WrappedState {
    pub async fn new(is_db_service: bool) -> anyhow::Result<Self> {
        let (ws_sender, mut ws_receiver) = tokio::sync::mpsc::channel(100_000);
        let (sender, receiver) = tokio::sync::mpsc::channel(100_000);

        let state = if is_db_service {
            let state = WrappedState {
                state: Some(Arc::new(Mutex::new(AppState {
                    db: PaymentsDb::new().expect("Failed to initialize database"),
                }))),
                client: Client::new(),
                sender,
                ws: ws_sender,
            };
            create_worker(receiver, state.clone());
            state
        } else {
            let db_url = std::env::var("DB_URL")?;
            let db_url = format!("ws://{db_url}/ws");
            println!("Connecting to WebSocket at: {}", db_url);
            let mut websocket = reqwest_websocket::websocket(db_url).await?;

            tokio::spawn(async move {
                while let Some(msg) = ws_receiver.recv().await {
                    let msg = WebsocketMessage::Payment(msg.to_vec());
                    let bin_message = reqwest_websocket::Message::Binary(msg.to_bytes());
                    websocket
                        .send(bin_message)
                        .await
                        .expect("Failed to send WebSocket message");
                }
            });

            WrappedState {
                state: None,
                client: Client::new(),
                sender,
                ws: ws_sender,
            }
        };
        Ok(state)
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
            .get(format!("http://{}/payments-summary", url))
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

    fn on_payment_bytes(&self, bytes: Bytes) {
        self.sender
            .try_send(bytes)
            .expect("Failed to send payment bytes");
    }

    pub async fn on_websocket(self, mut socket: axum::extract::ws::WebSocket) {
        let res = tokio::spawn(async move {
            while let Some(Ok(msg)) = socket.recv().await {
                let data = msg.into_data();
                let ws_message = WebsocketMessage::from_bytes(data);
                match ws_message {
                    WebsocketMessage::Payment(bytes) => self.on_payment_bytes(bytes.into()),
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
    Payment(Vec<u8>),
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
