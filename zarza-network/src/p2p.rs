use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use log::{error, info, debug};
use rand::seq::SliceRandom;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use zarza_consensus::blockchain::Blockchain;
use super::messages::NetworkMessage;

#[derive(Clone, PartialEq, Debug)]
pub enum NodeStatus {
    Synchronizing,
    Synced,
}

type BroadcastSender = broadcast::Sender<NetworkMessage>;

pub struct P2PNode {
    address: SocketAddr,
    peers: Arc<Mutex<HashMap<SocketAddr, u64>>>,
    known_peers: Arc<Mutex<HashSet<SocketAddr>>>,
    blockchain: Arc<Mutex<Blockchain>>,
    broadcast_tx: BroadcastSender,
    status: Arc<Mutex<NodeStatus>>,
}

impl P2PNode {
    pub async fn new(
        settings: &config::Config,
        blockchain: Arc<Mutex<Blockchain>>,
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
            peers: Arc::new(Mutex::new(HashMap::new())),
            known_peers: Arc::new(Mutex::new(initial_known_peers)),
            blockchain,
            broadcast_tx,
            status: Arc::new(Mutex::new(NodeStatus::Synced)),
        })
    }

    pub async fn start(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Iniciando nodo P2P en {}", self.address);
        let listener = TcpListener::bind(self.address).await?;

        let sync_clone = self.clone();
        tokio::spawn(async move {
            sync_clone.sync_task().await;
        });

        loop {
            let (socket, addr) = listener.accept().await?;
            self.known_peers.lock().unwrap().insert(addr);
            let node_clone = self.clone();
            tokio::spawn(async move {
                if let Err(e) = node_clone.handle_peer_connection(socket).await {
                    error!("Conexión desde {} cerrada con error: {}", addr, e);
                }
            });
        }
    }

    async fn handle_peer_connection(self: Arc<Self>, socket: TcpStream) -> Result<(), Box<dyn Error + Send + Sync>> {
        let addr = socket.peer_addr()?;
        let (tx_direct, mut rx_direct) = mpsc::channel::<NetworkMessage>(32);
        let mut broadcast_rx = self.broadcast_tx.subscribe();
        
        // --- LÓGICA CONCURRENTE SEGURA ---
        let (our_height, peers_list) = {
            let bc_lock = self.blockchain.lock().unwrap();
            let height = bc_lock.chain.len() as u64 - 1;
            let peers = self.known_peers.lock().unwrap().iter().cloned().collect();
            (height, peers)
        };
        
        tx_direct.send(NetworkMessage::Height(our_height)).await?;
        tx_direct.send(NetworkMessage::GetPeers).await?;
        tx_direct.send(NetworkMessage::Peers(peers_list)).await?;

        let (mut sender, mut receiver) = Framed::new(socket, LengthDelimitedCodec::new()).split();
        
        loop {
            tokio::select! {
                Some(message_result) = receiver.next() => {
                    if let Ok(bytes) = message_result {
                        if let Ok(msg) = serde_json::from_slice::<NetworkMessage>(&bytes) {
                             match msg {
                                NetworkMessage::Height(peer_height) => {
                                    self.peers.lock().unwrap().insert(addr, peer_height);
                                },
                                NetworkMessage::GetPeers => {
                                    let peers = { self.known_peers.lock().unwrap().iter().cloned().collect() };
                                    tx_direct.send(NetworkMessage::Peers(peers)).await?;
                                },
                                NetworkMessage::Peers(peers) => {
                                    let mut known = self.known_peers.lock().unwrap();
                                    for peer in peers {
                                        if peer != self.address {
                                            known.insert(peer);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    } else {
                        break;
                    }
                },
                Some(msg_to_send) = rx_direct.recv() => {
                    let bytes = serde_json::to_vec(&msg_to_send)?;
                    sender.send(bytes.into()).await?;
                },
                Ok(broadcast_msg) = broadcast_rx.recv() => {
                    let bytes = serde_json::to_vec(&broadcast_msg)?;
                    sender.send(bytes.into()).await?;
                }
            }
        }
        self.peers.lock().unwrap().remove(&addr);
        Ok(())
    }
    
    async fn sync_task(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(10)).await;
            self.connect_to_new_peers().await;

            let (best_peer, max_height) = self.find_best_sync_peer();
            let our_height = { self.blockchain.lock().unwrap().chain.len() as u64 - 1 };

            if max_height > our_height {
                *self.status.lock().unwrap() = NodeStatus::Synchronizing;
                info!("Sincronización necesaria. Nuestra altura: {}, Red: {}. Sincronizando...", our_height, max_height);
                if let Some(peer_addr) = best_peer {
                    self.sync_with_peer(peer_addr).await;
                }
            } else {
                if *self.status.lock().unwrap() == NodeStatus::Synchronizing {
                    info!("Sincronización completada. Altura actual: {}", our_height);
                }
                *self.status.lock().unwrap() = NodeStatus::Synced;
            }
        }
    }

    async fn connect_to_new_peers(self: &Arc<Self>) {
        let peers_to_connect: Vec<SocketAddr> = {
            let known = self.known_peers.lock().unwrap();
            let active_peers_map = self.peers.lock().unwrap();
            known.iter()
                 .filter(|p| !active_peers_map.contains_key(p))
                 .cloned()
                 .collect()
        };

        for peer in peers_to_connect {
            if peer != self.address {
                let node_clone = self.clone();
                tokio::spawn(async move {
                    if let Ok(stream) = TcpStream::connect(peer).await {
                       let _ = node_clone.handle_peer_connection(stream).await;
                    }
                });
            }
        }
    }

    fn find_best_sync_peer(&self) -> (Option<SocketAddr>, u64) {
        let peers = self.peers.lock().unwrap();
        peers.iter()
            .max_by_key(|(_, &height)| height)
            .map(|(&addr, &height)| (Some(addr), height))
            .unwrap_or((None, 0))
    }

    async fn sync_with_peer(self: &Arc<Self>, peer_addr: SocketAddr) {
        let mut our_height = { self.blockchain.lock().unwrap().chain.len() as u64 };
        
        loop {
            if let Ok(stream) = TcpStream::connect(peer_addr).await {
                let (mut sender, mut receiver) = Framed::new(stream, LengthDelimitedCodec::new()).split();
                let get_blocks_msg = serde_json::to_vec(&NetworkMessage::GetBlocks(vec![our_height])).unwrap();
                
                if sender.send(get_blocks_msg.into()).await.is_err() { break; }

                if let Some(Ok(bytes)) = receiver.next().await {
                    if let Ok(NetworkMessage::Blocks(blocks)) = serde_json::from_slice(&bytes) {
                        if blocks.is_empty() { break; }
                        let num_blocks = blocks.len();
                        let mut bc = self.blockchain.lock().unwrap();
                        for block in blocks {
                            let _ = bc.add_block(block);
                        }
                        our_height += num_blocks as u64;
                    } else { break; }
                } else { break; }
            } else {
                error!("No se pudo conectar al peer de sincronización {}", peer_addr);
                break;
            }
        }
    }

    pub fn get_broadcast_sender(&self) -> BroadcastSender {
        self.broadcast_tx.clone()
    }
}