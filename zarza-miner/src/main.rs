use std::error::Error;
use log::{info, warn, error};
use clap::Parser;
use reqwest::blocking::Client;
use serde_json::json;
use zarza_consensus::block::Block;
use zarza_consensus::transaction::Transaction;
use zrzhash::ZRZHasher;
use config::Config;
use std::sync::Arc; // Se elimina Mutex de la importación
use std::sync::atomic::{AtomicBool, Ordering, AtomicU64};
use std::thread;
use std::time::Duration;
use rand::Rng;

mod cli;

#[derive(serde::Deserialize, Debug)]
struct MiningJob {
    last_block: Block,
    difficulty: f64,
    current_transactions: Vec<Transaction>,
}

struct MinerSharedState {
    current_job_height: AtomicU64,
    total_hashes: AtomicU64,
    session_accepted: AtomicU64,
    session_rejected: AtomicU64,
    is_shutdown_requested: AtomicBool,
}

impl MinerSharedState {
    fn new() -> Self {
        Self {
            current_job_height: AtomicU64::new(0),
            total_hashes: AtomicU64::new(0),
            session_accepted: AtomicU64::new(0),
            session_rejected: AtomicU64::new(0),
            is_shutdown_requested: AtomicBool::new(false),
        }
    }
}

struct Miner {
    address: String,
    node_url: String,
    client: Client,
    threads: usize,
    report_interval: u64,
    shared_state: Arc<MinerSharedState>,
}

impl Miner {
    fn new(address: String, node_url: String, threads: usize, report_interval: u64) -> Self {
        Miner {
            address,
            node_url,
            client: Client::new(),
            threads,
            report_interval,
            shared_state: Arc::new(MinerSharedState::new()),
        }
    }

    fn fetch_job(&self) -> Result<Option<MiningJob>, Box<dyn Error>> {
        let response = self.client.post(&format!("{}/rpc", self.node_url))
            .json(&json!({"jsonrpc": "2.0", "method": "get_blockchain_state", "id": 1}))
            .send()?;
        
        let rpc_response: serde_json::Value = response.json()?;
        if let Some(error) = rpc_response.get("error") {
            return Err(format!("Error del nodo: {}", error).into());
        }
        
        Ok(serde_json::from_value(rpc_response["result"].clone())?)
    }

    fn submit_block(&self, block: &Block) -> Result<bool, Box<dyn Error>> {
        let response = self.client.post(&format!("{}/rpc", self.node_url))
            .json(&json!({ "jsonrpc": "2.0", "method": "submit_block", "params": [block], "id": 1 }))
            .send()?;
        
        let response_json: serde_json::Value = response.json()?;
        if let Some(error) = response_json.get("error") {
             warn!("El nodo rechazó el bloque: {}", error);
             Ok(false)
        } else {
             Ok(true)
        }
    }

    pub fn run(&self) {
        info!("Iniciando {} hilos de minería...", self.threads);
        
        let shutdown_state = self.shared_state.clone();
        ctrlc::set_handler(move || {
            info!("Señal de apagado recibida. Terminando hilos...");
            shutdown_state.is_shutdown_requested.store(true, Ordering::Relaxed);
        }).expect("Error al establecer el manejador de Ctrl-C");

        let state_clone = self.shared_state.clone();
        let node_url_clone = self.node_url.clone();
        let client_clone = self.client.clone();
        thread::spawn(move || {
            loop {
                if state_clone.is_shutdown_requested.load(Ordering::Relaxed) { break; }
                
                if let Ok(response) = client_clone.post(&format!("{}/rpc", node_url_clone))
                    .json(&json!({"jsonrpc": "2.0", "method": "get_blockchain_state", "id": "watcher"}))
                    .send()
                {
                    if let Ok(rpc_response) = response.json::<serde_json::Value>() {
                        if let Some(result) = rpc_response.get("result") {
                            if let Ok(job) = serde_json::from_value::<MiningJob>(result.clone()) {
                                let network_height = job.last_block.index + 1;
                                let miner_height = state_clone.current_job_height.load(Ordering::Relaxed);
                                if network_height > miner_height {
                                    warn!("¡Bloque obsoleto detectado! La red está en el bloque #{}. Reiniciando mineros...", network_height);
                                    state_clone.current_job_height.store(0, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                }
                thread::sleep(Duration::from_secs(5));
            }
        });

        let state_clone = self.shared_state.clone();
        let report_interval = self.report_interval;
        thread::spawn(move || {
            loop {
                let last_hash_count = state_clone.total_hashes.swap(0, Ordering::Relaxed);
                let hashrate = last_hash_count as f64 / report_interval as f64;
                
                info!(
                    "Hashrate: {:.2} H/s | Aceptados: {} | Rechazados: {}",
                    hashrate,
                    state_clone.session_accepted.load(Ordering::Relaxed),
                    state_clone.session_rejected.load(Ordering::Relaxed)
                );

                for _ in 0..report_interval {
                    if state_clone.is_shutdown_requested.load(Ordering::Relaxed) { return; }
                    thread::sleep(Duration::from_secs(1));
                }
            }
        });

        // Se elimina la label 'main_loop'
        loop {
            if self.shared_state.is_shutdown_requested.load(Ordering::Relaxed) { break; }

            info!("Sincronizando con el nodo para obtener un nuevo trabajo...");
            let job = match self.fetch_job() {
                Ok(Some(j)) => j,
                _ => {
                    error!("No se pudo obtener un trabajo del nodo. Reintentando en 10 segundos...");
                    thread::sleep(Duration::from_secs(10));
                    continue;
                }
            };
            
            let block_height = job.last_block.index + 1;
            self.shared_state.current_job_height.store(block_height, Ordering::Relaxed);
            info!("⛏️  Nuevo trabajo recibido. Minando bloque #{} (Dificultad: {:.2})", block_height, job.difficulty);

            let mut block_transactions = job.current_transactions;
            block_transactions.insert(0, Transaction::new_coinbase(&self.address, 0.0));
            
            let block_template = Block::new(
                block_height,
                chrono::Utc::now().timestamp(),
                job.last_block.hash.clone(),
                block_transactions,
                job.difficulty,
            );

            let solution_found = Arc::new(AtomicBool::new(false));
            let solution_nonce = Arc::new(AtomicU64::new(0));

            let mut mining_handles = vec![];
            for _ in 0..self.threads {
                let job_state = self.shared_state.clone();
                let block_data = block_template.to_bytes();
                let target = block_template.target;
                let found_flag = solution_found.clone();
                let nonce_result = solution_nonce.clone();
                
                mining_handles.push(thread::spawn(move || {
                    let hasher = ZRZHasher::new();
                    let mut nonce = rand::thread_rng().gen::<u64>();
                    
                    while !found_flag.load(Ordering::Relaxed) && job_state.current_job_height.load(Ordering::Relaxed) == block_height {
                        if hasher.verify(&block_data, nonce, &target) {
                            if !found_flag.swap(true, Ordering::Relaxed) {
                                nonce_result.store(nonce, Ordering::Relaxed);
                            }
                            break;
                        }
                        nonce = nonce.wrapping_add(1);
                        job_state.total_hashes.fetch_add(1, Ordering::Relaxed);
                    }
                }));
            }

            for handle in mining_handles {
                handle.join().unwrap();
            }

            if solution_found.load(Ordering::Relaxed) {
                let final_nonce = solution_nonce.load(Ordering::Relaxed);
                info!("🎉 ¡Solución encontrada para el bloque #{}! Nonce: {}", block_height, final_nonce);
                
                let mut final_block = block_template.clone();
                final_block.nonce = final_nonce;
                final_block.hash = hex::encode(ZRZHasher::new().hash(&final_block.to_bytes(), final_nonce));
                
                info!("   Enviando bloque al nodo para validación...");
                match self.submit_block(&final_block) {
                    Ok(true) => {
                        info!("✅ ¡Bloque ACEPTADO por la red!");
                        self.shared_state.session_accepted.fetch_add(1, Ordering::Relaxed);
                    },
                    _ => {
                        warn!("❌ Bloque RECHAZADO.");
                        self.shared_state.session_rejected.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();
    let args = cli::CliArgs::parse();
    
    let settings = Config::builder()
        .add_source(config::File::with_name("config/config"))
        .build()?;
    let report_interval = settings.get_int("mining.report_interval")? as u64;

    info!("Iniciando Zarza Miner v1.1 (Modo Profesional Hydra)");
    
    let miner = Miner::new(args.address, args.node, args.threads, report_interval);
    miner.run();
    
    Ok(())
}