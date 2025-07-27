use serde::{Serialize, Deserialize};
use zarza_consensus::{block::Block, transaction::Transaction};
use std::net::SocketAddr;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NetworkMessage {
    Ping,
    Pong,
    NewBlock(Block),
    NewTransaction(Transaction),
    GetBlocks(Vec<u64>),
    Blocks(Vec<Block>),
    GetPeers,
    Peers(Vec<SocketAddr>),

    // --- NUEVOS MENSAJES PARA LA SINCRONIZACIÓN DE LA CADENA ---
    /// Pide a un nodo su altura actual de la blockchain.
    GetHeight,
    /// La respuesta a GetHeight, conteniendo la altura de la cadena del nodo.
    Height(u64),
}