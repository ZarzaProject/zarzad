use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use log::{info, error, warn};
use std::error::Error;
use futures::{SinkExt, StreamExt};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::task;
use std::collections::HashSet;
use tokio::sync::mpsc; // ¡NUEVO! Importamos mpsc

use super::messages::NetworkMessage;
use zarza_consensus::blockchain::Blockchain;
use zarza_consensus::block::Block;
use zarza_consensus::transaction::Transaction;

// Un canal de difusión para enviar mensajes a todos los pares
type BroadcastSender = broadcast::Sender<NetworkMessage>;

pub struct P2PNode {
    address: SocketAddr,
    peers: Arc<Mutex<Vec<SocketAddr>>>, // Estos son los pares actualmente conectados
    pub known_peers: Arc<Mutex<HashSet<SocketAddr>>>, // Pares conocidos para reconexión
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
        task::spawn(async move {
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
            task::spawn(async move {
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
        
        // ¡NUEVO! Canal mpsc para mensajes directos de read_task a write_task
        let (tx_direct, mut rx_direct) = mpsc::channel::<NetworkMessage>(32);

        // Tarea para leer mensajes del peer y procesarlos
        let read_task = task::spawn({
            let node_clone = self.clone();
            let tx_direct_clone = tx_direct; // Mover el sender del canal mpsc a esta tarea
            async move {
                while let Some(message_result) = receiver.next().await {
                    match message_result {
                        Ok(bytes) => {
                            let msg: NetworkMessage = serde_json::from_slice(&bytes)?;
                            info!("[{}] Received message: {:?}", addr, msg);
                            
                            match msg {
                                NetworkMessage::Ping => {
                                    // Enviamos Pong al canal mpsc para que write_task lo reenvíe directamente
                                    if let Err(e) = tx_direct_clone.send(NetworkMessage::Pong).await {
                                        error!("[{}] Error sending Pong to direct channel: {}", addr, e);
                                    }
                                },
                                NetworkMessage::NewBlock(block) => {
                                    let block_accepted = {
                                        let mut bc = node_clone.blockchain.lock().unwrap();
                                        bc.add_block(block.clone()).is_ok()
                                    };
                                    
                                    if block_accepted {
                                        let mut mempool = node_clone.mempool.lock().unwrap();
                                        let included_tx_ids: Vec<String> = block.transactions.iter().map(|tx| tx.id.clone()).collect();
                                        mempool.retain(|tx| !included_tx_ids.contains(&tx.id));
                                    } else {
                                        warn!("[{}] Invalid block received from peer.", addr);
                                    }
                                    
                                    if block_accepted {
                                        node_clone.broadcast_tx.send(NetworkMessage::NewBlock(block))?;
                                    }
                                },
                                NetworkMessage::NewTransaction(tx) => {
                                    let mut mempool = node_clone.mempool.lock().unwrap();
                                    if !mempool.iter().any(|t| t.id == tx.id) {
                                        // MEJORA: Validar la transacción antes de añadirla al mempool
                                        // if tx.verify_signature() && /* otras validaciones */ {
                                            mempool.push(tx.clone());
                                            node_clone.broadcast_tx.send(NetworkMessage::NewTransaction(tx))?;
                                        // } else {
                                        //     warn!("[{}] Invalid transaction received from peer.", addr);
                                        // }
                                    }
                                },
                                NetworkMessage::Pong => {
                                    info!("[{}] Received Pong from peer.", addr);
                                },
                                _ => {
                                    warn!("[{}] Received unhandled message type: {:?}", addr, msg);
                                }
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
        let write_task = task::spawn(async move {
            loop {
                tokio::select! {
                    // Escucha mensajes del canal de difusión
                    msg = broadcast_rx.recv() => {
                        match msg {
                            Ok(msg) => {
                                if matches!(msg, NetworkMessage::Pong) {
                                    continue; // No reenviar Pong recibidos de difusión
                                }
                                let bytes = serde_json::to_vec(&msg)?;
                                sender.send(bytes.into()).await?;
                            },
                            Err(e) => {
                                error!("Error receiving from broadcast channel: {}", e);
                                break; // Salir de la tarea si el canal de difusión falla
                            }
                        }
                    },
                    // Escucha mensajes del canal mpsc (respuestas directas como Pong)
                    msg = rx_direct.recv() => {
                        match msg {
                            Some(msg) => {
                                let bytes = serde_json::to_vec(&msg)?;
                                sender.send(bytes.into()).await?;
                            },
                            None => {
                                info!("Direct message channel closed.");
                                break; // Salir si el canal directo se cierra
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
        let stream = TcpStream::connect(addr).await?;
        info!("Conectado a peer {}", addr);
        
        let node_clone = self.clone();
        task::spawn(async move {
            if let Err(e) = node_clone.handle_peer_connection(stream).await {
                error!("Connection to {} closed with error: {}", addr, e);
            } else {
                info!("Connection to {} closed.", addr);
            }
        });

        Ok(())
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