use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use log::{info, error, warn};
use std::error::Error;
use futures::{SinkExt, StreamExt};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;
use tokio::task;

use super::messages::NetworkMessage;
use zarza_consensus::blockchain::Blockchain;
use zarza_consensus::block::Block;
use zarza_consensus::transaction::Transaction;

// Un canal de difusión para enviar mensajes a todos los pares
type BroadcastSender = broadcast::Sender<NetworkMessage>;

pub struct P2PNode {
    address: SocketAddr,
    peers: Arc<Mutex<Vec<SocketAddr>>>,
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

        Ok(P2PNode {
            address,
            peers: Arc::new(Mutex::new(Vec::new())),
            blockchain,
            mempool,
            broadcast_tx,
        })
    }
    
    // Método de inicio para aceptar conexiones entrantes
    pub async fn start(self: Arc<Self>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        info!("Starting P2P node on {}", self.address);
        
        let listener = TcpListener::bind(self.address).await?;
        
        loop {
            let (socket, addr) = listener.accept().await?;
            info!("New connection from {}", addr);
            
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

    // Maneja una conexión única, con tareas de lectura y escritura separadas
    async fn handle_peer_connection(self: Arc<Self>, socket: TcpStream) -> Result<(), Box<dyn Error + Send + Sync>> {
        let addr = socket.peer_addr()?;
        
        // Clonamos la lista de peers y el broadcast sender para la tarea
        let peers_lock = self.peers.clone();
        let mut broadcast_rx = self.broadcast_tx.subscribe();

        let framed = Framed::new(socket, LengthDelimitedCodec::new());
        let (mut sender, mut receiver) = framed.split();
        
        // Tarea para leer mensajes del peer y procesarlos
        let read_task = task::spawn({
            let node_clone = self.clone();
            async move {
                while let Some(message_result) = receiver.next().await {
                    match message_result {
                        Ok(bytes) => {
                            let msg: NetworkMessage = serde_json::from_slice(&bytes)?;
                            info!("[{}] Received message: {:?}", addr, msg);
                            
                            match msg {
                                NetworkMessage::Ping => {
                                    node_clone.broadcast_tx.send(NetworkMessage::Pong)?;
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
                                    
                                    node_clone.broadcast_tx.send(NetworkMessage::NewBlock(block))?;
                                },
                                NetworkMessage::NewTransaction(tx) => {
                                    let mut mempool = node_clone.mempool.lock().unwrap();
                                    if !mempool.iter().any(|t| t.id == tx.id) {
                                        mempool.push(tx.clone());
                                    }
                                    node_clone.broadcast_tx.send(NetworkMessage::NewTransaction(tx))?;
                                },
                                _ => {}
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

        // Tarea para enviar mensajes al peer (escucha el canal de difusión)
        let write_task = task::spawn(async move {
            while let Ok(msg) = broadcast_rx.recv().await {
                let bytes = serde_json::to_vec(&msg)?;
                sender.send(bytes.into()).await?;
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
            info!("[{}] Peer removed. Total peers: {}", addr, peers.len());
        }
        
        Ok(())
    }
    
    // Conecta a un par y lo añade a la lista
    pub async fn connect_to_peer(self: Arc<Self>, addr: SocketAddr) -> Result<(), Box<dyn Error + Send + Sync>> {
        {
            let mut peers = self.peers.lock().unwrap();
            if peers.contains(&addr) {
                return Ok(());
            }
            peers.push(addr);
        }

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

    pub fn get_broadcast_sender(&self) -> BroadcastSender {
        self.broadcast_tx.clone()
    }
}