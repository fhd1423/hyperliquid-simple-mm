use ethers::{signers::LocalWallet, types::H160};
use log::info;

use hyperliquid_rust_sdk::{
     AssetMeta, BaseUrl, ClientCancelRequest, ClientLimit, ClientOrder, ClientOrderRequest, ExchangeClient, ExchangeDataStatus, ExchangeResponseStatus, InfoClient, Message, Meta, Subscription
};
use tokio::{spawn, sync::mpsc::unbounded_channel};
use std::{collections::HashMap, f64, str::FromStr, sync::{Arc, Mutex}, thread::sleep, time::Duration};

use std::collections::VecDeque;

struct PriceQueue {
    queue: VecDeque<f64>,
    capacity: usize,
    current_sum: f64,
    last_trade: String,
}

impl PriceQueue {
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
            current_sum: 0.0,
            last_trade: "hold".to_string(),
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

        // Capture the current (past) state of last_trade before potentially updating it
        let past_trade = self.last_trade.clone();


        if new_price > avg * 1.001 && past_trade != "sell" {
            self.last_trade = "sell".to_string();
            println!("here in sell, past trade is {0}", past_trade);
            (avg, "sell".to_string())

        } else if new_price < avg * 0.999 && past_trade != "buy"{
            self.last_trade = "buy".to_string();
            println!("here in buy, past trade is {0}", past_trade);
            (avg, "buy".to_string())

        } else {
            self.last_trade = "hold".to_string();
            (avg, "hold".to_string())
        }
    }

    pub fn current_average(&self) -> f64 {
        self.current_sum / self.queue.len() as f64
    }
}
#[cfg(test)]
mod tests {
    use super::*;  // Import the necessary items from the outer module

    #[test]
    fn test_initialization() {
        let queue = PriceQueue::new(5);
        assert_eq!(queue.capacity, 5);
        assert!(queue.queue.is_empty());
        assert_eq!(queue.current_sum, 0.0);
        assert_eq!(queue.last_trade, "hold");
    }

    #[test]
    fn test_add_price_and_average() {
        let mut queue = PriceQueue::new(3);
        queue.update_and_decide(100.0);
        assert_eq!(queue.current_average(), 100.0);

        queue.update_and_decide(200.0);
        assert_eq!(queue.current_average(), 150.0);

        queue.update_and_decide(300.0);
        assert_eq!(queue.current_average(), 200.0);
    }

    #[test]
    fn test_capacity_and_rollover() {
        let mut queue = PriceQueue::new(2);
        queue.update_and_decide(100.0);
        queue.update_and_decide(200.0);

        // Capacity reached, next addition should remove the oldest price (100.0)
        queue.update_and_decide(300.0);
        assert_eq!(queue.queue.len(), 2);
        assert_eq!(queue.current_average(), 250.0);
    }

    #[test]
    fn test_decision_logic() {
        let mut queue = PriceQueue::new(3);
        queue.update_and_decide(100.0);
        queue.update_and_decide(100.0);

        // Test "hold" decision
        let (_, decision) = queue.update_and_decide(100.0);
        assert_eq!(queue.last_trade, "hold");
        assert_eq!(decision, "hold");

        // Test "sell" decision
        let (_, decision) = queue.update_and_decide(102.0);  // price > average * 1.001
        assert_eq!(queue.last_trade, "sell");
        assert_eq!(decision, "sell");

        // Test "buy" decision
        let (_, decision) = queue.update_and_decide(98.0);  // price < average * 0.999
        assert_eq!(queue.last_trade, "buy");
        assert_eq!(decision, "buy");
    }
    #[test]
    fn test_update_and_decide_logic() {
        let mut queue = PriceQueue::new(3);
        let (_, decision) = queue.update_and_decide(1.1); // Assuming avg around 1.0
        assert_eq!(decision, "hold");
        assert_eq!(queue.last_trade, "hold");

        let (_, decision) = queue.update_and_decide(0.9);
        assert_eq!(decision, "buy");
        assert_eq!(queue.last_trade, "buy");
}

}
