use std::error::Error;
use std::sync::{Arc, Mutex};
use log::{info, warn};
use config::Config;
use zarza_consensus::blockchain::Blockchain;
use zarza_consensus::transaction::{Transaction, TxOutput};
use zarza_network::p2p::P2PNode;
use warp::Filter;
use serde_json::json;
use zarza_consensus::block::Block;
use std::fs;
use std::path::Path;
use std::collections::HashMap;

#[derive(serde::Serialize)]
struct BlockchainState {
    last_block: Block,
    difficulty: f64, // Aseguramos que sea f64
}

fn save_blockchain(blockchain: &Blockchain) -> Result<(), Box<dyn Error>> {
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

fn load_blockchain(settings: &Config) -> Result<Blockchain, Box<dyn Error>> {
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();
    info!("Iniciando Zarza Daemon v{}", env!("CARGO_PKG_VERSION"));
    
    let settings = Config::builder().add_source(config::File::with_name("config/config")).build()?;
    
    let blockchain = Arc::new(Mutex::new(load_blockchain(&settings)?));
    
    let mempool = Arc::new(Mutex::new(Vec::<Transaction>::new()));

    let mut p2p_node = P2PNode::new(&settings).await?;
    let p2p_handle = tokio::spawn(async move {
        if let Err(e) = p2p_node.start().await { warn!("Error en el nodo P2P: {}", e); }
    });

    let blockchain_rpc = blockchain.clone();
    let mempool_rpc = mempool.clone();

    let rpc_route = warp::path("rpc").and(warp::post()).and(warp::body::json()).and_then(move |body: serde_json::Value| {
        let blockchain_rpc_clone = blockchain_rpc.clone();
        let mempool_rpc_clone = mempool_rpc.clone();

        async move {
            let method_name = body["method"].as_str().unwrap_or("unknown");
            info!("[RPC] Solicitud recibida -> {}", method_name);

            let reply = if let Some(method) = body["method"].as_str() {
                match method {
                    "get_blockchain_state" => {
                        let bc_lock = blockchain_rpc_clone.lock().unwrap();
                        match bc_lock.get_last_block() {
                            Ok(last_block) => {
                                let state = BlockchainState { last_block: last_block.clone(), difficulty: bc_lock.difficulty };
                                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": state, "id": body["id"]})))
                            },
                            Err(e) => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32001, "message": format!("No se pudo obtener el estado: {}", e)}, "id": body["id"]})))
                        }
                    },
                    "submit_block" => {
                        let block: Block = match serde_json::from_value(body["params"][0].clone()) {
                            Ok(b) => b,
                            Err(e) => return Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32700, "message": format!("Parse error: {}", e)}, "id": body["id"]}))),
                        };
                        
                        let mut bc_lock = blockchain_rpc_clone.lock().unwrap();
                        match bc_lock.add_block(block) {
                            Ok(_) => {
                                info!("[RPC] Bloque aceptado.");
                                mempool_rpc_clone.lock().unwrap().clear();
                                
                                let bc_to_save = bc_lock.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = save_blockchain(&bc_to_save) {
                                        warn!("¡FALLO CRÍTICO! No se pudo guardar la blockchain: {}", e);
                                    }
                                });
                                
                                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": "accepted", "id": body["id"]})))
                            },
                            Err(e) => {
                                warn!("[RPC] Bloque rechazado: {}", e);
                                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32000, "message": format!("Rechazo de bloque: {}", e)},"id": body["id"]})))
                            }
                        }
                    },
                    "get_utxos_by_address" => {
                        let address = body["params"][0].as_str().unwrap_or("");
                        let bc_lock = blockchain_rpc_clone.lock().unwrap();
                        let utxos: HashMap<String, TxOutput> = bc_lock.utxos.iter()
                            .filter(|(_, utxo)| utxo.address == address)
                            .map(|(key, utxo)| (key.clone(), utxo.clone()))
                            .collect();
                        Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": utxos, "id": body["id"]})))
                    },
                    "send_raw_transaction" => {
                        let tx: Transaction = match serde_json::from_value(body["params"][0].clone()) {
                            Ok(t) => t,
                            Err(e) => return Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32700, "message": format!("Error de parseo de tx: {}", e)}, "id": body["id"]}))),
                        };
                        info!("Recibida nueva transacción {} para el mempool", tx.id);
                        mempool_rpc_clone.lock().unwrap().push(tx);
                        Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": "transaction received", "id": body["id"]})))
                    },
                    _ => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32601, "message": "Method not found"}, "id": body["id"]})))
                }
            } else {
                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "Invalid Request"}, "id": null})))
            };
            
            // Esto es necesario para que el tipo de retorno coincida con lo que espera `and_then`
            reply.map_err(|e: warp::Rejection| e)
        }
    });

    let rpc_port = settings.get::<u16>("network.rpc_port")?;
    let rpc_server = warp::serve(rpc_route).run(([127, 0, 0, 1], rpc_port));
    info!("Servidor RPC iniciado en http://127.0.0.1:{}", rpc_port);

    tokio::select! {
        _ = p2p_handle => warn!("La tarea P2P ha terminado."),
        _ = rpc_server => warn!("El servidor RPC ha terminado."),
    }

    Ok(())
}
