use std::error::Error;
use zarza_consensus::wallet::Wallet;
use clap::Parser;
use reqwest::blocking::Client;
use serde_json::json;

#[derive(Parser, Debug)]
#[command(name = "zarza-wallet")]
#[command(about = "Zarza Wallet CLI", long_about = None)]
enum Cli {
    /// Create a new wallet
    New,
    /// Check balance
    Balance {
        address: String
    },
    /// Send ZRZ to address
    Send {
        from: String,
        to: String,
        amount: f64,
        #[arg(short, long)]
        key: String
    }
}

pub fn process_commands(wallet: Wallet) -> Result<(), Box<dyn Error>> {
    let args = Cli::parse();

    match args {
        Cli::New => {
            println!("=== New Zarza Wallet ===");
            println!("Address: {}", wallet.get_address());
            println!("Private Key: {}", wallet.export_private_key());
            println!("=== IMPORTANT ===");
            println!("1. Backup your private key securely");
            println!("2. Never share it with anyone");
        },
        Cli::Balance { address } => {
            let client = Client::new();
            let response = client.post("http://localhost:18445")
                .json(&json!({
                    "jsonrpc": "2.0",
                    "method": "get_balance",
                    "params": [address],
                    "id": 1
                }))
                .send()?;

            let balance: f64 = response.json()?;
            println!("Balance for {}: {} ZRZ", address, balance);
        },
        Cli::Send { from, to, amount, key } => {
            unimplemented!()
        }
    }

    Ok(())
}
