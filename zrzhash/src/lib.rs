use sha3::{Digest, Sha3_256};
use blake3::Hasher as Blake3;

#[derive(Clone)] // HE AÑADIDO ESTA LÍNEA
pub struct ZRZHasher;

impl ZRZHasher {
    pub fn new() -> Self {
        ZRZHasher
    }
    
    pub fn hash(&self, data: &[u8], nonce: u64) -> [u8; 32] {
        let mut sha3 = Sha3_256::new();
        sha3.update(data);
        sha3.update(&nonce.to_le_bytes());
        let sha3_result = sha3.finalize();
        
        let mut blake3 = Blake3::new();
        blake3.update(&sha3_result);
        blake3.update(&nonce.to_le_bytes());
        
        *blake3.finalize().as_bytes()
    }
    
    pub fn verify(&self, data: &[u8], nonce: u64, target: &[u8; 32]) -> bool {
        let hash = self.hash(data, nonce);
        hash.iter().zip(target.iter()).all(|(h, t)| h <= t)
    }
}