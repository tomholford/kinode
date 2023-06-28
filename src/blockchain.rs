use rs_merkle::{MerkleTree, algorithms::Sha256, Hasher, MerkleProof};

/* ablablawaba blabwalalaba */

pub fn blockchain() -> Option<()> {
    let leaf_values = ["a", "b", "c", "d", "e", "f"];
    let leaves: Vec<[u8; 32]> = leaf_values
        .iter()
        .map(|x| Sha256::hash(x.as_bytes()))
        .collect();

    let merkle_tree = MerkleTree::<Sha256>::from_leaves(&leaves);
    let indices_to_prove = vec![3, 4];
    let leaves_to_prove = leaves.get(3..5).ok_or("can't get leaves to prove").ok()?;
    let merkle_proof = merkle_tree.proof(&indices_to_prove);
    let merkle_root = merkle_tree.root()?;
    // Serialize proof to pass it to the client
    let proof_bytes = merkle_proof.to_bytes();

    // Parse proof back on the client
    let proof = MerkleProof::<Sha256>::try_from(proof_bytes).ok()?;

    assert!(proof.verify(merkle_root, &indices_to_prove, leaves_to_prove, leaves.len()));

    Some(())
}
