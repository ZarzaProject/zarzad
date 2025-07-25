use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use std::error::Error;
use hex;
use thiserror::Error;
use crate::transaction::{Transaction, TxInput, TxOutput};
use std::collections::HashMap;
use sha2::{Sha256, Digest};

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
    pub_key_hex: String, // ¡NUEVO! Almacenar la clave pública en hexadecimal
}

impl Wallet {
    pub fn new(_settings: &config::Config) -> Result<Self, Box<dyn Error>> {
        let rng = SystemRandom::new();
        let pkcs8_bytes = Ed25519KeyPair::generate_pkcs8(&rng)
            .map_err(|_| WalletError::KeyGenerationError)?.as_ref().to_vec();
            
        let key_pair = Ed25519KeyPair::from_pkcs8(&pkcs8_bytes)
            .map_err(|_| WalletError::KeyRejected)?;
        
        let public_key_bytes = key_pair.public_key().as_ref();
        let address = Self::generate_address(public_key_bytes);
        let pub_key_hex = hex::encode(public_key_bytes); // Convertir a hexadecimal

        Ok(Wallet { key_pair, pkcs8_bytes, address, pub_key_hex })
    }

    pub fn from_private_key(private_key: &str) -> Result<Self, Box<dyn Error>> {
        let pkcs8_bytes = hex::decode(private_key)
            .map_err(|_| WalletError::InvalidPrivateKey)?;
            
        let key_pair = Ed25519KeyPair::from_pkcs8(&pkcs8_bytes)
            .map_err(|_| WalletError::InvalidPrivateKey)?;
            
        let public_key_bytes = key_pair.public_key().as_ref();
        let address = Self::generate_address(public_key_bytes);
        let pub_key_hex = hex::encode(public_key_bytes); // Convertir a hexadecimal
        
        Ok(Wallet { key_pair, pkcs8_bytes, address, pub_key_hex })
    }

    // MEJORA: La función `create_transaction` ahora tiene en cuenta las tarifas.
    pub fn create_transaction(
        &self,
        recipient: String,
        amount: f64,
        fee: f64, // MEJORA: Añadimos un parámetro para la tarifa de la transacción.
        utxos: HashMap<String, TxOutput>,
    ) -> Result<Transaction, WalletError> {
        let mut balance = 0.0;
        let mut inputs_to_use = Vec::new();
        
        // El total a cubrir es la cantidad + la tarifa.
        let total_needed = amount + fee;

        for (utxo_key, utxo) in utxos {
            if utxo.address == self.address {
                balance += utxo.amount;
                let key_parts: Vec<&str> = utxo_key.split(':').collect();
                inputs_to_use.push(TxInput {
                    tx_id: key_parts[0].to_string(),
                    output_index: key_parts[1].parse().unwrap(),
                    signature: String::new(),
                    pub_key: self.pub_key_hex.clone(), // ¡NUEVO! Incluimos la clave pública
                });
                // MEJORA: Rompemos el bucle cuando se cubren los fondos necesarios.
                if balance >= total_needed { break; }
            }
        }

        if balance < total_needed { return Err(WalletError::InsufficientFunds); }

        let mut outputs = vec![TxOutput { address: recipient, amount }];
        // Si hay cambio, se devuelve el resto a la dirección del remitente.
        if balance > total_needed {
            outputs.push(TxOutput {
                address: self.address.clone(),
                amount: balance - total_needed,
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
            // No necesitamos asignar input.pub_key aquí, ya se hizo en create_transaction
        }
    }
    
    // MEJORA: La dirección se crea a partir del hash SHA256 de la clave pública,
    // garantizando una longitud y seguridad adecuadas.
    fn generate_address(public_key: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(public_key);
        let result = hasher.finalize();
        format!("ZRZ{}", hex::encode(result))
    }

    pub fn verify_signature(public_key_bytes: &[u8], message: &[u8], signature_bytes: &[u8]) -> bool {
        let public_key = UnparsedPublicKey::new(&ED25519, public_key_bytes);
        public_key.verify(message, signature_bytes).is_ok()
    }

    pub fn get_address(&self) -> &str { &self.address }

    pub fn export_private_key(&self) -> String { hex::encode(&self.pkcs8_bytes) }

    pub fn get_public_key_hex(&self) -> &str { &self.pub_key_hex } // ¡NUEVO!
}