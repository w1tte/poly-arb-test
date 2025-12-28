//! Orderbook data layer - ren sensor, ingen logik.
//!
//! Ansvar: Modtag live orderbogsdata fra Polymarket WebSocket,
//! vedligehold rolling state, og signal ved ændringer.

use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio_tungstenite::{connect_async, tungstenite::Message};

const WS_URL: &str = "wss://ws-subscriptions-clob.polymarket.com/ws/market";

/// Top-of-book state for et marked
#[derive(Debug, Clone, Default)]
pub struct OrderbookState {
    pub up_bid_price: String,
    pub up_bid_size: String,
    pub up_ask_price: String,
    pub up_ask_size: String,
    pub down_bid_price: String,
    pub down_bid_size: String,
    pub down_ask_price: String,
    pub down_ask_size: String,
    pub last_update_ms: i64,
}

/// Signal der udsendes ved state-ændring
#[derive(Debug, Clone)]
pub struct StateUpdated;

/// Input til orderbook data layer
pub struct OrderbookConfig {
    pub token_up: String,
    pub token_down: String,
}

/// Handle til at interagere med orderbook data layer
pub struct OrderbookHandle {
    state: Arc<RwLock<OrderbookState>>,
    update_tx: broadcast::Sender<StateUpdated>,
    shutdown_tx: tokio::sync::oneshot::Sender<()>,
}

impl OrderbookHandle {
    /// Læs nuværende orderbook state
    pub async fn get_current_state(&self) -> OrderbookState {
        self.state.read().await.clone()
    }

    /// Subscribe til state updates
    pub fn subscribe_updates(&self) -> broadcast::Receiver<StateUpdated> {
        self.update_tx.subscribe()
    }

    /// Stop orderbook data layer
    pub fn shutdown(self) {
        let _ = self.shutdown_tx.send(());
    }
}

/// Start orderbook data layer - returnerer handle til interaktion
pub fn spawn(config: OrderbookConfig) -> OrderbookHandle {
    let state = Arc::new(RwLock::new(OrderbookState::default()));
    let (update_tx, _) = broadcast::channel(64);
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

    let state_clone = state.clone();
    let update_tx_clone = update_tx.clone();

    tokio::spawn(async move {
        run_websocket_loop(config, state_clone, update_tx_clone, shutdown_rx).await;
    });

    OrderbookHandle {
        state,
        update_tx,
        shutdown_tx,
    }
}

async fn run_websocket_loop(
    config: OrderbookConfig,
    state: Arc<RwLock<OrderbookState>>,
    update_tx: broadcast::Sender<StateUpdated>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<()>,
) {
    // Forbind til WebSocket
    let (ws, _) = match connect_async(WS_URL).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("[orderbook] WS connect error: {}", e);
            return;
        }
    };


    let (mut write, mut read) = ws.split();

    // Subscribe til begge tokens
    let sub_up = serde_json::json!({
        "type": "subscribe",
        "channel": "book",
        "assets_ids": [&config.token_up]
    });
    let sub_down = serde_json::json!({
        "type": "subscribe",
        "channel": "book",
        "assets_ids": [&config.token_down]
    });

    if write.send(Message::Text(sub_up.to_string())).await.is_err() {
        eprintln!("[orderbook] Fejl ved subscribe UP");
        return;
    }
    if write.send(Message::Text(sub_down.to_string())).await.is_err() {
        eprintln!("[orderbook] Fejl ved subscribe DOWN");
        return;
    }


    // Event loop
    loop {
        tokio::select! {
            // Shutdown signal
            _ = &mut shutdown_rx => {
                break;
            }

            // WebSocket message
            msg = read.next() => {
                let Some(msg) = msg else {
                    break;
                };

                let Ok(Message::Text(txt)) = msg else { continue };

                if let Some(updated) = process_message(&txt, &config, &state).await {
                    if updated {
                        // Signal state ændring
                        let _ = update_tx.send(StateUpdated);
                    }
                }
            }
        }
    }

}

/// Processér en WebSocket besked og opdater state
/// Returnerer Some(true) hvis state blev opdateret, Some(false) hvis ikke, None ved parse fejl
async fn process_message(
    txt: &str,
    config: &OrderbookConfig,
    state: &Arc<RwLock<OrderbookState>>,
) -> Option<bool> {
    let data: serde_json::Value = serde_json::from_str(txt).ok()?;

    // Find asset ID
    let asset_id = data
        .get("asset_id")
        .or_else(|| data.get("assetId"))
        .or_else(|| data.get("token_id"))
        .and_then(|v| v.as_str())?;

    let is_up = asset_id == config.token_up;
    let is_down = asset_id == config.token_down;
    if !is_up && !is_down {
        return Some(false);
    }

    // Parse bids og asks
    let bids: Vec<serde_json::Value> = data
        .get("bids")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let asks: Vec<serde_json::Value> = data
        .get("asks")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Best bid/ask - kun hvis der er data
    let best_bid = bids.last().and_then(|v| {
        let price = v.get("price")?.as_str()?;
        let size = v.get("size")?.as_str()?;
        Some((price.to_string(), size.to_string()))
    });
    let best_ask = asks.last().and_then(|v| {
        let price = v.get("price")?.as_str()?;
        let size = v.get("size")?.as_str()?;
        Some((price.to_string(), size.to_string()))
    });

    // Hvis ingen data, behold tidligere state
    if best_bid.is_none() && best_ask.is_none() {
        return Some(false);
    }

    let now_ms = chrono::Utc::now().timestamp_millis();

    // Opdater state - kun felter med ny data, behold resten
    {
        let mut s = state.write().await;
        
        if is_up {
            // Opdater UP direkte
            if let Some((price, size)) = &best_bid {
                s.up_bid_price = price.clone();
                s.up_bid_size = size.clone();
                // DOWN ask = 1 - UP bid
                if let Ok(p) = price.parse::<f64>() {
                    s.down_ask_price = format!("{:.2}", 1.0 - p);
                    s.down_ask_size = size.clone();
                }
            }
            if let Some((price, size)) = &best_ask {
                s.up_ask_price = price.clone();
                s.up_ask_size = size.clone();
                // DOWN bid = 1 - UP ask
                if let Ok(p) = price.parse::<f64>() {
                    s.down_bid_price = format!("{:.2}", 1.0 - p);
                    s.down_bid_size = size.clone();
                }
            }
        } else {
            // Opdater DOWN direkte
            if let Some((price, size)) = &best_bid {
                s.down_bid_price = price.clone();
                s.down_bid_size = size.clone();
                // UP ask = 1 - DOWN bid
                if let Ok(p) = price.parse::<f64>() {
                    s.up_ask_price = format!("{:.2}", 1.0 - p);
                    s.up_ask_size = size.clone();
                }
            }
            if let Some((price, size)) = &best_ask {
                s.down_ask_price = price.clone();
                s.down_ask_size = size.clone();
                // UP bid = 1 - DOWN ask
                if let Ok(p) = price.parse::<f64>() {
                    s.up_bid_price = format!("{:.2}", 1.0 - p);
                    s.up_bid_size = size.clone();
                }
            }
        }
        
        s.last_update_ms = now_ms;
    }

    Some(true)
}

