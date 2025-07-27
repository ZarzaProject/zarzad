use serde::{Serialize, Deserialize};
use crate::transaction::Transaction;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Block {
    pub index: u64,
    pub timestamp: i64,
    pub transactions: Vec<Transaction>,
    pub previous_hash: String,
    pub hash: String,
    pub nonce: u64,
    pub target: [u8; 32],
}

impl Block {
    pub fn new(
        index: u64,
        timestamp: i64,
        previous_hash: String,
        transactions: Vec<Transaction>,
        difficulty: f64,
    ) -> Self {
        Block {
            index,
            timestamp,
            transactions,
            previous_hash,
            hash: String::new(),
            nonce: 0,
            target: Self::calculate_target(difficulty),
        }
    }

    pub fn calculate_target(difficulty: f64) -> [u8; 32] {
        let mut target = [0xff; 32];
        if difficulty <= 0.0 { return target; }

        let leading_zeros = (difficulty.floor() as u64 / 8) as usize;
        let remainder = difficulty.floor() as u64 % 8;

        for i in 0..leading_zeros {
            if i < 32 {
                target[i] = 0;
            }
        }

        if leading_zeros < 32 {
            target[leading_zeros] = 0xff >> remainder;
        }

        target
    }
    
    // --- MÉTODO AÑADIDO ---
    pub fn target_difficulty(&self) -> f64 {
        let mut difficulty: f64 = 0.0;
        for &byte in self.target.iter() {
            if byte == 0x00 {
                difficulty += 8.0;
            } else {
                difficulty += byte.leading_zeros() as f64;
                break;
            }
        }
        difficulty
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(&self.index.to_le_bytes());
        bytes.extend(&self.timestamp.to_le_bytes());
        bytes.extend(self.previous_hash.as_bytes());
        for tx in &self.transactions {
            bytes.extend(tx.to_bytes());
        }
        bytes
    }
}