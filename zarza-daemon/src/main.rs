use std::error::Error;
use std::sync::{Arc, Mutex};
use log::{info, warn, error, debug};
use config::Config;
use zarza_consensus::blockchain::Blockchain;
use zarza_consensus::transaction::Transaction;
use zarza_network::p2p::P2PNode;
use warp::Filter;
use serde_json::json;
use zarza_consensus::block::Block;
use std::fs;
use std::path::Path;
use std::collections::{HashMap, HashSet};
use tokio::task;
use serde::Deserialize;

#[derive(serde::Serialize)]
struct BlockchainState {
    last_block: Block,
    difficulty: f64,
    current_transactions: Vec<Transaction>,
}

#[derive(Deserialize, Debug)]
struct GetBlockByHeightParams {
    height: u64,
}

#[derive(Deserialize, Debug)]
struct GetTransactionByIdParams {
    id: String,
}

fn save_blockchain(blockchain: &Blockchain) -> Result<(), Box<dyn Error + Send + Sync>> {
    let path_str = "data/chain.json";
    info!("Guardando la blockchain en {}...", path_str);
    if let Some(parent) = Path::new(path_str).parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(blockchain)?;
    fs::write(path_str, data)?;
    info!("Blockchain guardada con éxito.");
    Ok(())
}

fn load_blockchain(settings: &Config) -> Result<Blockchain, Box<dyn Error + Send + Sync>> {
    let path_str = "data/chain.json";
    if Path::new(path_str).exists() {
        info!("Encontrada blockchain existente. Cargando desde {}...", path_str);
        let data = fs::read_to_string(path_str)?;
        let blockchain: Blockchain = serde_json::from_str(&data)?;
        info!("Blockchain cargada. Altura actual: {}", blockchain.chain.len() - 1);
        Ok(blockchain)
    } else {
        info!("No se encontró una blockchain. Creando una nueva desde cero...");
        let blockchain = Blockchain::new(settings)?;
        save_blockchain(&blockchain)?;
        Ok(blockchain)
    }
}

fn save_mempool(blockchain: &Blockchain) -> Result<(), Box<dyn Error + Send + Sync>> {
    let path_str = "data/mempool.json";
    info!("Guardando el mempool en {}...", path_str);
    let data = serde_json::to_string_pretty(&blockchain.current_transactions)?;
    fs::write(path_str, data)?;
    info!("Mempool guardado con éxito.");
    Ok(())
}

fn load_mempool(blockchain: &mut Blockchain) -> Result<(), Box<dyn Error + Send + Sync>> {
    let path_str = "data/mempool.json";
    if Path::new(path_str).exists() {
        info!("Encontrado mempool existente. Cargando desde {}...", path_str);
        let data = fs::read_to_string(path_str)?;
        let transactions: Vec<Transaction> = serde_json::from_str(&data)?;
        info!("Cargadas {} transacciones. Re-validando...", transactions.len());
        
        for tx in transactions {
            if blockchain.add_transaction(tx).is_ok() {
                debug!("Transacción re-validada y añadida al mempool.");
            } else {
                warn!("Transacción del mempool cargado es inválida y ha sido descartada.");
            }
        }
        fs::remove_file(path_str)?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    pretty_env_logger::init();
    info!("Iniciando Zarza Daemon v{} (Modo Hydra - Seguridad Activada)", env!("CARGO_PKG_VERSION"));
    
    let settings = Config::builder().add_source(config::File::with_name("config/config")).build()?;
    
    let mut initial_blockchain = load_blockchain(&settings)?;
    load_mempool(&mut initial_blockchain)?;
    
    let blockchain = Arc::new(Mutex::new(initial_blockchain));
    
    let p2p_node = Arc::new(P2PNode::new(&settings, blockchain.clone()).await?);
    let p2p_handle = task::spawn(p2p_node.clone().start());
    
    let blockchain_rpc = blockchain.clone();

    let shutdown_blockchain = blockchain.clone();
    ctrlc::set_handler(move || {
        info!("Señal de apagado (Ctrl-C) recibida. Guardando estado...");
        let bc_lock = shutdown_blockchain.lock().unwrap();
        if let Err(e) = save_blockchain(&bc_lock) {
            error!("¡FALLO CRÍTICO al guardar la blockchain!: {}", e);
        }
        if let Err(e) = save_mempool(&bc_lock) {
            error!("¡FALLO CRÍTICO al guardar el mempool!: {}", e);
        }
        info!("Guardado completado. Apagando.");
        std::process::exit(0);
    }).expect("Error al establecer el manejador de Ctrl-C");

    let rpc_route = warp::path("rpc").and(warp::post()).and(warp::body::json()).and_then(move |body: serde_json::Value| {
        let blockchain_rpc_clone = blockchain_rpc.clone();
        
        async move {
            let method_name = body["method"].as_str().unwrap_or("unknown");
            info!("[RPC] Solicitud recibida -> {}", method_name);

            let reply = if let Some(method) = body["method"].as_str() {
                match method {
                    "get_blockchain_state" => {
                        let bc_lock = blockchain_rpc_clone.lock().unwrap();
                        match bc_lock.get_last_block() {
                            Ok(last_block) => {
                                let state = BlockchainState { 
                                    last_block: last_block.clone(), 
                                    difficulty: bc_lock.difficulty,
                                    current_transactions: bc_lock.current_transactions.clone(),
                                };
                                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": state, "id": body["id"]})))
                            },
                            Err(e) => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32001, "message": format!("No se pudo obtener el estado: {}", e)}, "id": body["id"]})))
                        }
                    },
                    "submit_block" => {
                        let block: Block = serde_json::from_value(body["params"][0].clone()).unwrap();
                        let mut bc_lock = blockchain_rpc_clone.lock().unwrap();
                        match bc_lock.add_block(block.clone()) {
                            Ok(_) => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": "accepted", "id": body["id"]}))),
                            Err(e) => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32000, "message": format!("Rechazo de bloque: {}", e)}, "id": body["id"]})))
                        }
                    },
                    "get_utxos_by_address" => {
                         let address = body["params"][0].as_str().unwrap_or("").to_string();
                         let bc_lock = blockchain_rpc_clone.lock().unwrap();
                         let utxos: HashMap<String, _> = bc_lock.utxos.iter()
                            .filter(|(_, utxo)| utxo.address == address)
                            .map(|(key, utxo)| (key.clone(), utxo.clone()))
                            .collect();
                        Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": utxos, "id": body["id"]})))
                    },
                    "send_raw_transaction" => {
                        let tx: Transaction = serde_json::from_value(body["params"][0].clone()).unwrap();
                        let mut bc_lock = blockchain_rpc_clone.lock().unwrap();
                        match bc_lock.add_transaction(tx) {
                            Ok(_) => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": "transaction received", "id": body["id"]}))),
                            Err(e) => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32000, "message": format!("Error de transacción: {}", e)},"id": body["id"]})))
                        }
                    },
                    "get_chain_height" => {
                        let bc_lock = blockchain_rpc_clone.lock().unwrap();
                        let height = bc_lock.chain.len() as u64 - 1;
                        Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": height, "id": body["id"]})))
                    },
                    "get_block_by_height" => {
                        let params: GetBlockByHeightParams = serde_json::from_value(body["params"][0].clone()).unwrap();
                        let bc_lock = blockchain_rpc_clone.lock().unwrap();
                        if let Some(block) = bc_lock.chain.get(params.height as usize) {
                            Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": block, "id": body["id"]})))
                        } else {
                            Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32002, "message": "Bloque no encontrado"}, "id": body["id"]})))
                        }
                    },
                    "get_transaction_by_id" => {
                        let params: GetTransactionByIdParams = serde_json::from_value(body["params"][0].clone()).unwrap();
                        let bc_lock = blockchain_rpc_clone.lock().unwrap();
                        let mut found_tx = None;
                        for block in bc_lock.chain.iter().rev() {
                            if let Some(tx) = block.transactions.iter().find(|tx| tx.id == params.id) {
                                found_tx = Some(tx.clone());
                                break;
                            }
                        }
                        if let Some(tx) = found_tx {
                            Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": tx, "id": body["id"]})))
                        } else {
                             Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32003, "message": "Transacción no encontrada"}, "id": body["id"]})))
                        }
                    },
                    _ => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32601, "message": "Method not found"}, "id": body["id"]}))),
                }
            } else {
                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "Invalid Request"}, "id": null})))
            };
            
            reply.map_err(|_: warp::Rejection| warp::reject())
        }
    });

    let rpc_port = settings.get::<u16>("network.rpc_port")?;
    let rpc_server = warp::serve(rpc_route).run(([127, 0, 0, 1], rpc_port));
    info!("Servidor RPC (Modo Hydra) iniciado en http://127.0.0.1:{}", rpc_port);

    tokio::select! {
        _ = p2p_handle => warn!("La tarea P2P ha terminado."),
        _ = rpc_server => warn!("El servidor RPC ha terminado."),
    }

    Ok(())
}