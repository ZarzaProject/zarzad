use thiserror::Error;
use crate::transaction::{Transaction, TxInput, TxOutput};
use std::collections::HashMap;
// Importaciones limpias y correctas
use ed25519_dalek::{Signer, Verifier, SigningKey, VerifyingKey, Signature, SECRET_KEY_LENGTH};
use rand::rngs::OsRng;
use sha2::{Sha256, Digest};

#[derive(Error, Debug)]
pub enum WalletError {
    #[error("Error al generar claves: {0}")]
    KeyGenerationError(String),
    #[error("Clave privada inválida")]
    InvalidPrivateKey,
    #[error("Fondos insuficientes")]
    InsufficientFunds,
    #[error("Error de firma: {0}")]
    SigningError(String),
}

pub struct Wallet {
    keypair: SigningKey,
    pub_key: VerifyingKey,
    address: String,
}

impl Wallet {
    pub fn new() -> Result<Self, WalletError> {
        let mut csprng = OsRng{};
        // --- CÓDIGO DE GENERACIÓN FINAL Y CORRECTO ---
        // Ahora que la feature 'rand_core' está activa, esta función existirá.
        let keypair = SigningKey::generate(&mut csprng);
        let pub_key = keypair.verifying_key();
        let address = Self::generate_address(pub_key.as_bytes());
        Ok(Wallet { keypair, pub_key, address })
    }

    pub fn from_private_key(private_key_hex: &str) -> Result<Self, WalletError> {
        let pk_bytes = hex::decode(private_key_hex).map_err(|_| WalletError::InvalidPrivateKey)?;
        let secret_key_array: [u8; SECRET_KEY_LENGTH] = pk_bytes.try_into().map_err(|_| WalletError::InvalidPrivateKey)?;
        let keypair = SigningKey::from_bytes(&secret_key_array);
        let pub_key = keypair.verifying_key();
        let address = Self::generate_address(pub_key.as_bytes());
        Ok(Wallet { keypair, pub_key, address })
    }

    fn generate_address(pub_key_bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(pub_key_bytes);
        let result = hasher.finalize();
        format!("ZRZ{}", hex::encode(result))
    }

    pub fn create_transaction(
        &self,
        recipient: String,
        amount: f64,
        fee: f64,
        utxos: HashMap<String, TxOutput>,
    ) -> Result<Transaction, WalletError> {
        let total_needed = amount + fee;
        let mut balance = 0.0;
        let mut inputs_to_use = Vec::new();

        for (utxo_key, utxo) in utxos {
            if utxo.address == self.address {
                balance += utxo.amount;
                let key_parts: Vec<&str> = utxo_key.split(':').collect();
                inputs_to_use.push(TxInput {
                    tx_id: key_parts[0].to_string(),
                    output_index: key_parts[1].parse().unwrap_or(0),
                    signature: String::new(),
                    pub_key: hex::encode(self.pub_key.as_bytes()),
                });
                if balance >= total_needed { break; }
            }
        }

        if balance < total_needed { return Err(WalletError::InsufficientFunds); }

        let mut outputs = vec![TxOutput { address: recipient, amount }];
        if balance > total_needed {
            outputs.push(TxOutput {
                address: self.address.clone(),
                amount: balance - total_needed,
            });
        }

        let mut tx = Transaction::new(inputs_to_use, outputs);
        self.sign_transaction(&mut tx)?;
        Ok(tx)
    }

    fn sign_transaction(&self, tx: &mut Transaction) -> Result<(), WalletError> {
        let data_to_sign = tx.to_bytes_for_signing();
        let signature = self.keypair.sign(&data_to_sign);
        
        for input in &mut tx.inputs {
            input.signature = hex::encode(signature.to_bytes());
        }
        Ok(())
    }
    
    pub fn verify_signature(public_key_bytes: &[u8], message: &[u8], signature_bytes: &[u8]) -> bool {
        let pub_key = match VerifyingKey::try_from(public_key_bytes) {
            Ok(pk) => pk,
            Err(_) => return false,
        };
        let signature = match Signature::try_from(signature_bytes) {
            Ok(sig) => sig,
            Err(_) => return false,
        };
        pub_key.verify(message, &signature).is_ok()
    }

    pub fn get_address(&self) -> &str { &self.address }

    pub fn export_private_key(&self) -> String { hex::encode(self.keypair.to_bytes()) }
    
    pub fn get_public_key_hex(&self) -> String { hex::encode(self.pub_key.as_bytes()) }
}