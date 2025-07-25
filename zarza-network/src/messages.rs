use serde::{Serialize, Deserialize};
use zarza_consensus::{block::Block, transaction::Transaction};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum NetworkMessage {
    Ping,
    Pong,
    NewBlock(Block),
    NewTransaction(Transaction),
    GetBlocks(Vec<u64>),
    Blocks(Vec<Block>),
}