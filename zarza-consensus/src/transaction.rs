use serde::{Serialize, Deserialize};
use chrono::Utc;

// Representa una referencia a una salida de una transacción anterior que ahora se usará como entrada.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TxInput {
    pub tx_id: String,      // ID de la transacción anterior
    pub output_index: u32,  // Índice de la salida en esa transacción
    pub signature: String,  // Firma que desbloquea esta UTXO
}

// Representa una nueva salida que se crea en una transacción. Esta es una futura UTXO.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TxOutput {
    pub address: String, // La dirección que puede gastar esta salida
    pub amount: f64,     // La cantidad de ZRZ
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
            id: String::new(), // El ID se calculará después
            inputs,
            outputs,
            timestamp: Utc::now().timestamp(),
        };
        tx.id = tx.calculate_hash(); // Calculamos y asignamos el ID
        tx
    }

    pub fn new_coinbase(recipient: &str, amount: f64) -> Self {
        let output = TxOutput { address: recipient.to_string(), amount };
        let mut tx = Transaction {
            id: String::new(),
            inputs: vec![], // Las transacciones coinbase no tienen entradas
            outputs: vec![output],
            timestamp: Utc::now().timestamp(),
        };
        tx.id = tx.calculate_hash();
        tx
    }

    /// Devuelve los datos de la transacción que necesitan ser firmados.
    pub fn to_bytes_for_signing(&self) -> Vec<u8> {
        let mut bytes = vec![];
        // Para la firma, usamos el timestamp y los datos de las salidas.
        bytes.extend(&self.timestamp.to_le_bytes());
        for output in &self.outputs {
            bytes.extend(output.address.as_bytes());
            bytes.extend(&output.amount.to_le_bytes());
        }
        bytes
    }
    
    /// Devuelve los bytes para calcular el ID único de la transacción.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = self.to_bytes_for_signing();
        // El ID sí incluye los datos de las entradas.
        for input in &self.inputs {
            bytes.extend(input.tx_id.as_bytes());
            bytes.extend(&input.output_index.to_le_bytes());
            bytes.extend(input.signature.as_bytes());
        }
        bytes
    }

    // Calcula el hash de la transacción para usarlo como ID.
    fn calculate_hash(&self) -> String {
        let bytes = self.to_bytes();
        let hash = blake3::hash(&bytes);
        hex::encode(hash.as_bytes())
    }

    // Comprueba si la transacción es una transacción coinbase (de recompensa por minado).
    pub fn is_coinbase(&self) -> bool {
        self.inputs.is_empty()
    }

    // Lógica de verificación de firma (aún simplificada, pero preparada).
    pub fn verify_signature(&self) -> bool {
        // En una implementación real, aquí se verificaría cada firma de cada entrada
        // usando la clave pública correspondiente.
        if self.is_coinbase() {
            return true;
        }
        !self.inputs.iter().any(|input| input.signature.is_empty())
    }
}