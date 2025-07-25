use std::collections::HashMap;
use chrono::Utc;
use zrzhash::ZRZHasher;
use crate::{block::Block, transaction::{Transaction, TxOutput}};
use serde::{Serialize, Deserialize};
use thiserror::Error;
use log;
use std::error::Error;

#[derive(Error, Debug)]
pub enum BlockchainError {
    #[error("El bloque propuesto es inválido")]
    InvalidBlock,
    #[error("La blockchain está vacía")]
    EmptyChain,
    #[error("Error de transacción: {0}")]
    TransactionError(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blockchain {
    pub chain: Vec<Block>,
    pub current_transactions: Vec<Transaction>,
    pub utxos: HashMap<String, TxOutput>,
    pub difficulty: f64,
    pub reward: f64,
    pub min_reward: f64,
    pub reduction_rate: f64,
    pub block_time: u64,
    pub difficulty_adjust_blocks: u64,
}

impl Blockchain {
    pub fn new(settings: &config::Config) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut blockchain = Blockchain {
            chain: Vec::new(),
            current_transactions: Vec::new(),
            utxos: HashMap::new(),
            difficulty: settings.get::<f64>("consensus.initial_difficulty")?,
            reward: settings.get::<f64>("consensus.initial_block_reward")?,
            min_reward: settings.get::<f64>("consensus.min_block_reward")?,
            reduction_rate: settings.get::<f64>("consensus.reward_reduction_rate")?,
            block_time: settings.get::<u64>("consensus.block_time")?,
            difficulty_adjust_blocks: settings.get::<u64>("consensus.difficulty_adjust_blocks")?,
        };
        blockchain.create_genesis_block();
        Ok(blockchain)
    }

    fn create_genesis_block(&mut self) {
        if self.chain.is_empty() {
            let genesis_block = Block {
                index: 0,
                timestamp: Utc::now().timestamp(),
                transactions: vec![],
                previous_hash: String::from("0"),
                hash: String::from("0000000000000000000000000000000000000000000000000000000000000000"),
                nonce: 0,
                target: Block::calculate_target(1.0),
            };
            self.chain.push(genesis_block);
        }
    }
    
    pub fn add_block(&mut self, block: Block) -> Result<(), BlockchainError> {
        if !self.is_valid_block(&block) {
            return Err(BlockchainError::InvalidBlock);
        }
        self.process_transactions(&block.transactions)?;
        self.chain.push(block);
        self.adjust_reward();
        self.adjust_difficulty();
        self.current_transactions.clear();
        Ok(())
    }

    fn process_transactions(&mut self, transactions: &[Transaction]) -> Result<(), BlockchainError> {
        let mut total_fees = 0.0;
        for tx in transactions {
            if !tx.verify_signature() {
                return Err(BlockchainError::TransactionError(format!("Firma inválida en la transacción {}", tx.id)));
            }
            if tx.is_coinbase() {
                self.update_utxos_with_new_outputs(tx);
                continue;
            }
            let input_sum = self.verify_inputs(tx)?;
            let output_sum: f64 = tx.outputs.iter().map(|o| o.amount).sum();
            
            if input_sum < output_sum {
                return Err(BlockchainError::TransactionError(format!("Fondos insuficientes en la transacción {}", tx.id)));
            }

            total_fees += input_sum - output_sum;
            
            self.consume_inputs_as_utxos(tx);
            self.update_utxos_with_new_outputs(tx);
        }
        self.reward += total_fees;
        Ok(())
    }

    fn verify_inputs(&self, tx: &Transaction) -> Result<f64, BlockchainError> {
        let mut total_input_amount = 0.0;
        for input in &tx.inputs {
            let utxo_key = format!("{}:{}", input.tx_id, input.output_index);
            let utxo = self.utxos.get(&utxo_key)
                .ok_or_else(|| BlockchainError::TransactionError(format!("Intento de doble gasto o UTXO inválida: {}", utxo_key)))?;
            total_input_amount += utxo.amount;
        }
        Ok(total_input_amount)
    }
    
    fn consume_inputs_as_utxos(&mut self, tx: &Transaction) {
        for input in &tx.inputs {
            let utxo_key = format!("{}:{}", input.tx_id, input.output_index);
            self.utxos.remove(&utxo_key);
        }
    }

    fn update_utxos_with_new_outputs(&mut self, tx: &Transaction) {
        for (index, output) in tx.outputs.iter().enumerate() {
            let utxo_key = format!("{}:{}", tx.id, index);
            self.utxos.insert(utxo_key, output.clone());
        }
    }

    pub fn get_balance(&self, address: &str) -> f64 {
        self.utxos.values().filter(|utxo| utxo.address == address).map(|utxo| utxo.amount).sum()
    }
    
    fn adjust_difficulty(&mut self) {
        let last_block = match self.chain.last() {
            Some(block) => block,
            None => return,
        };

        if last_block.index > 0 && last_block.index % self.difficulty_adjust_blocks == 0 {
            let period_start_index = (last_block.index - self.difficulty_adjust_blocks) as usize;
            let period_start_block = match self.chain.get(period_start_index) {
                Some(block) => block,
                None => return,
            };
            
            let mut timestamps: Vec<i64> = self.chain[period_start_index..=last_block.index as usize]
                .iter()
                .map(|b| b.timestamp)
                .collect();
            timestamps.sort_unstable();

            let median_time_past = if timestamps.len() % 2 == 0 {
                let mid = timestamps.len() / 2;
                (timestamps[mid - 1] + timestamps[mid]) / 2
            } else {
                timestamps[timestamps.len() / 2]
            };

            let time_taken_for_period = median_time_past - period_start_block.timestamp;
            let expected_time_for_period = (self.difficulty_adjust_blocks * self.block_time) as i64;
            
            if time_taken_for_period <= 0 { return; }

            let alpha = 2.0 / (self.difficulty_adjust_blocks as f64 + 1.0);
            let target_adjustment = self.difficulty * (expected_time_for_period as f64 / time_taken_for_period as f64);
            let new_difficulty = self.difficulty * (1.0 - alpha) + target_adjustment * alpha;

            let max_difficulty = self.difficulty * 2.0;
            let min_difficulty = self.difficulty / 2.0;

            self.difficulty = new_difficulty.clamp(min_difficulty, max_difficulty);
            
            if self.difficulty < 1.0 { self.difficulty = 1.0; }

            log::info!("Dificultad ajustada en el bloque #{}. Nueva dificultad: {:.2}", last_block.index, self.difficulty);
        }
    }

    pub fn is_valid_block(&self, block: &Block) -> bool {
        let last_block = match self.chain.last() {
            Some(b) => b,
            None => return false,
        };
        if block.previous_hash != last_block.hash {
            log::warn!("Fallo de validación: el 'previous_hash' no coincide.");
            return false;
        }
        if block.index != last_block.index + 1 {
            log::warn!("Fallo de validación: el 'index' es incorrecto.");
            return false;
        }
        let hasher = ZRZHasher::new();
        let hash_check = hasher.hash(&block.to_bytes(), block.nonce);
        if hex::encode(hash_check) != block.hash {
             log::warn!("Fallo de validación: El Hash no coincide. Calculado: {} pero el bloque tiene: {}", hex::encode(hash_check), block.hash);
             return false;
        }
        if !hash_check.iter().zip(block.target.iter()).all(|(h, t)| h <= t) {
            log::warn!("Fallo de validación: El Hash no cumple con el 'target'.");
            return false;
        }
        true
    }
    
    pub fn mine_block(&mut self, miner_address: &str) -> Result<Block, BlockchainError> {
        let last_block = self.chain.last().ok_or(BlockchainError::EmptyChain)?;
        
        let mut block_transactions: Vec<Transaction> = self.current_transactions.drain(..).collect();
        let mut total_fees = 0.0;
        
        for tx in &block_transactions {
            let input_sum = self.verify_inputs(tx)?;
            let output_sum: f64 = tx.outputs.iter().map(|o| o.amount).sum();
            if input_sum > output_sum {
                total_fees += input_sum - output_sum;
            }
        }
        
        let coinbase_tx = Transaction::new_coinbase(miner_address, self.reward + total_fees);
        block_transactions.insert(0, coinbase_tx);
        
        let mut block = Block::new(
            last_block.index + 1,
            Utc::now().timestamp(),
            last_block.hash.clone(),
            block_transactions,
            self.difficulty,
        );
        let hasher = ZRZHasher::new();
        let mut nonce = 0;
        loop {
            let hash_result = hasher.hash(&block.to_bytes(), nonce);
            if hash_result.iter().zip(block.target.iter()).all(|(h, t)| h <= t) {
                block.nonce = nonce;
                block.hash = hex::encode(hash_result);
                break;
            }
            nonce += 1;
        }
        Ok(block)
    }

    fn adjust_reward(&mut self) {
        self.reward *= 1.0 - (self.reduction_rate / 100.0);
        if self.reward < self.min_reward {
            self.reward = self.min_reward;
        }
    }

    pub fn get_last_block(&self) -> Result<&Block, BlockchainError> {
        self.chain.last().ok_or(BlockchainError::EmptyChain)
    }

    pub fn add_transaction(&mut self, tx: Transaction) -> Result<(), BlockchainError> {
        if !tx.verify_signature() {
            return Err(BlockchainError::TransactionError("Transacción inválida".into()));
        }
        self.current_transactions.push(tx);
        Ok(())
    }
    
    pub fn resolve_conflicts(&mut self, new_chain: Vec<Block>) -> bool {
        if new_chain.len() > self.chain.len() {
            log::info!("Conflicto resuelto: la cadena local de {} bloques es reemplazada por una más larga de {} bloques.", self.chain.len(), new_chain.len());
            self.chain = new_chain;
            self.rebuild_utxos();
            true
        } else {
            log::info!("No se reemplaza la cadena local. La nueva cadena no es más larga.");
            false
        }
    }
    
    fn rebuild_utxos(&mut self) {
        self.utxos.clear();
        for block in &self.chain {
            for tx in &block.transactions {
                if !tx.is_coinbase() {
                    for input in &tx.inputs {
                        let utxo_key = format!("{}:{}", input.tx_id, input.output_index);
                        self.utxos.remove(&utxo_key);
                    }
                }
                for (index, output) in tx.outputs.iter().enumerate() {
                    let utxo_key = format!("{}:{}", tx.id, index);
                    self.utxos.insert(utxo_key, output.clone());
                }
            }
        }
    }
}