use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use std::error::Error;
use config::Config;
use hex;
use thiserror::Error;
use crate::transaction::{Transaction, TxInput, TxOutput};
use std::collections::HashMap;

#[derive(Error, Debug)]
pub enum WalletError {
    #[error("La generación de claves falló")] KeyGenerationError,
    #[error("La clave fue rechazada")] KeyRejected,
    #[error("Clave privada inválida")] InvalidPrivateKey,
    #[error("Fondos insuficientes")] InsufficientFunds,
}

pub struct Wallet {
    key_pair: Ed25519KeyPair,
    pkcs8_bytes: Vec<u8>,
    address: String,
}

impl Wallet {
    pub fn new(_settings: &config::Config) -> Result<Self, Box<dyn Error>> {
        let rng = SystemRandom::new();
        let pkcs8_bytes = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| WalletError::KeyGenerationError)?.as_ref().to_vec();
            
        let key_pair = Ed25519KeyPair::from_pkcs8(&pkcs8_bytes)
            .map_err(|_| WalletError::KeyRejected)?;
        
        let address = Self::generate_address(key_pair.public_key().as_ref());
        
        Ok(Wallet { key_pair, pkcs8_bytes, address })
    }

    pub fn from_private_key(private_key: &str) -> Result<Self, Box<dyn Error>> {
        let pkcs8_bytes = hex::decode(private_key)
            .map_err(|_| WalletError::InvalidPrivateKey)?;
            
        let key_pair = Ed25519KeyPair::from_pkcs8(&pkcs8_bytes)
            .map_err(|_| WalletError::InvalidPrivateKey)?;
            
        let address = Self::generate_address(key_pair.public_key().as_ref());
        
        Ok(Wallet { key_pair, pkcs8_bytes, address })
    }

    pub fn create_transaction(
        &self,
        recipient: String,
        amount: f64,
        utxos: HashMap<String, TxOutput>,
    ) -> Result<Transaction, WalletError> {
        let mut balance = 0.0;
        let mut inputs_to_use = Vec::new();
        
        for (utxo_key, utxo) in utxos {
            if utxo.address == self.address {
                balance += utxo.amount;
                let key_parts: Vec<&str> = utxo_key.split(':').collect();
                inputs_to_use.push(TxInput {
                    tx_id: key_parts[0].to_string(),
                    output_index: key_parts[1].parse().unwrap(),
                    signature: String::new(),
                });
                if balance >= amount { break; }
            }
        }

        if balance < amount { return Err(WalletError::InsufficientFunds); }

        let mut outputs = vec![TxOutput { address: recipient, amount }];
        if balance > amount {
            outputs.push(TxOutput {
                address: self.address.clone(),
                amount: balance - amount,
            });
        }

        let mut tx = Transaction::new(inputs_to_use, outputs);
        self.sign_transaction(&mut tx);

        Ok(tx)
    }

    pub fn sign_transaction(&self, tx: &mut Transaction) {
        let data_to_sign = tx.to_bytes_for_signing();
        for input in &mut tx.inputs {
            let signature = self.key_pair.sign(&data_to_sign);
            input.signature = hex::encode(signature.as_ref());
        }
    }
    
    // --- FUNCIÓN CORREGIDA Y EN SU SITIO ---
    fn generate_address(public_key: &[u8]) -> String {
        format!("ZRZ{}", hex::encode(&public_key[..16]))
    }

    // --- FUNCIÓN CORREGIDA (sin 'static') ---
    pub fn verify_signature(public_key_bytes: &[u8], message: &[u8], signature_bytes: &[u8]) -> bool {
        let public_key = UnparsedPublicKey::new(&ED25519, public_key_bytes);
        public_key.verify(message, signature_bytes).is_ok()
    }

    pub fn get_address(&self) -> &str { &self.address }

    pub fn export_private_key(&self) -> String { hex::encode(&self.pkcs8_bytes) }
}