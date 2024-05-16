use ethers::signers::LocalWallet;
use tokio::sync::mpsc::unbounded_channel;
use std::collections::VecDeque;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use chrono::Utc;

use hyperliquid_rust_sdk::{
    AssetMeta, BaseUrl, ClientCancelRequest, ClientLimit, ClientOrder, ClientOrderRequest, ExchangeClient, ExchangeDataStatus, ExchangeResponseStatus, InfoClient, Message, Meta, Subscription
};
use std::{
    f64,
    sync::{Arc, Mutex},
    thread::sleep,
    time::Duration
};

#[derive(Serialize)]
struct RequestPayload {
    #[serde(rename = "type")]
    type_field: String,
    user: String,
}

#[derive(Deserialize)]
struct ResponseData {
    balances: Vec<Balance>,
}

#[derive(Deserialize)]
struct Balance {
    coin: String,
    total: String, // Using String to handle potential floating-point inconsistencies.
}

struct PriceQueue {
    queue: VecDeque<f64>,
    capacity: usize,
    current_sum: f64,
}

impl PriceQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
            current_sum: 0.0,
        }
    }

    pub fn update_and_decide(&mut self, new_price: f64) -> (f64, String) {
        if self.queue.len() == self.capacity {
            let old_price = self.queue.pop_front().unwrap();  // Assuming it's safe to unwrap here
            self.current_sum -= old_price;
        }

        self.queue.push_back(new_price);
        self.current_sum += new_price;
        let avg = self.current_average();

        if new_price > avg * 1.015 {
            (avg, "sell".to_string())
        } else if new_price < avg * 0.99 {
            (avg, "buy".to_string())
        } else {
            (avg, "hold".to_string())
        }
    }

    pub fn current_average(&self) -> f64 {
        self.current_sum / self.queue.len() as f64
    }
}

lazy_static::lazy_static! {
    static ref WALLET: LocalWallet = "PRIVATE_KEY"
        .parse()
        .unwrap();

    static ref UNIVERSE: Vec<AssetMeta> = {
        let mut universe = Vec::<AssetMeta>::new();
        let asset_meta_purr: AssetMeta = AssetMeta {
            name: "PURR/USDC".to_string(),
            sz_decimals: 5,
        };

        for _ in 0..10000 {
            universe.push(AssetMeta {
                name: "".to_string(),
                sz_decimals: 0,
            });
        }
        universe.push(asset_meta_purr);
        universe
    };

    static ref META: Meta = Meta {
        universe: UNIVERSE.clone(),
    };
}

async fn get_position(ticker: &str) -> Option<f64> {
    let client = Client::new();
    let ticker = ticker.to_string();

    let response = client.post("https://api.hyperliquid.xyz/info")
        .json(&RequestPayload {
            type_field: "spotClearinghouseState".to_string(),
            user: "0x711ACA028ECAEA178EbC29c7059CFdb195FaCD37".to_string(),
        })
        .header("Content-Type", "application/json")
        .send()
        .await.ok();

    if let Some(response) = response {
        let data: ResponseData = response.json().await.ok()?;

        for balance in data.balances {
            if balance.coin == ticker {
                return match balance.total.parse::<f64>() {
                    Ok(num) => Some(num),
                    Err(_) => None,
                };
            }
        }
    }

    None
}

async fn place_trade(exchange_client: &mut ExchangeClient, is_buy: bool, limit_px: f64) -> Option<u64> {
    let current_purr_position = get_position("PURR").await.unwrap_or(0.0);
    let current_usdc_position = get_position("USDC").await.unwrap_or(0.0);

    // already have that position
    if is_buy && current_usdc_position < 100.0 || !is_buy && current_purr_position < 100.0 {
        return None;
    }

    let sz_to_trade = if is_buy { current_usdc_position / limit_px } else { current_purr_position };
    // Create the order request
    let order = ClientOrderRequest {
        asset: "PURR/USDC".to_string(),
        is_buy,
        reduce_only: false,
        limit_px,
        sz: sz_to_trade.floor(),
        order_type: ClientOrder::Limit(ClientLimit {
            tif: "Gtc".to_string(),
        }),
    };

    // Send the order and handle potential errors
    let response = exchange_client.order(order, None).await.ok()?;

    // Match on the response status and proceed accordingly
    let exchange_response = match response {
        ExchangeResponseStatus::Ok(data) => data,
        ExchangeResponseStatus::Err(e) => {
            eprintln!("error with exchange response: {e}");
            return None;
        }
    };

    // Extract the status from the response data
    let status = exchange_response.data.and_then(|data| data.statuses.first().cloned());

    // Based on the status, extract the oid if available
    match status {
        Some(ExchangeDataStatus::Filled(order)) => Some(order.oid),
        Some(ExchangeDataStatus::Resting(order)) => Some(order.oid),
        Some(status) => {
            eprintln!("Unhandled status: {status:?}");
            None
        }
        None => {
            eprintln!("Status list is empty");
            None
        }
    }
}

async fn cancel_trade(exchange_client: &mut ExchangeClient, oid: u64) -> bool {
    let cancel_request = ClientCancelRequest {
        asset: "PURR/USDC".to_string(),
        oid,
    };

    let response = exchange_client.cancel(cancel_request, None).await.unwrap();
    match response {
        ExchangeResponseStatus::Ok(_data) => true,
        ExchangeResponseStatus::Err(e) => {
            eprintln!("error with exchange response: {e}");
            false
        }
    }
}

#[tokio::main]
async fn main() {
    let mut price_queue = PriceQueue::new(500);

    let mut info_client = InfoClient::new(None, Some(BaseUrl::Mainnet)).await.unwrap();

    let (sender, mut receiver) = unbounded_channel();
    let _subscription_id = info_client
        .subscribe(Subscription::AllMids, sender)
        .await
        .unwrap();

    let exchange_client: ExchangeClient = ExchangeClient::new(None, WALLET.clone(), Some(BaseUrl::Mainnet), Some(META.clone()), None)
        .await
        .unwrap();

    let shared_client = Arc::new(Mutex::new(exchange_client));

    let mut client = shared_client.lock().unwrap();
    let mut last_trade_action = "none".to_string();

    while let Some(Message::AllMids(all_mids)) = receiver.recv().await {
        if let Some(purr_price) = all_mids.data.mids.get("PURR/USDC") {
            match purr_price.parse::<f64>() {
                Ok(new_price) => {
                    let (_average, decision) = price_queue.update_and_decide(new_price);
                    let new_price = (new_price * 100000.0).round() / 100000.0;
                    let current_time = Utc::now();
                    println!("Current Price: {}, Average: {}, Time: {}", new_price, _average, current_time);

                    match decision.as_str() {
                        "buy" | "sell" => {
                            // skip if trying to take the same trade twice in a row
                            if last_trade_action == decision {
                                continue;
                            }

                            println!("Decision: {decision}, Price: {new_price}");
                            let oid = place_trade(&mut client, decision == "buy", new_price).await;
                            if let Some(oid) = oid {
                                println!("{} order placed with oid: {}", decision, oid);
                                sleep(Duration::from_secs(30));

                                let cancel_response = cancel_trade(&mut client, oid).await;

                                if !cancel_response {
                                    println!("{} order filled completely for oid: {}", decision, oid);
                                    last_trade_action = decision.to_string();
                                } else {
                                    let new_trade_price = if decision == "buy" { (new_price * 1.01 * 100000.0).round() / 100000.0 } else { (new_price * 0.99 * 100000.0).round() / 100000.0 };
                                    // theoretically should always fill
                                    place_trade(&mut client, decision == "buy", new_trade_price).await;
                                    last_trade_action = decision.to_string();
                                }
                            } else {
                                // when the trade couldn't be placed, meaning it had already been bought/sold
                                last_trade_action = decision.to_string();
                            }
                        }
                        _ => {}
                    }
                }
                Err(_) => {
                    eprintln!("Failed to parse PURR price");
                    continue;
                }
            };
        }
    }
}
