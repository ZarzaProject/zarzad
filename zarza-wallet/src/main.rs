use std::error::Error;
use log::info;
use config::Config;
use zarza_consensus::wallet::Wallet;

mod cli;

fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();
    
    info!("Starting Zarza Wallet v{}", env!("CARGO_PKG_VERSION"));
    
    let settings = Config::builder()
        .add_source(config::File::with_name("config/config"))
        .build()?;
    
    let wallet = Wallet::new(&settings)?;
    cli::process_commands(wallet)?;
    
    Ok(())
}