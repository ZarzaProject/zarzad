//! # Zarza-Cerberus Hash Function v1.0 (Hybrid-Hardened Final Edition)
//!
//! El algoritmo de hash definitivo para el ecosistema Zarza.
//! Es un diseño híbrido que encadena la salida del algoritmo personalizado "Ouroboros"
//! como entrada para el estándar criptográfico BLAKE3.
//!
//! Flujo de Hashing: [DATOS] -> Ouroboros -> [HASH_INTERMEDIO] -> BLAKE3 -> [HASH_FINAL]

// --- Constantes y Lógica de Ouroboros (la primera cabeza de Cerberus) ---
const IV: [u64; 16] = [
    0x6a09e667f3bcc908, 0xbb67ae8584caa73b, 0x3c6ef372fe94f82b, 0xa54ff53a5f1d36f1,
    0x510e527fade682d1, 0x9b05688c2b3e6c1f, 0x1f83d9abfb41bd6b, 0x5be0cd19137e2179,
    0x428a2f98d728ae22, 0x7137449123ef65cd, 0xb5c0fbcfec4d3b2f, 0xe9b5dba58189dbbc,
    0x3956c25bf348b538, 0x59f111f1b605d019, 0x923f82a4af194f9b, 0xab1c5ed5da6d8118,
];

#[inline(always)]
fn g(state: &mut [u64; 16], a: usize, b: usize, c: usize, d: usize, rot1: u32, rot2: u32) {
    state[a] = state[a].wrapping_add(state[b]);
    state[d] = (state[d] ^ state[a]).rotate_right(rot1);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(rot2);
}

#[inline(always)]
fn ouroboros_permute(state: &mut [u64; 16]) {
    for _ in 0..16 {
        g(state, 0, 4, 8, 12, 32, 24); g(state, 1, 5, 9, 13, 16, 63);
        g(state, 2, 6, 10, 14, 32, 24); g(state, 3, 7, 11, 15, 16, 63);
        g(state, 0, 5, 10, 15, 32, 24); g(state, 1, 6, 11, 12, 16, 63);
        g(state, 2, 7, 8, 13, 32, 24); g(state, 3, 4, 9, 14, 16, 63);
    }
}

#[derive(Clone)]
pub struct ZRZHasher;

impl ZRZHasher {
    pub fn new() -> Self { ZRZHasher }

    // --- La Implementación Híbrida de Cerberus ---
    pub fn hash(&self, data: &[u8], nonce: u64) -> [u8; 32] {
        // --- CABEZA 1: OUROBOROS ---
        // Generamos el hash intermedio con nuestro algoritmo personalizado.
        
        let mut input = data.to_vec();
        input.extend_from_slice(&nonce.to_le_bytes());
        
        let mut state = IV;
        state[0] ^= u64::from_le_bytes(*b"ZARZAPOW");
        ouroboros_permute(&mut state);

        let mut padded_input = input;
        padded_input.push(0x80);
        while padded_input.len() % 32 != 0 {
            padded_input.push(0x00);
        }

        for chunk in padded_input.chunks_exact(32) {
            state[0] ^= u64::from_le_bytes(chunk[0..8].try_into().unwrap());
            state[1] ^= u64::from_le_bytes(chunk[8..16].try_into().unwrap());
            state[2] ^= u64::from_le_bytes(chunk[16..24].try_into().unwrap());
            state[3] ^= u64::from_le_bytes(chunk[24..32].try_into().unwrap());
            ouroboros_permute(&mut state);
        }

        for _ in 0..12 {
            ouroboros_permute(&mut state);
        }

        let mut ouroboros_output = [0u8; 32];
        ouroboros_output[0..8].copy_from_slice(&state[0].to_le_bytes());
        ouroboros_output[8..16].copy_from_slice(&state[1].to_le_bytes());
        ouroboros_output[16..24].copy_from_slice(&state[2].to_le_bytes());
        ouroboros_output[24..32].copy_from_slice(&state[3].to_le_bytes());

        // --- CABEZA 2: BLAKE3 ---
        // Usamos el resultado de Ouroboros como entrada para BLAKE3.
        // El resultado final es un hash BLAKE3 de un hash Ouroboros.
        let final_hash = blake3::hash(&ouroboros_output);

        *final_hash.as_bytes()
    }

    pub fn verify(&self, data: &[u8], nonce: u64, target: &[u8; 32]) -> bool {
        let hash = self.hash(data, nonce);
        hash.iter().zip(target.iter()).all(|(h, t)| h <= t)
    }
}