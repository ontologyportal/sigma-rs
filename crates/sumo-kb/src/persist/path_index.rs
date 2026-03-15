// crates/sumo-kb/src/persist/path_index.rs
//
// 18-byte path-index key encode/decode.
// Ported verbatim from sumo-store/src/path_index.rs.

/// Encode a `(pred_id, arg_pos, sym_id)` triple into an 18-byte path-index key.
pub(crate) fn encode_key(pred_id: u64, arg_pos: u16, sym_id: u64) -> [u8; 18] {
    let mut key = [0u8; 18];
    key[0..8].copy_from_slice(&pred_id.to_be_bytes());
    key[8..10].copy_from_slice(&arg_pos.to_be_bytes());
    key[10..18].copy_from_slice(&sym_id.to_be_bytes());
    key
}

/// Decode an 18-byte path-index key into `(pred_id, arg_pos, sym_id)`.
#[allow(dead_code)]
pub(crate) fn decode_key(key: &[u8; 18]) -> (u64, u16, u64) {
    let pred_id = u64::from_be_bytes(key[0..8].try_into().unwrap());
    let arg_pos = u16::from_be_bytes(key[8..10].try_into().unwrap());
    let sym_id  = u64::from_be_bytes(key[10..18].try_into().unwrap());
    (pred_id, arg_pos, sym_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let (p, a, s) = (0xDEAD_BEEF_0000_1234u64, 3u16, 0x0000_0042u64);
        let key = encode_key(p, a, s);
        let (p2, a2, s2) = decode_key(&key);
        assert_eq!((p, a, s), (p2, a2, s2));
    }

    #[test]
    fn ordering() {
        let k1 = encode_key(1, 0, 100);
        let k2 = encode_key(1, 0, 200);
        let k3 = encode_key(1, 1, 0);
        let k4 = encode_key(2, 0, 0);
        assert!(k1 < k2);
        assert!(k2 < k3);
        assert!(k3 < k4);
    }
}
