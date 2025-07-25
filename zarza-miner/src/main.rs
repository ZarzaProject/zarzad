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
use std::time::Instant; // Importamos solo Instant, que es el que usamos.

mod cli;

#[derive(serde::Deserialize)]
struct BlockchainState {
    last_block: Block,
    difficulty: f64,
}

struct Miner {
    blockchain: Blockchain,
    address: String,
    hasher: ZRZHasher,
    node_url: String,
    client: Client,
    blocks_submitted: u64,
    total_hashes: u128,
    start_time: Instant,
}

impl Miner {
    fn new(blockchain: Blockchain, address: String, _threads: usize, node_url: String) -> Self {
        Miner {
            blockchain,
            address,
            hasher: ZRZHasher::new(),
            node_url,
            client: Client::new(),
            blocks_submitted: 0,
            total_hashes: 0,
            start_time: Instant::now(),
        }
    }

    fn mine_block(&mut self) -> Result<Block, Box<dyn Error>> {
        let last_block = self.blockchain.chain.last().ok_or("Blockchain vacía")?;
        let block_height = last_block.index + 1;
        
        info!("⛏️  Comenzando a minar bloque #{} con dificultad {:.2}", block_height, self.blockchain.difficulty);
        
        let mut block = Block::new(
            block_height,
            Utc::now().timestamp(),
            last_block.hash.clone(),
            self.blockchain.current_transactions.clone(),
            self.blockchain.difficulty,
        );

        let target = Block::calculate_target(self.blockchain.difficulty);
        let mut nonce = 0u64;
        let block_start_time = Instant::now();

        loop {
            nonce += 1;
            let hash_result = self.hasher.hash(&block.to_bytes(), nonce);

            if hash_result.iter().zip(target.iter()).all(|(h, t)| h <= t) {
                block.nonce = nonce;
                block.hash = hex::encode(hash_result);
                
                let duration = block_start_time.elapsed();
                // Evitamos división por cero si el bloque se encuentra instantáneamente
                let secs = duration.as_secs_f64().max(1e-9);
                let hashrate = nonce as f64 / secs;
                
                self.total_hashes += nonce as u128;
                
                info!("🎉 ¡Bloque encontrado! Nonce: {}, Hashrate: {:.2} H/s", nonce, hashrate);
                break;
            }
            
            if nonce % 2_000_000 == 0 {
                let duration = block_start_time.elapsed();
                let secs = duration.as_secs_f64().max(1e-9);
                let hashrate = nonce as f64 / secs;
                info!("   Progreso... Nonce: {}, Hashrate: {:.2} H/s", nonce, hashrate);
            }
        }
        Ok(block)
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

    fn start(&mut self) -> Result<(), Box<dyn Error>> {
        info!("Minero iniciado para la dirección: {}", self.address);
        loop {
            let block = self.mine_block()?;
            info!("   Enviando bloque #{} al nodo...", block.index);
            match self.submit_block(&block) {
                Ok(_) => {
                    self.blocks_submitted += 1;
                    let total_secs = self.start_time.elapsed().as_secs_f64().max(1.0);
                    let avg_hashrate = self.total_hashes as f64 / total_secs;

                    println!("======================================================");
                    info!("✅ Bloque Aceptado!");
                    info!("   Block Height: {}", block.index);
                    info!("    Recompensa: {} ZRZ", self.blockchain.reward);
                    info!("   Bloques enviados (sesión): {}", self.blocks_submitted);
                    info!("   Hashrate promedio: {:.2} H/s", avg_hashrate);
                    println!("======================================================");

                    self.blockchain.add_block(block)?;
                }
                Err(e) => {
                    warn!("❌ Fallo al enviar el bloque: {}", e);
                    warn!("   Puede que la red haya encontrado un bloque antes. Resincronizando...");
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
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
        args.threads,
        args.node,
    );
    
    miner.start()
}
