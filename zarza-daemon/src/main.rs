use std::error::Error;
use std::sync::{Arc, Mutex};
use log::{info, warn, error};
use config::Config;
use zarza_consensus::blockchain::{Blockchain, BlockchainError};
use zarza_consensus::transaction::{Transaction, TxOutput};
use zarza_network::p2p::P2PNode;
use warp::Filter;
use serde_json::json;
use zarza_consensus::block::Block;
use std::fs;
use std::path::Path;
use std::collections::HashMap;
use tokio::task;
use zarza_network::messages::NetworkMessage;

#[derive(serde::Serialize)]
struct BlockchainState {
    last_block: Block,
    difficulty: f64,
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    pretty_env_logger::init();
    info!("Iniciando Zarza Daemon v{}", env!("CARGO_PKG_VERSION"));
    
    let settings = Config::builder().add_source(config::File::with_name("config/config")).build()?;
    
    let blockchain = Arc::new(Mutex::new(load_blockchain(&settings)?));
    
    let mempool = Arc::new(Mutex::new(Vec::<Transaction>::new()));

    let p2p_node = Arc::new(P2PNode::new(&settings, blockchain.clone(), mempool.clone()).await?);
    let p2p_handle = task::spawn(p2p_node.clone().start());

    let p2p_broadcast_tx = p2p_node.get_broadcast_sender();

    let blockchain_rpc = blockchain.clone();
    let mempool_rpc = mempool.clone();

    let rpc_route = warp::path("rpc").and(warp::post()).and(warp::body::json()).and_then(move |body: serde_json::Value| {
        let blockchain_rpc_clone = blockchain_rpc.clone();
        let mempool_rpc_clone = mempool_rpc.clone();
        let p2p_tx_clone = p2p_broadcast_tx.clone();

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
                            Err(e) => {
                                error!("Error al parsear bloque enviado vía RPC: {}", e);
                                return Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32700, "message": format!("Parse error: {}", e)}, "id": body["id"]})));
                            },
                        };
                        
                        let mut bc_lock = blockchain_rpc_clone.lock().unwrap();
                        match bc_lock.add_block(block.clone()) {
                            Ok(_) => {
                                info!("[RPC] Bloque #{} aceptado y añadido.", block.index);
                                
                                let mut mempool_lock = mempool_rpc_clone.lock().unwrap();
                                let included_tx_ids: Vec<String> = block.transactions.iter().map(|tx| tx.id.clone()).collect();
                                mempool_lock.retain(|tx| !included_tx_ids.contains(&tx.id));
                                info!("[RPC] Mempool actualizado. Transacciones restantes: {}", mempool_lock.len());

                                if let Err(e) = p2p_tx_clone.send(NetworkMessage::NewBlock(block.clone())) {
                                    warn!("Error al difundir el nuevo bloque: {}", e);
                                }

                                let bc_to_save = bc_lock.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = save_blockchain(&bc_to_save) {
                                        warn!("¡FALLO CRÍTICO! No se pudo guardar la blockchain: {}", e);
                                    }
                                });
                                
                                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": "accepted", "id": body["id"]})))
                            },
                            Err(e) => {
                                warn!("[RPC] Bloque #{} rechazado: {}", block.index, e);
                                let error_message = match e {
                                    BlockchainError::InvalidBlock => "Bloque inválido".to_string(),
                                    BlockchainError::EmptyChain => "Cadena vacía".to_string(),
                                    BlockchainError::TransactionError(msg) => format!("Error de transacción en bloque: {}", msg),
                                    BlockchainError::BlockValidationError(msg) => format!("Error de validación de bloque: {}", msg),
                                    _ => format!("Error desconocido al añadir bloque: {}", e),
                                };
                                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32000, "message": format!("Rechazo de bloque: {}", error_message)},"id": body["id"]})))
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
                            Err(e) => {
                                error!("Error al parsear transacción enviada vía RPC: {}", e);
                                return Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32700, "message": format!("Parse error: {}", e)}, "id": body["id"]})));
                            },
                        };
                        
                        let mut bc_lock = blockchain_rpc_clone.lock().unwrap();
                        match bc_lock.add_transaction(tx.clone()) {
                            Ok(_) => {
                                info!("Recibida nueva transacción {} para el mempool", tx.id);
        
                                if let Err(e) = p2p_tx_clone.send(NetworkMessage::NewTransaction(tx)) {
                                    warn!("Error al difundir la nueva transacción: {}", e);
                                }
                                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": "transaction received", "id": body["id"]})))
                            },
                            Err(e) => {
                                warn!("Transacción {} rechazada por el nodo RPC: {}", tx.id, e);
                                let error_message = match e {
                                    BlockchainError::TransactionError(msg) => format!("Transacción inválida: {}", msg),
                                    _ => format!("Error desconocido al procesar transacción: {}", e),
                                };
                                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32000, "message": error_message},"id": body["id"]})))
                            }
                        }
                    },
                    "replace_chain" => {
                        let new_chain: Vec<Block> = match serde_json::from_value(body["params"][0].clone()) {
                            Ok(c) => c,
                            Err(e) => {
                                error!("Error al parsear cadena enviada vía RPC para reemplazar: {}", e);
                                return Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32700, "message": format!("Parse error: {}", e)}, "id": body["id"]})));
                            },
                        };
                        
                        let mut bc_lock = blockchain_rpc_clone.lock().unwrap();
                        if bc_lock.resolve_conflicts(new_chain) {
                            info!("[RPC] Cadena reemplazada con éxito.");
                            let bc_to_save = bc_lock.clone();
                            tokio::spawn(async move {
                                if let Err(e) = save_blockchain(&bc_to_save) {
                                    warn!("¡FALLO CRÍTICO! No se pudo guardar la blockchain después de reemplazar la cadena: {}", e);
                                }
                            });
                            Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": "chain replaced", "id": body["id"]})))
                        } else {
                            info!("[RPC] Cadena no reemplazada, la local es la más larga o la nueva no es válida.");
                            Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "result": "chain not replaced", "id": body["id"]})))
                        }
                    }
                    _ => Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32601, "message": "Method not found"}, "id": body["id"]})))
                }
            } else {
                Ok(warp::reply::json(&json!({"jsonrpc": "2.0", "error": {"code": -32600, "message": "Invalid Request"}, "id": null})))
            };
            
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