use std::error::Error;
use zarza_consensus::wallet::Wallet;
use clap::Parser;
use reqwest::blocking::Client;
use serde_json::json;
use std::collections::HashMap;
use zarza_consensus::transaction::TxOutput;

#[derive(Parser, Debug)]
#[command(name = "zarza-wallet")]
#[command(about = "Zarza Wallet CLI", long_about = None)]
enum Cli {
    /// Crea una nueva cartera
    New,
    /// Comprueba el balance de una dirección
    Balance {
        address: String,
    },
    /// Envía ZRZ a una dirección
    Send {
        /// Dirección de origen (debe corresponder a la clave privada)
        from: String,
        /// Dirección de destino
        to: String,
        /// Cantidad a enviar
        amount: f64,
        /// Clave privada para firmar la transacción
        #[arg(short, long)]
        key: String,
    }
}

pub fn process_commands(wallet: Wallet) -> Result<(), Box<dyn Error>> {
    let args = Cli::parse();
    let client = Client::new();
    let node_url = "http://localhost:18445/rpc";

    match args {
        Cli::New => {
            println!("=== Nueva Cartera Zarza ===");
            println!("Dirección: {}", wallet.get_address());
            println!("Clave Privada: {}", wallet.export_private_key());
            println!("================================");
            println!("IMPORTANTE: Guarda tu clave privada en un lugar seguro. ¡Si la pierdes, pierdes tus fondos!");
        },
        Cli::Balance { address } => {
            // La lógica de balance ahora debería sumar las UTXOs
            println!("Consultando balance para {}...", address);
            let response = client.post(node_url)
                .json(&json!({"jsonrpc": "2.0", "method": "get_utxos_by_address", "params": [address]}))
                .send()?;
            
            let rpc_response: serde_json::Value = response.json()?;
            let utxos: HashMap<String, TxOutput> = serde_json::from_value(rpc_response["result"].clone())?;

            let balance: f64 = utxos.values().map(|utxo| utxo.amount).sum();
            println!("Balance de {}: {} ZRZ", address, balance);
        },
        // --- ¡COMANDO 'SEND' IMPLEMENTADO! ---
        Cli::Send { from, to, amount, key } => {
            println!("Preparando transacción de {} ZRZ desde {} hacia {}...", amount, from, to);

            // 1. Crear la cartera desde la clave privada
            let sending_wallet = Wallet::from_private_key(&key)?;
            if sending_wallet.get_address() != from {
                return Err("La clave privada no corresponde a la dirección de origen.".into());
            }

            // 2. Pedir las UTXOs disponibles al daemon
            println!("Paso 1: Obteniendo UTXOs del nodo...");
            let response = client.post(node_url)
                .json(&json!({"jsonrpc": "2.0", "method": "get_utxos_by_address", "params": [from]}))
                .send()?;
            let rpc_response: serde_json::Value = response.json()?;
            let utxos: HashMap<String, TxOutput> = serde_json::from_value(rpc_response["result"].clone())?;

            // 3. Crear y firmar la transacción
            println!("Paso 2: Creando y firmando la transacción...");
            let tx = sending_wallet.create_transaction(to, amount, utxos)?;
            
            // 4. Enviar la transacción al mempool del daemon
            println!("Paso 3: Enviando transacción a la red...");
            let response = client.post(node_url)
                .json(&json!({"jsonrpc": "2.0", "method": "send_raw_transaction", "params": [tx]}))
                .send()?;
            
            println!("¡Transacción enviada con éxito a la red! ID de la Transacción: {}", tx.id);
            println!("Será confirmada cuando se mine el próximo bloque.");
        }
    }

    Ok(())
}