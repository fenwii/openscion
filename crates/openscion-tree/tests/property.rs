use openscion_tree::{Tree, TreeEntry};
use openscion_types::{FileMode, ObjectId, ObjectKind, RepoName};
use proptest::prelude::*;

proptest! {
    #[test]
    fn canonical_tree_roundtrip(names in proptest::collection::btree_set("[a-z][a-z0-9]{0,10}", 0..50)) {
        let entries = names.into_iter().map(|name| TreeEntry {
            name: RepoName::new(name.into_bytes()).unwrap(),
            mode: FileMode::Regular,
            id: ObjectId::hash(ObjectKind::Blob,b"payload"),
        }).collect();
        let tree=Tree::new(entries).unwrap();
        let bytes=tree.encode().unwrap();
        let decoded=Tree::decode(&bytes).unwrap();
        prop_assert_eq!(tree, decoded);
    }
    #[test]
    fn tree_digest_is_independent_of_input_order(names in proptest::collection::btree_set("[a-z][a-z0-9]{0,5}", 0..30)) {
        let entries: Vec<_> = names.into_iter().map(|name| TreeEntry {
            name: RepoName::new(name.into_bytes()).unwrap(),
            mode: FileMode::Regular,
            id: ObjectId::hash(ObjectKind::Blob,b"a"),
        }).collect();
        let mut backwards=entries.clone(); backwards.reverse();
        let a=Tree::new(entries).unwrap();
        let b=Tree::new(backwards).unwrap();
        prop_assert_eq!(a.id().unwrap(),b.id().unwrap());
    }
}
