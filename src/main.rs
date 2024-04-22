use ethers::{signers::LocalWallet};
use hyperliquid_rust_sdk::{
     AssetMeta, BaseUrl, ClientCancelRequest, ClientLimit, ClientOrder, ClientOrderRequest, ExchangeClient, ExchangeDataStatus, ExchangeResponseStatus, InfoClient, Message, Meta, Subscription
};
use tokio::{sync::mpsc::unbounded_channel};
use std::{f64, sync::{Arc, Mutex}, thread::sleep, time::Duration};

use std::collections::VecDeque;

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


        if new_price > avg * 1.001 {
            (avg, "sell".to_string())
        } else if new_price < avg * 0.999 {
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


async fn place_trade(exchange_client: &mut ExchangeClient, is_buy: bool, limit_px: f64) -> Option<u64> {
    // Create the order request
    let order = ClientOrderRequest {
        asset: "PURR/USDC".to_string(),
        is_buy,
        reduce_only: false,
        limit_px,
        sz: 9000.0,
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
        },
    };

    // Extract the status from the response data
    let status = exchange_response.data.and_then(|data| data.statuses.get(0).cloned());

    // Based on the status, extract the oid if available
    match status {
        Some(ExchangeDataStatus::Filled(order)) => Some(order.oid),
        Some(ExchangeDataStatus::Resting(order)) => Some(order.oid),
        Some(status) => {
            eprintln!("Unhandled status: {status:?}");
            return None;
        },
        None => {
            eprintln!("Status list is empty");
            return None;
        },
    }
}

async fn cancel_trade(exchange_client: &mut ExchangeClient, oid: u64) -> bool {
    let cancel_request = ClientCancelRequest {
        asset: "PURR/USDC".to_string(),
        oid,
    };

    let response = exchange_client.cancel(cancel_request, None).await.unwrap();
    let _exchange_response = match response {
        ExchangeResponseStatus::Ok(_data) => return true,
        ExchangeResponseStatus::Err(e) => {
            eprintln!("error with exchange response: {e}");
            return false; 
        },
    };
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

                    match decision.as_str() {
                        "buy" | "sell" => {
                            if last_trade_action == decision {
                                continue;
                            }
                            
                            println!("Decision: {decision}, Last Trade: {last_trade_action} Price: {new_price}");
                            let oid = place_trade(&mut *client, decision == "buy", new_price).await;
                            if let Some(oid) = oid {
                                println!("{} order placed with oid: {}", decision, oid);
                                sleep(Duration::from_secs(10));

                                let cancel_response = cancel_trade(&mut *client, oid).await;

                                if !cancel_response {
                                    println!("{} order filled completely for oid: {}", decision, oid);
                                    last_trade_action = decision.to_string();
                                }
                                else{
                                    let new_buy_price;
                                    if decision == "buy" {
                                        new_buy_price = (new_price * 1.05 * 100000.0).round() / 100000.0;
                                    } else {
                                        new_buy_price = (new_price * 0.95 * 100000.0).round() / 100000.0;
                                    }
                                    // theoretically should always fill
                                    place_trade(&mut *client, decision == "buy", new_buy_price).await;
                                    last_trade_action = decision.to_string();
                                }
                            }
                            else{
                                // when the trade couldnt be placed, meaning it had already been bought/sold
                                last_trade_action = decision.to_string();
                            }
                        },
                        _ => {
                        }
                    }
                },
                Err(_) => {
                    eprintln!("Failed to parse PURR price");
                    continue;
                }
            };
        }
    }
}