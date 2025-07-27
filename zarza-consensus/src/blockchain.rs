use std::collections::{HashMap, HashSet};
use chrono::Utc;
use zrzhash::ZRZHasher;
use crate::{block::Block, transaction::{Transaction, TxOutput}};
use serde::{Serialize, Deserialize};
use thiserror::Error;
use log::{info, warn, debug};
use std::error::Error;

#[derive(Error, Debug)]
pub enum BlockchainError {
    #[error("El bloque propuesto es inválido")]
    InvalidBlock,
    #[error("La blockchain está vacía")]
    EmptyChain,
    #[error("Error de transacción: {0}")]
    TransactionError(String),
    #[error("Fallo de validación de bloque: {0}")]
    BlockValidationError(String),
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
            let initial_difficulty = self.difficulty;
            let genesis_block = Block {
                index: 0,
                timestamp: Utc::now().timestamp(),
                transactions: vec![],
                previous_hash: String::from("0"),
                hash: hex::encode(ZRZHasher::new().hash(b"genesis", 0)),
                nonce: 0,
                target: Block::calculate_target(initial_difficulty),
            };
            self.chain.push(genesis_block);
            info!("Bloque Génesis creado con el hash de Zarza-Hydra.");
        }
    }

    pub fn add_block(&mut self, block: Block) -> Result<(), BlockchainError> {
        self.pre_validate_block(&block)?;
        let total_fees = self.process_transactions(&block.transactions)?;
        self.chain.push(block.clone());
        self.difficulty = self.calculate_next_difficulty();
        self.adjust_reward(total_fees);
        info!("Bloque #{} [hash: ...{}] aceptado. Nueva dificultad para el siguiente bloque: {:.2}",
            block.index, &block.hash[block.hash.len()-6..], self.difficulty);
        Ok(())
    }

    // --- FUNCIÓN DE VALIDACIÓN CORREGIDA ---
    fn pre_validate_block(&self, block: &Block) -> Result<(), BlockchainError> {
        let last_block = self.chain.last().ok_or(BlockchainError::EmptyChain)?;

        if block.index != last_block.index + 1 {
            return Err(BlockchainError::BlockValidationError(format!(
                "Índice incorrecto. Se esperaba {} pero se recibió {}.", last_block.index + 1, block.index
            )));
        }
        if block.previous_hash != last_block.hash {
            return Err(BlockchainError::BlockValidationError(format!(
                "Hash previo incorrecto para el bloque {}.", block.index
            )));
        }

        // --- LA CORRECCIÓN CLAVE ---
        // En lugar de comparar los `f64` de dificultad, que son imprecisos,
        // calculamos el `target` que esperábamos y lo comparamos byte por byte.
        let expected_target = Block::calculate_target(self.difficulty);
        if block.target != expected_target {
            return Err(BlockchainError::BlockValidationError(format!(
                "Target incorrecto en el bloque. Se esperaba {:?} pero se recibió {:?}.",
                expected_target, block.target
            )));
        }

        let hasher = ZRZHasher::new();
        if !hasher.verify(&block.to_bytes(), block.nonce, &block.target) {
            warn!("Fallo de validación de PoW de Hydra para el bloque #{}.", block.index);
            return Err(BlockchainError::BlockValidationError("La prueba de trabajo es inválida.".into()));
        }

        let calculated_hash = hex::encode(hasher.hash(&block.to_bytes(), block.nonce));
        if calculated_hash != block.hash {
            return Err(BlockchainError::BlockValidationError(format!(
                "El hash del bloque no coincide. Calculado: {}, Recibido: {}", calculated_hash, block.hash
            )));
        }

        if block.transactions.is_empty() {
            return Err(BlockchainError::BlockValidationError("El bloque no puede estar vacío de transacciones.".into()));
        }
        let coinbase_tx = &block.transactions[0];
        if !coinbase_tx.is_coinbase() {
            return Err(BlockchainError::BlockValidationError("La primera transacción debe ser coinbase.".into()));
        }
        if !coinbase_tx.inputs.is_empty() {
            return Err(BlockchainError::BlockValidationError("La transacción coinbase no debe tener entradas.".into()));
        }

        for tx in block.transactions.iter().skip(1) {
            if tx.is_coinbase() {
                return Err(BlockchainError::BlockValidationError("Solo puede haber una transacción coinbase por bloque.".into()));
            }
            if !tx.verify_signature() {
                return Err(BlockchainError::TransactionError(format!("Firma inválida en la transacción {}", tx.id)));
            }
        }

        Ok(())
    }

    fn calculate_next_difficulty(&self) -> f64 {
        let window_size = self.difficulty_adjust_blocks as usize;
        let chain_len = self.chain.len();

        if chain_len < window_size {
            return self.difficulty;
        }

        let anchor_block = &self.chain[chain_len - window_size];
        let current_block = self.chain.last().unwrap();

        let time_elapsed = current_block.timestamp - anchor_block.timestamp;
        let blocks_elapsed = current_block.index - anchor_block.index;

        if time_elapsed <= 0 || blocks_elapsed == 0 {
            return self.difficulty;
        }

        let target_timespan = (blocks_elapsed * self.block_time) as f64;
        let halflife = (window_size as f64) * self.block_time as f64 * 10.0;
        let exponent = (target_timespan - time_elapsed as f64) / halflife;
        let adjustment_factor = 2.0_f64.powf(exponent);
        let clamped_factor = adjustment_factor.max(0.5).min(2.0);
        let new_difficulty = self.difficulty * clamped_factor;

        if (chain_len + 1) % 10 == 0 {
            info!(
                "Cálculo de dificultad para bloque #{}: Tiempo real/bloque: {:.2}s, Objetivo: {}s, Factor: {:.4}, Nueva Dificultad: {:.2}",
                chain_len + 1,
                time_elapsed as f64 / blocks_elapsed as f64,
                self.block_time,
                clamped_factor,
                new_difficulty
            );
        }

        new_difficulty.max(1.0)
    }

    fn process_transactions(&mut self, transactions: &[Transaction]) -> Result<f64, BlockchainError> {
        let mut total_fees = 0.0;
        let mut spent_utxos_in_block: HashSet<String> = HashSet::new();

        for tx in transactions {
            if tx.is_coinbase() {
                self.update_utxos_with_new_outputs(tx);
                continue;
            }

            for input in &tx.inputs {
                let utxo_key = format!("{}:{}", input.tx_id, input.output_index);
                if spent_utxos_in_block.contains(&utxo_key) {
                    return Err(BlockchainError::TransactionError(format!("Doble gasto detectado dentro del mismo bloque para UTXO: {}", utxo_key)));
                }
                spent_utxos_in_block.insert(utxo_key);
            }

            let input_sum = self.verify_inputs(tx)?;
            let output_sum: f64 = tx.outputs.iter().map(|o| o.amount).sum();

            if input_sum < output_sum {
                return Err(BlockchainError::TransactionError(format!("Fondos insuficientes en la transacción {}. Entradas: {}, Salidas: {}", tx.id, input_sum, output_sum)));
            }

            total_fees += input_sum - output_sum;

            self.consume_inputs_as_utxos(tx);
            self.update_utxos_with_new_outputs(tx);
        }
        Ok(total_fees)
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
            debug!("UTXO consumida: {}", utxo_key);
        }
    }

    fn update_utxos_with_new_outputs(&mut self, tx: &Transaction) {
        for (index, output) in tx.outputs.iter().enumerate() {
            let utxo_key = format!("{}:{}", tx.id, index);
            self.utxos.insert(utxo_key.clone(), output.clone());
            debug!("Nueva UTXO creada: {} con {} ZRZ a {}", utxo_key, output.amount, output.address);
        }
    }

    pub fn get_balance(&self, address: &str) -> f64 {
        self.utxos.values().filter(|utxo| utxo.address == address).map(|utxo| utxo.amount).sum()
    }

    fn adjust_reward(&mut self, fees: f64) {
        let base_reward = self.reward;
        let new_base_reward = base_reward * (1.0 - self.reduction_rate);
        self.reward = new_base_reward.max(self.min_reward) + fees;
    }

    pub fn get_last_block(&self) -> Result<&Block, BlockchainError> {
        self.chain.last().ok_or(BlockchainError::EmptyChain)
    }

    pub fn add_transaction(&mut self, tx: Transaction) -> Result<(), BlockchainError> {
        if !tx.verify_signature() {
            warn!("Transacción rechazada: firma inválida para {}", tx.id);
            return Err(BlockchainError::TransactionError("Transacción inválida: firma".into()));
        }
        self.current_transactions.push(tx.clone());
        info!("Transacción {} añadida al mempool.", tx.id);
        Ok(())
    }

    pub fn resolve_conflicts(&mut self, new_chain: Vec<Block>) -> bool {
        if new_chain.len() <= self.chain.len() {
            info!("No se reemplaza la cadena local. La nueva cadena no es más larga.");
            return false;
        }

        info!("Conflicto detectado: la cadena entrante ({} bloques) es más larga que la local ({} bloques). Validando...", new_chain.len(), self.chain.len());
        if !self.validate_entire_chain(&new_chain) {
            warn!("La cadena entrante no es válida. No se reemplaza.");
            return false;
        }

        self.chain = new_chain;
        self.rebuild_utxos();
        self.current_transactions.clear();
        info!("Conflicto resuelto: la cadena local ha sido reemplazada por una más larga y válida. Mempool limpiado.");
        true
    }

    fn validate_entire_chain(&self, chain_to_validate: &[Block]) -> bool {
        if chain_to_validate.is_empty() {
            warn!("La cadena a validar está vacía.");
            return false;
        }

        let genesis_hash = hex::encode(ZRZHasher::new().hash(b"genesis", 0));
        if chain_to_validate[0].index != 0 || chain_to_validate[0].hash != genesis_hash {
            warn!("Bloque génesis inválido en la cadena a validar.");
            return false;
        }

        let mut temp_blockchain = self.clone();
        temp_blockchain.chain = vec![chain_to_validate[0].clone()];
        temp_blockchain.utxos.clear();
        temp_blockchain.rebuild_utxos_from_chain(&temp_blockchain.chain.clone());


        for i in 1..chain_to_validate.len() {
            let block = &chain_to_validate[i];
            if let Err(e) = temp_blockchain.add_block(block.clone()) {
                warn!("Fallo al añadir bloque {} a la cadena temporal durante la validación: {}", block.index, e);
                return false;
            }
        }
        true
    }

    fn rebuild_utxos_from_chain(&mut self, chain: &[Block]) {
        self.utxos.clear();
        for block in chain {
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

    fn rebuild_utxos(&mut self) {
        info!("Reconstruyendo UTXOs desde la cadena actual...");
        let chain_clone = self.chain.clone();
        self.rebuild_utxos_from_chain(&chain_clone);
        info!("Reconstrucción de UTXOs completada. Total UTXOs: {}", self.utxos.len());
    }
}