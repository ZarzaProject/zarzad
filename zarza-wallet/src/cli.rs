use std::error::Error;
use zarza_consensus::wallet::{Wallet, WalletError};
use clap::{Parser, Subcommand};
use reqwest::blocking::Client;
use serde_json::json;
use std::collections::HashMap;
use zarza_consensus::transaction::{Transaction, TxOutput};
use zarza_consensus::block::Block;

#[derive(Parser, Debug)]
#[command(name = "zarza-wallet")]
#[command(about = "Zarza Wallet CLI (Modo Hydra - Seguridad Activada)", long_about = None)]
enum Cli {
    /// Crea una nueva cartera.
    New,
    /// Comprueba el balance de una dirección.
    Balance {
        address: String,
    },
    /// Envía ZRZ a una dirección.
    Send {
        #[arg(short, long)]
        from: String,
        #[arg(short, long)]
        to: String,
        amount: f64,
        #[arg(short, long)]
        key: String,
        #[arg(short, long, default_value_t = 0.001)]
        fee: f64,
    },
    /// Herramientas para explorar la blockchain.
    #[command(subcommand)]
    Explore(ExploreCommands),
}

#[derive(Subcommand, Debug)]
enum ExploreCommands {
    /// Muestra información general de la blockchain.
    Info,
    /// Busca y muestra un bloque por su altura.
    Block {
        height: u64,
    },
    /// Busca y muestra una transacción por su ID.
    Tx {
        id: String,
    },
}

pub fn process_commands(_wallet: Wallet) -> Result<(), Box<dyn Error>> {
    let args = Cli::parse();
    let client = Client::new();
    let node_url = "http://localhost:18445/rpc";

    match args {
        Cli::New => {
            let wallet = Wallet::new()?;
            println!("=== Nueva Cartera Zarza (Seguridad Activada) ===");
            println!("Dirección: {}", wallet.get_address());
            println!("Clave Privada (¡GUARDA ESTO EN SECRETO!): {}", wallet.export_private_key());
            println!("==================================================");
        },
        Cli::Balance { address } => {
            println!("Consultando balance para {}...", address);
            let response = client.post(node_url)
                .json(&json!({"jsonrpc": "2.0", "method": "get_utxos_by_address", "params": [address.clone()]}))
                .send()?;
            let rpc_response: serde_json::Value = response.json()?;
            if let Some(error) = rpc_response.get("error") {
                return Err(format!("Error del nodo: {}", error).into());
            }
            let utxos: HashMap<String, TxOutput> = serde_json::from_value(rpc_response["result"].clone())?;
            let balance: f64 = utxos.values().map(|utxo| utxo.amount).sum();
            println!("Balance de {}: {} ZRZ", address, balance);
        },
        Cli::Send { from, to, amount, key, fee } => {
            println!("Preparando transacción de {} ZRZ desde {} hacia {}...", amount, from, to);
            let sending_wallet = Wallet::from_private_key(&key)?;
            if sending_wallet.get_address() != from {
                return Err("La clave privada no corresponde a la dirección de origen.".into());
            }
            println!("Paso 1: Obteniendo UTXOs del nodo...");
            let response = client.post(node_url)
                .json(&json!({"jsonrpc": "2.0", "method": "get_utxos_by_address", "params": [from]}))
                .send()?;
            let utxos: HashMap<String, TxOutput> = serde_json::from_value(response.json::<serde_json::Value>()?["result"].clone())?;
            println!("Paso 2: Creando y firmando la transacción...");
            let tx = sending_wallet.create_transaction(to, amount, fee, utxos)?;
            println!("Paso 3: Enviando transacción a la red...");
            let response = client.post(node_url)
                .json(&json!({"jsonrpc": "2.0", "method": "send_raw_transaction", "params": [tx.clone()]}))
                .send()?;
            let rpc_response: serde_json::Value = response.json()?;
            if let Some(error) = rpc_response.get("error") {
                return Err(format!("El nodo rechazó la transacción: {}", error).into());
            }
            println!("¡Transacción enviada con éxito! ID: {}", tx.id);
        },
        // --- NUEVA LÓGICA PARA EL EXPLORADOR ---
        Cli::Explore(command) => {
            match command {
                ExploreCommands::Info => {
                    println!("--- Información de la Blockchain de Zarza ---");
                    let response = client.post(node_url)
                        .json(&json!({"jsonrpc": "2.0", "method": "get_chain_height", "id": 1}))
                        .send()?;
                    let height: u64 = serde_json::from_value(response.json::<serde_json::Value>()?["result"].clone())?;
                    println!("Altura de la Cadena: {}", height);
                },
                ExploreCommands::Block { height } => {
                    println!("--- Explorando Bloque #{} ---", height);
                    let response = client.post(node_url)
                        .json(&json!({"jsonrpc": "2.0", "method": "get_block_by_height", "params": [{"height": height}]}))
                        .send()?;
                    let block: Block = serde_json::from_value(response.json::<serde_json::Value>()?["result"].clone())?;
                    println!("{:#?}", block); // Usamos el pretty-print para mostrarlo bien
                },
                ExploreCommands::Tx { id } => {
                    println!("--- Explorando Transacción {} ---", id);
                     let response = client.post(node_url)
                        .json(&json!({"jsonrpc": "2.0", "method": "get_transaction_by_id", "params": [{"id": id}]}))
                        .send()?;
                    let tx: Transaction = serde_json::from_value(response.json::<serde_json::Value>()?["result"].clone())?;
                    println!("{:#?}", tx);
                },
            }
        }
    }

    Ok(())
}