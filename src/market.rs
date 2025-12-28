//! Market discovery - finder aktivt BTC Up/Down 15min marked.

use reqwest::Client;
use serde::Deserialize;

const GAMMA_API: &str = "https://gamma-api.polymarket.com/events/slug/";
const INTERVAL: i64 = 900;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketData {
    #[serde(default)]
    clob_token_ids: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Event {
    #[serde(default)]
    title: String,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    closed: bool,
    #[serde(default)]
    end_date: String,
    #[serde(default)]
    markets: Vec<MarketData>,
}

pub struct Market {
    pub title: String,
    pub end_ts: i64,
    pub token_up: String,
    pub token_down: String,
}

pub async fn find_active(client: &Client) -> Option<Market> {
    let now = chrono::Utc::now().timestamp();
    let base = now - (now % INTERVAL);

    for offset in [0, 1, 2] {
        let slot = base + (offset * INTERVAL);
        let slug = format!("btc-updown-15m-{}", slot);
        let url = format!("{}{}", GAMMA_API, slug);

        let Ok(resp) = client.get(&url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(event) = resp.json::<Event>().await else {
            continue;
        };

        if event.active && !event.closed {
            if let Some(m) = event.markets.first() {
                let tokens: Vec<String> = serde_json::from_str(&m.clob_token_ids).ok()?;
                if tokens.len() >= 2 {
                    let end_ts = chrono::DateTime::parse_from_rfc3339(&event.end_date)
                        .map(|dt| dt.timestamp())
                        .unwrap_or(0);

                    return Some(Market {
                        title: event.title,
                        end_ts,
                        token_up: tokens[0].clone(),
                        token_down: tokens[1].clone(),
                    });
                }
            }
        }
    }
    None
}
