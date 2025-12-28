mod market;
mod orderbook;

use std::io::Write;

#[tokio::main]
async fn main() {
    let client = reqwest::Client::builder().tcp_nodelay(true).build().unwrap();

    // Market discovery
    let Some(m) = market::find_active(&client).await else {
        println!("Intet aktivt marked fundet");
        return;
    };

    println!("{}", m.title);
    let end_ts = m.end_ts;

    // Start orderbook data layer
    let handle = orderbook::spawn(orderbook::OrderbookConfig {
        token_up: m.token_up,
        token_down: m.token_down,
    });

    // Subscribe til updates
    let mut updates = handle.subscribe_updates();

    loop {
        match updates.recv().await {
            Ok(_) => {
                let state = handle.get_current_state().await;
                let now = chrono::Utc::now().timestamp();
                let ttl = end_ts - now;

                print!("\rTTL:{:>4}s | UP {}/{} - {}/{} | DOWN {}/{} - {}/{}    ",
                    ttl,
                    state.up_bid_price, state.up_bid_size,
                    state.up_ask_price, state.up_ask_size,
                    state.down_bid_price, state.down_bid_size,
                    state.down_ask_price, state.down_ask_size,
                );
                let _ = std::io::stdout().flush();

                if ttl <= 0 {
                    println!("\nMarked udløbet!");
                    break;
                }
            }
            Err(_) => break,
        }
    }
}
