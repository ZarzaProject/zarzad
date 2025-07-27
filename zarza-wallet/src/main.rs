use std::error::Error;
use log::info;
use zarza_consensus::wallet::Wallet;

mod cli;

fn main() -> Result<(), Box<dyn Error>> {
    pretty_env_logger::init();
    
    info!("Iniciando Zarza Wallet v{} (Modo Hydra - Seguridad Activada)", env!("CARGO_PKG_VERSION"));
    
    // Al instanciar la cartera, llamamos a `Wallet::new()` sin argumentos,
    // ya que la nueva implementación criptográfica no necesita el archivo de configuración.
    // Esta instancia se utiliza si el usuario ejecuta un comando que no sea `new`.
    let wallet = Wallet::new()?;
    
    if let Err(e) = cli::process_commands(wallet) {
        // Usamos eprintln para que los errores se muestren en la salida de error estándar,
        // lo cual es una mejor práctica para aplicaciones de línea de comandos.
        eprintln!("\nError: {}", e);
        std::process::exit(1);
    }
    
    Ok(())
}