use serde::{Serialize, Deserialize};
use chrono::Utc;
use zrzhash::ZRZHasher;
use crate::wallet::Wallet; // Importamos Wallet para la verificación.
use log::warn;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TxInput {
    pub tx_id: String,
    pub output_index: u32,
    pub signature: String,
    pub pub_key: String, // Clave pública del firmante en formato hexadecimal
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TxOutput {
    pub address: String,
    pub amount: f64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Transaction {
    pub id: String,
    pub inputs: Vec<TxInput>,
    pub outputs: Vec<TxOutput>,
    pub timestamp: i64,
}

impl Transaction {
    pub fn new(inputs: Vec<TxInput>, outputs: Vec<TxOutput>) -> Self {
        let mut tx = Transaction {
            id: String::new(),
            inputs,
            outputs,
            timestamp: Utc::now().timestamp(),
        };
        tx.id = tx.calculate_hash();
        tx
    }

    pub fn new_coinbase(recipient: &str, amount: f64) -> Self {
        let output = TxOutput { address: recipient.to_string(), amount };
        let mut tx = Transaction {
            id: String::new(),
            inputs: vec![],
            outputs: vec![output],
            timestamp: Utc::now().timestamp(),
        };
        tx.id = tx.calculate_hash();
        tx
    }

    /// --- NUEVO MÉTODO CRÍTICO ---
    /// Serializa los datos de la transacción que deben ser firmados.
    /// No incluye las firmas en sí mismas para evitar recursividad.
    pub fn to_bytes_for_signing(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend(&self.timestamp.to_le_bytes());
        // Solo incluimos los IDs de las entradas, no las firmas.
        for input in &self.inputs {
            bytes.extend(input.tx_id.as_bytes());
            bytes.extend(&input.output_index.to_le_bytes());
        }
        for output in &self.outputs {
            bytes.extend(output.address.as_bytes());
            bytes.extend(&output.amount.to_le_bytes());
        }
        bytes
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.to_bytes_for_signing();
        // El hash final sí incluye las firmas para que sea único.
        for input in &self.inputs {
            bytes.extend(input.signature.as_bytes());
            bytes.extend(input.pub_key.as_bytes());
        }
        bytes
    }

    fn calculate_hash(&self) -> String {
        let bytes = self.to_bytes();
        let hasher = ZRZHasher::new();
        let hash_result = hasher.hash(&bytes, 0);
        hex::encode(hash_result)
    }

    pub fn is_coinbase(&self) -> bool {
        self.inputs.is_empty()
    }

    /// --- LÓGICA DE VERIFICACIÓN ACTUALIZADA ---
    pub fn verify_signature(&self) -> bool {
        if self.is_coinbase() {
            return true;
        }

        let data_to_verify = self.to_bytes_for_signing();
        for input in &self.inputs {
            let public_key_bytes = match hex::decode(&input.pub_key) {
                Ok(bytes) => bytes,
                Err(_) => return false,
            };
            let signature_bytes = match hex::decode(&input.signature) {
                Ok(bytes) => bytes,
                Err(_) => return false,
            };

            // Delegamos la verificación a la lógica de la cartera.
            if !Wallet::verify_signature(&public_key_bytes, &data_to_verify, &signature_bytes) {
                warn!("Firma inválida para la entrada {}:{}", input.tx_id, input.output_index);
                return false;
            }
        }
        true
    }
}