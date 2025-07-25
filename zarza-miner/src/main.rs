use std::error::Error;
use log::{info, warn};
use chrono::Utc;
use clap::Parser;
use reqwest::blocking::Client;
use serde_json::json;
use zarza_consensus::block::Block;
use zarza_consensus::blockchain::Blockchain;
use zrzhash::ZRZHasher;
use config::Config;
use std::time::Instant;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use zarza_consensus::transaction::Transaction;

mod cli;

#[derive(serde::Deserialize)]
struct BlockchainState {
    last_block: Block,
    difficulty: f64,
}

struct Miner {
    blockchain: Mutex<Blockchain>,
    address: String,
    hasher: ZRZHasher,
    node_url: String,
    client: Client,
    blocks_submitted: u64,
    total_hashes: u128,
    start_time: Instant,
}

impl Miner {
    fn new(blockchain: Blockchain, address: String, node_url: String) -> Self {
        Miner {
            blockchain: Mutex::new(blockchain),
            address,
            hasher: ZRZHasher::new(),
            node_url,
            client: Client::new(),
            blocks_submitted: 0,
            total_hashes: 0,
            start_time: Instant::now(),
        }
    }

    fn start_mining(&mut self, threads: usize) -> Result<(), Box<dyn Error + Send + Sync>> {
        info!("Minero iniciado para la dirección: {}", self.address);
        loop {
            let (last_block, difficulty, reward, mempool_txs) = {
                let bc_lock = self.blockchain.lock().unwrap();
                (
                    bc_lock.chain.last().unwrap().clone(),
                    bc_lock.difficulty,
                    bc_lock.reward,
                    bc_lock.current_transactions.clone(),
                )
            };
            
            let block_height = last_block.index + 1;
            info!("⛏️  Comenzando a minar bloque #{} con dificultad {:.2}", block_height, difficulty);
            
            let coinbase_tx = Transaction::new_coinbase(&self.address, reward);
            let mut block_transactions = vec![coinbase_tx];
            block_transactions.extend(mempool_txs);

            let mut block = Block::new(
                block_height,
                Utc::now().timestamp(),
                last_block.hash.clone(),
                block_transactions,
                difficulty,
            );
            
            let target = Block::calculate_target(difficulty);
            let found_block_nonce = Arc::new(Mutex::new(None));
            let mining_stopped = Arc::new(AtomicBool::new(false));
            
            let mut handles = vec![];
            for i in 0..threads {
                let block_clone = block.clone();
                let target_clone = target;
                let found_block_nonce_clone = found_block_nonce.clone();
                let mining_stopped_clone = mining_stopped.clone();
                let hasher_clone = self.hasher.clone();

                handles.push(thread::spawn(move || {
                    let mut nonce = i as u64;
                    loop {
                        if mining_stopped_clone.load(Ordering::Relaxed) {
                            return None;
                        }

                        let hash_result = hasher_clone.hash(&block_clone.to_bytes(), nonce);
                        if hash_result.iter().zip(target_clone.iter()).all(|(h, t)| h <= t) {
                            let mut found = found_block_nonce_clone.lock().unwrap();
                            *found = Some(nonce);
                            mining_stopped_clone.store(true, Ordering::Relaxed);
                            return Some(nonce);
                        }
                        nonce += threads as u64;
                    }
                }));
            }

            let mut found_nonce: Option<u64> = None;
            for handle in handles {
                if let Some(nonce) = handle.join().unwrap() {
                    found_nonce = Some(nonce);
                }
            }

            if let Some(nonce) = found_nonce {
                let duration = Instant::now().elapsed();
                let secs = duration.as_secs_f64().max(1e-9);
                let hashrate = nonce as f64 / secs;
                
                self.total_hashes += nonce as u128;
                
                info!("🎉 ¡Bloque encontrado! Nonce: {}, Hashrate: {:.2} H/s", nonce, hashrate);
                
                block.nonce = nonce;
                let hash_result = self.hasher.hash(&block.to_bytes(), nonce);
                block.hash = hex::encode(hash_result);

                info!("   Enviando bloque #{} al nodo...", block.index);
                match self.submit_block(&block) {
                    Ok(_) => {
                        self.blocks_submitted += 1;
                        let total_secs = self.start_time.elapsed().as_secs_f64().max(1.0);
                        let avg_hashrate = self.total_hashes as f64 / total_secs;

                        println!("======================================================");
                        info!("✅ Bloque Aceptado!");
                        info!("   Block Height: {}", block.index);
                        info!("   Bloques enviados (sesión): {}", self.blocks_submitted);
                        info!("   Hashrate promedio: {:.2} H/s", avg_hashrate);
                        println!("======================================================");

                        self.blockchain.lock().unwrap().add_block(block)?;
                    }
                    Err(e) => {
                        warn!("❌ Fallo al enviar el bloque: {}", e);
                        warn!("   Puede que la red haya encontrado un bloque antes. Resincronizando...");
                    }
                }
            }
        }
    }

    fn submit_block(&self, block: &Block) -> Result<(), Box<dyn Error>> {
        let response = self.client.post(&format!("{}/rpc", self.node_url))
            .json(&json!({ "jsonrpc": "2.0", "method": "submit_block", "params": [block], "id": 1 }))
            .send()?;
        let response_json: serde_json::Value = response.json()?;
        if let Some(error) = response_json.get("error") {
             return Err(format!("El nodo rechazó el bloque: {}", error).into());
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    pretty_env_logger::init();
    let args = cli::CliArgs::parse();

    info!("Iniciando Zarza Miner v{}", env!("CARGO_PKG_VERSION"));
    
    let settings = Config::builder()
        .add_source(config::File::with_name("config/config"))
        .build()?;

    info!("Sincronizando con el nodo en {}...", &args.node);
    let client = Client::new();
    let response = client.post(&format!("{}/rpc", &args.node))
        .json(&json!({"jsonrpc": "2.0", "method": "get_blockchain_state", "id": 1}))
        .send()?;

    let rpc_response: serde_json::Value = response.json()?;
    if let Some(error) = rpc_response.get("error") {
        return Err(format!("Error al sincronizar con el nodo: {}", error).into());
    }
    let state: BlockchainState = serde_json::from_value(rpc_response["result"].clone())?;
    
    let mut blockchain = Blockchain::new(&settings)?;
    blockchain.difficulty = state.difficulty;
    blockchain.chain = vec![state.last_block];

    info!("Sincronización completa. Block Height actual: #{}", blockchain.chain[0].index);
    info!("Dificultad para el siguiente bloque: {:.2}", blockchain.difficulty);

    let mut miner = Miner::new(
        blockchain,
        args.address,
        args.node,
    );
    
    miner.start_mining(args.threads)
}