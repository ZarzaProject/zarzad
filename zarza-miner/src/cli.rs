use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct CliArgs {
    /// Zarza address for mining rewards
    #[arg(short, long)]
    pub address: String,

    /// Node RPC URL
    #[arg(short, long, default_value = "http://127.0.0.1:18445")]
    pub node: String,

    /// Mining threads
    #[arg(short, long, default_value_t = 4)]
    pub threads: usize,
}
