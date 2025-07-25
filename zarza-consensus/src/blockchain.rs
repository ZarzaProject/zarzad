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
            info!("Bloque Génesis creado.");
        }
    }
    
    pub fn add_block(&mut self, block: Block) -> Result<(), BlockchainError> {
        // Validación preliminar antes de procesar transacciones
        if let Err(e) = self.pre_validate_block(&block) {
            warn!("Bloque rechazado por pre-validación: {}", e);
            return Err(e);
        }

        // Si el bloque es válido preliminarmente, procesamos las transacciones
        let total_fees = self.process_transactions(&block.transactions)?;

        // Si todas las transacciones son válidas, el bloque es finalmente aceptado
        self.chain.push(block);
        
        // Ajustar recompensa con las tarifas del bloque
        self.reward += total_fees;
        self.adjust_reward();
        self.adjust_difficulty();
        
        // El mempool se limpia de las transacciones incluidas en el bloque en el demonio o minero.
        // Aquí no limpiamos current_transactions para no afectar el mempool global del daemon/miner,
        // ya que este método se usa también para validar cadenas entrantes.

        info!("Bloque #{} aceptado y añadido a la cadena.", self.chain.last().unwrap().index);
        Ok(())
    }

    fn pre_validate_block(&self, block: &Block) -> Result<(), BlockchainError> {
        let last_block = self.chain.last().ok_or(BlockchainError::EmptyChain)?;

        if block.previous_hash != last_block.hash {
            warn!("Fallo de validación de bloque: el 'previous_hash' no coincide (esperado: {}, recibido: {}).", last_block.hash, block.previous_hash);
            return Err(BlockchainError::BlockValidationError("Previous hash no coincide".into()));
        }
        if block.index != last_block.index + 1 {
            warn!("Fallo de validación de bloque: el 'index' es incorrecto (esperado: {}, recibido: {}).", last_block.index + 1, block.index);
            return Err(BlockchainError::BlockValidationError("Índice de bloque incorrecto".into()));
        }
        
        let hasher = ZRZHasher::new();
        let hash_check = hasher.hash(&block.to_bytes(), block.nonce);
        if hex::encode(hash_check) != block.hash {
             warn!("Fallo de validación de bloque: El Hash no coincide. Calculado: {} pero el bloque tiene: {}", hex::encode(hash_check), block.hash);
             return Err(BlockchainError::BlockValidationError("Hash de bloque incorrecto".into()));
        }
        if !hash_check.iter().zip(block.target.iter()).all(|(h, t)| h <= t) {
            warn!("Fallo de validación de bloque: El Hash no cumple con el 'target'.");
            return Err(BlockchainError::BlockValidationError("Hash no cumple con el target".into()));
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
        // TODO: Validar la recompensa de la transacción coinbase.

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
    
    fn adjust_difficulty(&mut self) {
        let last_block_index = self.chain.len() as u64 - 1;
        if last_block_index == 0 { return; }

        if last_block_index % self.difficulty_adjust_blocks == 0 {
            let period_start_index = (last_block_index - self.difficulty_adjust_blocks) as usize;
            let period_start_block = match self.chain.get(period_start_index) {
                Some(block) => block,
                None => {
                    warn!("No se pudo encontrar el bloque de inicio del período para el ajuste de dificultad.");
                    return;
                },
            };
            
            let mut timestamps: Vec<i64> = self.chain[period_start_index..=last_block_index as usize]
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
            
            if time_taken_for_period <= 0 { 
                warn!("Tiempo de período para ajuste de dificultad inválido ({} segundos). No se ajusta la dificultad.", time_taken_for_period);
                return; 
            }

            let alpha = 2.0 / (self.difficulty_adjust_blocks as f64 + 1.0);
            let target_adjustment = self.difficulty * (expected_time_for_period as f64 / time_taken_for_period as f64);
            let mut new_difficulty = self.difficulty * (1.0 - alpha) + target_adjustment * alpha;

            let max_difficulty_change_factor = 2.0;
            let min_difficulty_change_factor = 0.5;
            
            let upper_bound = self.difficulty * max_difficulty_change_factor;
            let lower_bound = self.difficulty * min_difficulty_change_factor;

            new_difficulty = new_difficulty.clamp(lower_bound, upper_bound);
            
            if new_difficulty < 1.0 { new_difficulty = 1.0; }

            self.difficulty = new_difficulty;
            info!("Dificultad ajustada en el bloque #{}. Nueva dificultad: {:.2} (Tiempo tomado: {}s, Esperado: {}s)", last_block_index, self.difficulty, time_taken_for_period, expected_time_for_period);
        }
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

        info!("Minando bloque #{} con {} transacciones. Recompensa base: {:.2}, Tarifas: {:.2}", 
            last_block.index + 1, block_transactions.len(), self.reward, total_fees);
        
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
                info!("Bloque #{} minado con éxito. Nonce: {}, Hash: {}", block.index, block.nonce, block.hash);
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
            warn!("Transacción rechazada: firma inválida para {}", tx.id);
            return Err(BlockchainError::TransactionError("Transacción inválida: firma".into()));
        }
        // TODO: Validación de UTXO de entrada y doble gasto en mempool.
        // Esto es crucial para prevenir la aceptación de transacciones inválidas en el mempool.
        // Por ahora, solo se verifica la firma.
        self.current_transactions.push(tx);
        info!("Transacción añadida al mempool. ID: {}", self.current_transactions.last().unwrap().id);
        Ok(())
    }
    
    pub fn resolve_conflicts(&mut self, new_chain: Vec<Block>) -> bool {
        if new_chain.len() > self.chain.len() {
            info!("Conflicto detectado: la cadena local de {} bloques es más corta que la cadena entrante de {} bloques.", self.chain.len(), new_chain.len());
            if !self.validate_entire_chain(&new_chain) {
                warn!("La cadena entrante no es válida. No se reemplaza.");
                return false;
            }

            self.chain = new_chain;
            self.rebuild_utxos();
            // Limpiar el mempool para evitar transacciones que ya no son válidas en la nueva cadena.
            // Una solución más avanzada sería re-validar las transacciones del mempool contra la nueva cadena.
            self.current_transactions.clear();
            info!("Conflicto resuelto: la cadena local ha sido reemplazada por una más larga y válida. Mempool limpiado.");
            true
        } else {
            info!("No se reemplaza la cadena local. La nueva cadena no es más larga.");
            false
        }
    }
    
    fn validate_entire_chain(&self, chain_to_validate: &[Block]) -> bool {
        if chain_to_validate.is_empty() {
            warn!("La cadena a validar está vacía.");
            return false;
        }
        // Validar el bloque Génesis
        if chain_to_validate[0].index != 0 || chain_to_validate[0].previous_hash != "0" || chain_to_validate[0].hash != "0000000000000000000000000000000000000000000000000000000000000000" {
            warn!("Bloque génesis inválido en la cadena a validar.");
            return false;
        }

        let mut temp_blockchain = Blockchain {
            chain: vec![chain_to_validate[0].clone()],
            current_transactions: Vec::new(), // El mempool temporal no es relevante aquí
            utxos: HashMap::new(),
            difficulty: self.difficulty,
            reward: self.reward,
            min_reward: self.min_reward,
            reduction_rate: self.reduction_rate,
            block_time: self.block_time,
            difficulty_adjust_blocks: self.difficulty_adjust_blocks,
        };
        // Reconstruir UTXOs para el bloque génesis
        temp_blockchain.rebuild_utxos();


        for i in 1..chain_to_validate.len() {
            let block = &chain_to_validate[i];
            if let Err(e) = temp_blockchain.add_block(block.clone()) {
                warn!("Fallo al añadir bloque {} a la cadena temporal durante la validación: {}", block.index, e);
                return false;
            }
        }
        true
    }

    fn rebuild_utxos(&mut self) {
        self.utxos.clear();
        info!("Reconstruyendo UTXOs desde la cadena actual...");
        let mut temp_utxos: HashMap<String, TxOutput> = HashMap::new();
        for block in &self.chain {
            for tx in &block.transactions {
                if !tx.is_coinbase() {
                    for input in &tx.inputs {
                        let utxo_key = format!("{}:{}", input.tx_id, input.output_index);
                        if temp_utxos.remove(&utxo_key).is_none() {
                            warn!("UTXO no encontrada al reconstruir para input {}. Posible cadena inválida o error lógico.", utxo_key);
                        }
                    }
                }
                for (index, output) in tx.outputs.iter().enumerate() {
                    let utxo_key = format!("{}:{}", tx.id, index);
                    temp_utxos.insert(utxo_key, output.clone());
                }
            }
        }
        self.utxos = temp_utxos;
        info!("Reconstrucción de UTXOs completada. Total UTXOs: {}", self.utxos.len());
    }
}