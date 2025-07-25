use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use log::{info, error, warn, debug};
use std::error::Error;
use futures::{SinkExt, StreamExt};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::task;
use std::collections::HashSet;
use tokio::sync::mpsc;

use super::messages::NetworkMessage;
use zarza_consensus::blockchain::Blockchain; // Se eliminó BlockchainError ya que no se usa directamente aquí
use zarza_consensus::block::Block;
use zarza_consensus::transaction::Transaction;

type BroadcastSender = broadcast::Sender<NetworkMessage>;

pub struct P2PNode {
    address: SocketAddr,
    peers: Arc<Mutex<Vec<SocketAddr>>>,
    pub known_peers: Arc<Mutex<HashSet<SocketAddr>>>,
    blockchain: Arc<Mutex<Blockchain>>,
    mempool: Arc<Mutex<Vec<Transaction>>>,
    broadcast_tx: BroadcastSender,
}

impl P2PNode {
    pub async fn new(
        settings: &config::Config,
        blockchain: Arc<Mutex<Blockchain>>,
        mempool: Arc<Mutex<Vec<Transaction>>>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let address: SocketAddr = format!("0.0.0.0:{}", settings.get::<u16>("network.port")?)
            .parse()?;
        
        let (broadcast_tx, _) = broadcast::channel(100);

        let seed_peers: Vec<SocketAddr> = settings.get::<Vec<String>>("network.seed_peers")
            .unwrap_or_else(|_| Vec::new())
            .into_iter()
            .filter_map(|s| s.parse().ok())
            .collect();
        
        let mut initial_known_peers = HashSet::new();
        for peer in seed_peers {
            initial_known_peers.insert(peer);
        }

        Ok(P2PNode {
            address,
            peers: Arc::new(Mutex::new(Vec::new())),
            known_peers: Arc::new(Mutex::new(initial_known_peers)),
            blockchain,
            mempool,
            broadcast_tx,
        })
    }
    
    pub async fn start(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting P2P node on {}", self.address);
        
        let listener = TcpListener::bind(self.address).await?;

        let node_clone = self.clone();
        tokio::spawn(async move {
            node_clone.reconnection_task().await;
        });
        
        loop {
            let (socket, addr) = listener.accept().await?;
            info!("New connection from {}", addr);
            
            {
                let mut known_peers = self.known_peers.lock().unwrap();
                known_peers.insert(addr);
            }

            let node_clone = self.clone();
            tokio::spawn(async move {
                if let Err(e) = node_clone.handle_peer_connection(socket).await {
                    error!("Connection from {} closed with error: {}", addr, e);
                } else {
                    info!("Connection from {} closed.", addr);
                }
            });
        }
    }

    async fn handle_peer_connection(self: Arc<Self>, socket: TcpStream) -> Result<(), Box<dyn Error + Send + Sync>> {
        let addr = socket.peer_addr()?;
        
        {
            let mut peers = self.peers.lock().unwrap();
            if !peers.contains(&addr) {
                peers.push(addr);
                info!("[{}] Peer added to active list. Total peers: {}", addr, peers.len());
            }
        }

        let peers_lock = self.peers.clone();
        let mut broadcast_rx = self.broadcast_tx.subscribe();

        let framed = Framed::new(socket, LengthDelimitedCodec::new());
        let (mut sender, mut receiver) = framed.split();
        
        let (tx_direct, mut rx_direct) = mpsc::channel::<NetworkMessage>(32);

        // --- Sincronización inicial de la cadena ---
        let local_chain_len = self.blockchain.lock().unwrap().chain.len() as u64;
        debug!("[{}] Solicitando bloques del peer desde el índice {}.", addr, local_chain_len);
        if let Err(e) = tx_direct.send(NetworkMessage::GetBlocks(vec![local_chain_len])).await {
            error!("[{}] Error al solicitar bloques al peer: {}", addr, e);
        }
        // --- Fin Sincronización inicial ---

        // Tarea para leer mensajes del peer y procesarlos
        let read_task = tokio::spawn({
            let node_clone = self.clone();
            let tx_direct_clone = tx_direct;
            async move {
                while let Some(message_result) = receiver.next().await {
                    match message_result {
                        Ok(bytes) => {
                            let msg: NetworkMessage = match serde_json::from_slice(&bytes) {
                                Ok(m) => m,
                                Err(e) => {
                                    error!("[{}] Error al deserializar mensaje: {}", addr, e);
                                    continue;
                                }
                            };
                            info!("[{}] Received message: {:?}", addr, msg);
                            
                            match msg {
                                NetworkMessage::Ping => {
                                    if let Err(e) = tx_direct_clone.send(NetworkMessage::Pong).await {
                                        error!("[{}] Error sending Pong to direct channel: {}", addr, e);
                                    }
                                },
                                NetworkMessage::Pong => {
                                    info!("[{}] Received Pong from peer.", addr);
                                },
                                NetworkMessage::NewBlock(block) => {
                                    let add_block_result = { // Ámbito para liberar el bloqueo antes del await
                                        let mut bc = node_clone.blockchain.lock().unwrap();
                                        bc.add_block(block.clone())
                                    };
                                    
                                    match add_block_result {
                                        Ok(_) => {
                                            info!("[{}] Bloque #{} aceptado del peer.", addr, block.index);
                                            let mut mempool = node_clone.mempool.lock().unwrap(); // 'mut' es necesario aquí debido a .retain()
                                            let included_tx_ids: Vec<String> = block.transactions.iter().map(|tx| tx.id.clone()).collect();
                                            mempool.retain(|tx| !included_tx_ids.contains(&tx.id));
                                            
                                            if let Err(e) = node_clone.broadcast_tx.send(NetworkMessage::NewBlock(block)) {
                                                warn!("[{}] Error al difundir el nuevo bloque: {}", addr, e);
                                            }
                                        },
                                        Err(e) => {
                                            warn!("[{}] Bloque #{} recibido de peer rechazado: {}", addr, block.index, e);
                                            let local_chain_len = node_clone.blockchain.lock().unwrap().chain.len() as u64; // Se adquiere el bloqueo de nuevo aquí para obtener la longitud
                                            warn!("[{}] Nuestro bloque más reciente es #{} . Solicitando cadena a peer para posible conflicto...", addr, local_chain_len.saturating_sub(1));
                                            if let Err(e) = tx_direct_clone.send(NetworkMessage::GetBlocks(vec![0])).await {
                                                error!("[{}] Error al solicitar cadena completa al peer: {}", addr, e);
                                            }
                                        }
                                    }
                                },
                                NetworkMessage::NewTransaction(tx) => {
                                    let add_tx_result = { // Ámbito para liberar el bloqueo antes del await
                                        let mut bc = node_clone.blockchain.lock().unwrap();
                                        bc.add_transaction(tx.clone())
                                    };
                                    
                                    let mut mempool = node_clone.mempool.lock().unwrap(); // 'mut' es necesario aquí debido a .retain()
                                    if !mempool.iter().any(|t| t.id == tx.id) {
                                        if let Err(e) = add_tx_result {
                                            warn!("[{}] Transacción {} recibida de peer rechazada: {}", addr, tx.id, e);
                                        } else {
                                            // Si add_transaction tuvo éxito, ya añadió la transacción al mempool interno de la blockchain
                                            if let Err(e) = node_clone.broadcast_tx.send(NetworkMessage::NewTransaction(tx)) {
                                                warn!("[{}] Error al difundir la nueva transacción: {}", addr, e);
                                            }
                                        }
                                    } else {
                                        debug!("[{}] Transacción {} ya en mempool, no se añade.", addr, tx.id);
                                    }
                                },
                                NetworkMessage::GetBlocks(indices) => {
                                    debug!("[{}] Recibida solicitud de bloques para índices: {:?}", addr, indices);
                                    let blocks_to_send_result = { // Ámbito para liberar el bloqueo antes del await
                                        let bc = node_clone.blockchain.lock().unwrap();
                                        let mut blocks_to_send: Vec<Block> = Vec::new();

                                        if let Some(start_index) = indices.get(0) {
                                            let mut current_idx = *start_index as usize;
                                            while let Some(block) = bc.chain.get(current_idx) {
                                                blocks_to_send.push(block.clone());
                                                current_idx += 1;
                                                if blocks_to_send.len() >= 100 { break; }
                                            }
                                        }
                                        blocks_to_send
                                    };
                                    
                                    if !blocks_to_send_result.is_empty() {
                                        info!("[{}] Enviando {} bloques al peer, desde el índice {}.", addr, blocks_to_send_result.len(), blocks_to_send_result[0].index);
                                        if let Err(e) = tx_direct_clone.send(NetworkMessage::Blocks(blocks_to_send_result)).await {
                                            error!("[{}] Error al enviar bloques al peer: {}", addr, e);
                                        }
                                    } else {
                                        debug!("[{}] No hay bloques para enviar al peer a partir del índice solicitado.", addr);
                                    }
                                },
                                NetworkMessage::Blocks(blocks) => {
                                    if blocks.is_empty() {
                                        debug!("[{}] Recibido mensaje Blocks vacío.", addr);
                                        continue;
                                    }
                                    info!("[{}] Recibidos {} bloques del peer, desde el índice {}.", addr, blocks.len(), blocks[0].index);
                                    
                                    let local_last_block_index = node_clone.blockchain.lock().unwrap().get_last_block().map(|b| b.index).unwrap_or(0);
                                    let received_first_block_index = blocks[0].index;

                                    if received_first_block_index == local_last_block_index + 1 {
                                        for block in blocks {
                                            let add_block_result = { // Ámbito para liberar el bloqueo
                                                let mut bc = node_clone.blockchain.lock().unwrap();
                                                bc.add_block(block.clone())
                                            };

                                            match add_block_result {
                                                Ok(_) => {
                                                    info!("[{}] Bloque {} añadido durante la sincronización.", addr, block.index);
                                                    if let Err(e) = node_clone.broadcast_tx.send(NetworkMessage::NewBlock(block.clone())) {
                                                        warn!("[{}] Error al difundir bloque sincronizado: {}", addr, e);
                                                    }
                                                },
                                                Err(e) => {
                                                    warn!("[{}] Fallo al añadir bloque {} durante la sincronización: {}. Se intenta resolver conflicto.", addr, block.index, e);
                                                    if let Err(e) = tx_direct_clone.send(NetworkMessage::GetBlocks(vec![0])).await {
                                                        error!("[{}] Error al solicitar cadena completa para resolución de conflicto: {}", addr, e);
                                                    }
                                                    break;
                                                }
                                            }
                                        }
                                    } else if received_first_block_index == 0 && blocks.last().unwrap().index > local_last_block_index {
                                        let resolve_conflicts_result = { // Ámbito para liberar el bloqueo
                                            let mut bc = node_clone.blockchain.lock().unwrap();
                                            bc.resolve_conflicts(blocks)
                                        };
                                        
                                        if resolve_conflicts_result {
                                            info!("[{}] Cadena reemplazada con éxito después de recibir bloques completos.", addr);
                                        } else {
                                            info!("[{}] Cadena recibida no reemplazó la nuestra (no es más larga o es inválida).", addr);
                                        }
                                    } else {
                                        warn!("[{}] Recibido mensaje Blocks con índice de inicio inesperado {}. Ignorando.", addr, received_first_block_index);
                                    }
                                },
                                // Se eliminó el patrón `_` inalcanzable.
                            }
                        },
                        Err(e) => {
                            error!("[{}] Error reading from socket: {}", addr, e);
                            return Err(e.into());
                        }
                    }
                }
                Ok(()) as Result<(), Box<dyn Error + Send + Sync>>
            }
        });

        // Tarea para enviar mensajes al peer (escucha el canal de difusión y el canal mpsc)
        let write_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    msg = broadcast_rx.recv() => {
                        match msg {
                            Ok(msg) => {
                                if matches!(msg, NetworkMessage::Pong) {
                                    continue; 
                                }
                                let bytes = serde_json::to_vec(&msg)?;
                                sender.send(bytes.into()).await?;
                            },
                            Err(e) => {
                                error!("Error receiving from broadcast channel: {}", e);
                                break;
                            }
                        }
                    },
                    msg = rx_direct.recv() => {
                        match msg {
                            Some(msg) => {
                                debug!("[{}] Enviando mensaje directo: {:?}", addr, msg);
                                let bytes = serde_json::to_vec(&msg)?;
                                sender.send(bytes.into()).await?;
                            },
                            None => {
                                info!("Direct message channel closed.");
                                break;
                            }
                        }
                    },
                }
            }
            Ok(()) as Result<(), Box<dyn Error + Send + Sync>>
        });

        tokio::select! {
            _ = read_task => info!("[{}] Read task finished.", addr),
            _ = write_task => info!("[{}] Write task finished.", addr),
        }
        
        {
            let mut peers = peers_lock.lock().unwrap();
            peers.retain(|p_addr| p_addr != &addr);
            info!("[{}] Peer removed from active list. Total active peers: {}", addr, peers.len());
        }
        
        Ok(())
    }
    
    pub async fn connect_to_peer(self: Arc<Self>, addr: SocketAddr) -> Result<(), Box<dyn Error + Send + Sync>> {
        {
            let mut known_peers = self.known_peers.lock().unwrap();
            known_peers.insert(addr);
        }
        
        {
            let peers = self.peers.lock().unwrap();
            if peers.contains(&addr) {
                info!("Ya conectado a peer {}. No se intenta reconexión.", addr);
                return Ok(());
            }
        }

        info!("Intentando conectar a peer {}...", addr);
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                info!("Conectado a peer {}", addr);
                let node_clone = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = node_clone.handle_peer_connection(stream).await {
                        error!("Connection to {} closed with error: {}", addr, e);
                    } else {
                        info!("Connection to {} closed.", addr);
                    }
                });
                Ok(())
            },
            Err(e) => {
                warn!("Fallo al conectar a {}: {}", addr, e);
                Err(e.into())
            }
        }
    }

    async fn reconnection_task(self: Arc<Self>) {
        let reconnect_interval = tokio::time::Duration::from_secs(10);
        loop {
            tokio::time::sleep(reconnect_interval).await;
            
            let current_active_peers: HashSet<SocketAddr> = {
                self.peers.lock().unwrap().iter().cloned().collect()
            };

            let peers_to_reconnect: Vec<SocketAddr> = {
                let known_peers = self.known_peers.lock().unwrap();
                known_peers.iter()
                           .filter(|&addr| !current_active_peers.contains(addr))
                           .cloned()
                           .collect()
            };

            for peer_addr in peers_to_reconnect {
                info!("Intentando reconectar a peer conocido: {}", peer_addr);
                let node_clone = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = node_clone.connect_to_peer(peer_addr).await {
                        warn!("Fallo al reconectar a {}: {}", peer_addr, e);
                    }
                });
            }
        }
    }

    pub fn get_broadcast_sender(&self) -> BroadcastSender {
        self.broadcast_tx.clone()
    }
}