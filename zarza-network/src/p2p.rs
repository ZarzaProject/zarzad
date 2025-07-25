use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};
use log::{info, error};

pub struct P2PNode {
    address: SocketAddr,
    peers: Vec<SocketAddr>,
}

impl P2PNode {
    pub async fn new(settings: &config::Config) -> Result<Self, Box<dyn std::error::Error>> {
        let address: SocketAddr = format!("0.0.0.0:{}", settings.get::<u16>("network.port")?)
            .parse()?;
            
        Ok(P2PNode {
            address,
            peers: Vec::new(),
        })
    }
    
    pub async fn start(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        info!("Starting P2P node on {}", self.address);
        
        let listener = TcpListener::bind(self.address).await?;
        
        loop {
            let (socket, addr) = listener.accept().await?;
            info!("New connection from {}", addr);
            
            tokio::spawn(async move {
                if let Err(e) = Self::handle_connection(socket).await {
                    error!("Connection error: {}", e);
                }
            });
        }
    }
    
    async fn handle_connection(_socket: TcpStream) -> Result<(), Box<dyn std::error::Error>> {
        // Handle incoming messages
        Ok(())
    }
    
    pub async fn connect_to_peer(&mut self, addr: SocketAddr) -> Result<(), Box<dyn std::error::Error>> {
        let stream = TcpStream::connect(addr).await?;
        self.peers.push(addr);
        
        tokio::spawn(async move {
            if let Err(e) = Self::handle_connection(stream).await {
                error!("Connection error: {}", e);
            }
        });
        
        Ok(())
    }
}
