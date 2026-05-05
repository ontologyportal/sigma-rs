// crates/core/src/persist/path_index.rs
//
// 18-byte path-index key encode/decode.
// Ported verbatim from sumo-store/src/path_index.rs.


#[cfg(feature = "cnf")]
/// Encode a `(pred_id, arg_pos, sym_id)` triple into an 18-byte path-index key.
pub(crate) fn encode_key(pred_id: u64, arg_pos: u16, sym_id: u64) -> [u8; 18] {
    let mut key = [0u8; 18];
    key[0..8].copy_from_slice(&pred_id.to_be_bytes());
    key[8..10].copy_from_slice(&arg_pos.to_be_bytes());
    key[10..18].copy_from_slice(&sym_id.to_be_bytes());
    key
}

#[cfg(feature = "cnf")]
#[cfg(test)]
mod tests {
    use super::*;

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
