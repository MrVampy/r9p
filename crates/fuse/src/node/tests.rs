use super::{mode_kind, qid_to_inode, DirEntry, NodeTable, ROOT_NODEID};
use r9p::qid::{Qid, DMSYMLINK};
use r9p::stat::Stat;

#[test]
fn inode_stays_under_signed_stat_boundary() {
    let inode = qid_to_inode(Qid::new(0x80, 0, u64::MAX));
    assert!(inode < (1_u64 << 63));
}

#[test]
fn symlink_stats_map_to_fuse_symlink_mode() {
    let stat = Stat::new(
        "link",
        Qid::new(r9p::qid::QTSYMLINK, 0, 7),
        DMSYMLINK | 0o777,
    );

    assert_eq!(
        libc::S_IFLNK | 0o777,
        mode_kind(&stat) | (stat.mode & 0o777)
    );
}

#[test]
fn lookup_nodes_remember_path_lineage() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    let alpha = nodes
        .insert_lookup(
            docs,
            3,
            Stat::new("alpha.md", Qid::file(3), 0o444),
            b"alpha.md",
        )
        .map(|inserted| inserted.nodeid)
        .expect("alpha node should insert");

    assert_eq!(nodes.node(docs).expect("docs").path, vec![b"docs".to_vec()]);
    assert_eq!(
        nodes.node(alpha).expect("alpha").path,
        vec![b"docs".to_vec(), b"alpha.md".to_vec()]
    );
}

#[test]
fn parent_nodeid_follows_path_lineage() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    let alpha = nodes
        .insert_lookup(
            docs,
            3,
            Stat::new("alpha.md", Qid::file(3), 0o444),
            b"alpha.md",
        )
        .map(|inserted| inserted.nodeid)
        .expect("alpha node should insert");

    assert_eq!(
        nodes.parent_nodeid(ROOT_NODEID).expect("root parent"),
        ROOT_NODEID
    );
    assert_eq!(nodes.parent_nodeid(docs).expect("docs parent"), ROOT_NODEID);
    assert_eq!(nodes.parent_nodeid(alpha).expect("alpha parent"), docs);
}

#[test]
fn lazy_lookup_nodes_keep_stat_without_a_fid_until_bound() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup_lazy(ROOT_NODEID, Stat::new("docs", Qid::dir(2), 0o555), b"docs")
        .expect("docs node should insert");

    let lazy = nodes.node(docs).expect("docs");
    assert_eq!(lazy.fid, None);
    assert_eq!(lazy.path, vec![b"docs".to_vec()]);
    assert_eq!(lazy.stat.qid, Qid::dir(2));

    let replaced = nodes
        .replace_binding(docs, 2, Stat::new("docs", Qid::dir(3), 0o555))
        .expect("docs node should bind");

    assert_eq!(replaced, None);
    let bound = nodes.node(docs).expect("docs");
    assert_eq!(bound.fid, Some(2));
    assert_eq!(bound.stat.qid, Qid::dir(3));
}

#[test]
fn replacing_binding_returns_superseded_fid() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");

    let replaced = nodes
        .replace_binding(docs, 3, Stat::new("docs", Qid::dir(3), 0o555))
        .expect("docs node should rebind");

    assert_eq!(replaced, Some(2));
    let rebound = nodes.node(docs).expect("docs");
    assert_eq!(rebound.fid, Some(3));
    assert_eq!(rebound.stat.qid, Qid::dir(3));
}

#[test]
fn rebinding_same_inode_keeps_generation_stable() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");

    let generation = nodes.node(docs).expect("docs").generation;
    let replaced = nodes
        .replace_binding(docs, 3, Stat::new("docs", Qid::dir(2), 0o555))
        .expect("docs node should rebind");

    assert_eq!(replaced, Some(2));
    assert_eq!(nodes.node(docs).expect("docs").generation, generation);
}

#[test]
fn rebinding_new_inode_bumps_generation() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");

    let generation = nodes.node(docs).expect("docs").generation;
    let _ = nodes
        .replace_binding(docs, 3, Stat::new("docs", Qid::dir(3), 0o555))
        .expect("docs node should rebind");

    assert!(nodes.node(docs).expect("docs").generation > generation);
}

#[test]
fn directory_entry_cache_survives_same_directory_stat_refresh() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");

    nodes
        .update_dir_cache(
            docs,
            vec![DirEntry {
                name: b"alpha.md".to_vec(),
                qid: Qid::file(3),
                stat: Stat::new("alpha.md", Qid::file(3), 0o444),
            }],
        )
        .expect("directory cache should update");
    nodes
        .update_stat(docs, Stat::new("docs", Qid::dir(2), 0o555))
        .expect("same directory stat should update");

    assert_eq!(
        nodes
            .node(docs)
            .expect("docs")
            .dir_cache
            .as_ref()
            .expect("cache should survive")
            .entries
            .len(),
        1
    );
}

#[test]
fn directory_entry_cache_clears_when_directory_identity_changes() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");

    nodes
        .update_dir_cache(docs, Vec::new())
        .expect("directory cache should update");
    nodes
        .update_stat(docs, Stat::new("docs", Qid::dir(7), 0o555))
        .expect("changed directory stat should update");

    assert!(nodes.node(docs).expect("docs").dir_cache.is_none());
}

#[test]
fn directory_entry_cache_clears_when_marked_stale() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");

    nodes
        .update_dir_cache(docs, Vec::new())
        .expect("directory cache should update");
    let stale = nodes.mark_path_stale(&[b"docs".to_vec()]);

    assert_eq!(stale.len(), 1);
    assert!(nodes.node(docs).expect("docs").dir_cache.is_none());
}

#[test]
fn forgetting_lazy_nodes_has_no_fid_to_clunk() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup_lazy(ROOT_NODEID, Stat::new("docs", Qid::dir(2), 0o555), b"docs")
        .expect("docs node should insert");

    assert_eq!(nodes.forget(docs, 1), None);
    assert!(nodes.node(docs).is_err());
}

#[test]
fn forget_returns_removed_fid_without_clunking_under_lock() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");

    assert_eq!(nodes.forget(docs, 1), Some(2));
    assert!(nodes.node(docs).is_err());
}

#[test]
fn lookup_reuses_path_and_discards_duplicate_fid() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let first = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .expect("first docs lookup should insert");
    let second = nodes
        .insert_lookup(
            ROOT_NODEID,
            3,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .expect("second docs lookup should reuse path");

    assert_eq!(second.nodeid, first.nodeid);
    assert_eq!(second.clunk_fid, Some(3));
    let docs = nodes.node(second.nodeid).expect("docs");
    assert_eq!(docs.fid, Some(2));
    assert_eq!(docs.lookups, 2);
}

#[test]
fn remove_path_subtree_drops_cached_descendants() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    let alpha = nodes
        .insert_lookup(
            docs,
            3,
            Stat::new("alpha.md", Qid::file(3), 0o444),
            b"alpha.md",
        )
        .map(|inserted| inserted.nodeid)
        .expect("alpha node should insert");

    let stale = nodes.remove_path_subtree(&[b"docs".to_vec()]);

    assert_eq!(stale, vec![2, 3]);
    assert!(nodes.node(docs).is_err());
    assert!(nodes.node(alpha).is_err());
    assert!(nodes.node(ROOT_NODEID).is_ok());
}

#[test]
fn move_path_prefix_moves_cached_descendants() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    let alpha = nodes
        .insert_lookup(
            docs,
            3,
            Stat::new("alpha.md", Qid::file(3), 0o444),
            b"alpha.md",
        )
        .map(|inserted| inserted.nodeid)
        .expect("alpha node should insert");

    nodes.move_path_prefix(&[b"docs".to_vec()], &[b"notes".to_vec()]);

    assert_eq!(
        nodes.node(docs).expect("docs").path,
        vec![b"notes".to_vec()]
    );
    assert_eq!(
        nodes.node(alpha).expect("alpha").path,
        vec![b"notes".to_vec(), b"alpha.md".to_vec()]
    );
}

#[test]
fn rebind_paths_snapshots_paths_without_network_work() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    let alpha = nodes
        .insert_lookup(
            docs,
            3,
            Stat::new("alpha.md", Qid::file(3), 0o444),
            b"alpha.md",
        )
        .map(|inserted| inserted.nodeid)
        .expect("alpha node should insert");

    let paths = nodes.rebind_paths();

    assert_eq!(
        paths,
        vec![
            (ROOT_NODEID, vec![]),
            (docs, vec![b"docs".to_vec()]),
            (alpha, vec![b"docs".to_vec(), b"alpha.md".to_vec()]),
        ]
    );
}

#[test]
fn apply_rebind_results_updates_fresh_nodes_and_marks_stale_nodes_for_lazy_rebind() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    let stale = nodes
        .insert_lookup(
            docs,
            3,
            Stat::new("stale.md", Qid::file(3), 0o444),
            b"stale.md",
        )
        .map(|inserted| inserted.nodeid)
        .expect("stale node should insert");

    let replaced = nodes.apply_rebind_results(
        vec![
            (ROOT_NODEID, 10, Stat::new("", Qid::dir(10), 0o555)),
            (docs, 11, Stat::new("docs", Qid::dir(11), 0o555)),
        ],
        vec![stale],
    );

    assert_eq!(replaced, vec![3, 1, 2]);
    assert_eq!(nodes.node(ROOT_NODEID).expect("root").fid, Some(10));
    assert_eq!(nodes.node(ROOT_NODEID).expect("root").qid, Qid::dir(10));
    assert_eq!(nodes.node(docs).expect("docs").fid, Some(11));
    assert_eq!(nodes.node(docs).expect("docs").qid, Qid::dir(11));
    let stale_node = nodes.node(stale).expect("stale node");
    assert_eq!(stale_node.fid, None);
    assert!(stale_node.needs_rebind);
}

#[test]
fn apply_rebind_results_keeps_generation_for_same_inode_refresh() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    let generation = nodes.node(docs).expect("docs").generation;

    let replaced = nodes.apply_rebind_results(
        vec![(docs, 3, Stat::new("docs", Qid::dir(2), 0o555))],
        vec![],
    );

    assert_eq!(replaced, vec![2]);
    assert_eq!(nodes.node(docs).expect("docs").generation, generation);
}

#[test]
fn targeted_stale_marking_leaves_unrelated_nodes_fresh() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let a = nodes
        .insert_lookup(ROOT_NODEID, 2, Stat::new("a", Qid::dir(2), 0o555), b"a")
        .map(|inserted| inserted.nodeid)
        .expect("a node should insert");
    let b = nodes
        .insert_lookup(ROOT_NODEID, 3, Stat::new("b", Qid::dir(3), 0o555), b"b")
        .map(|inserted| inserted.nodeid)
        .expect("b node should insert");
    let child = nodes
        .insert_lookup(a, 4, Stat::new("child", Qid::file(4), 0o444), b"child")
        .map(|inserted| inserted.nodeid)
        .expect("child node should insert");

    let stale = nodes.mark_path_prefix_stale(&[b"a".to_vec()]);

    assert_eq!(stale.len(), 2);
    assert!(nodes.node(a).expect("a").needs_rebind);
    assert!(nodes.node(child).expect("child").needs_rebind);
    assert!(!nodes.node(b).expect("b").needs_rebind);
    assert_eq!(
        nodes.parent_entry(&[b"a".to_vec(), b"child".to_vec()]),
        Some((a, b"child".to_vec()))
    );
}

#[test]
fn forget_decrements_lookup_count_before_removing() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    nodes.node_mut(docs).expect("docs").lookups = 2;

    assert_eq!(nodes.forget(docs, 1), None);
    assert_eq!(nodes.node(docs).expect("docs").lookups, 1);
    assert_eq!(nodes.forget(docs, 1), Some(2));
}

#[test]
fn namespace_mutation_marks_path_bindings_stale_without_network_rebind() {
    let mut nodes = NodeTable::new(1, Stat::new("", Qid::dir(1), 0o555));
    let docs = nodes
        .insert_lookup(
            ROOT_NODEID,
            2,
            Stat::new("docs", Qid::dir(2), 0o555),
            b"docs",
        )
        .map(|inserted| inserted.nodeid)
        .expect("docs node should insert");
    let alpha = nodes
        .insert_lookup(
            docs,
            3,
            Stat::new("alpha.md", Qid::file(3), 0o444),
            b"alpha.md",
        )
        .map(|inserted| inserted.nodeid)
        .expect("alpha node should insert");

    let stale = nodes.mark_path_bindings_stale();

    assert_eq!(
        stale.iter().map(|binding| binding.fid).collect::<Vec<_>>(),
        vec![Some(2), Some(3)]
    );
    assert_eq!(stale[0].nodeid, docs);
    assert_eq!(stale[0].parent_nodeid, Some(ROOT_NODEID));
    assert_eq!(stale[0].name, b"docs".to_vec());
    assert_eq!(stale[1].nodeid, alpha);
    assert_eq!(stale[1].parent_nodeid, Some(docs));
    assert_eq!(stale[1].name, b"alpha.md".to_vec());
    let root = nodes.node(ROOT_NODEID).expect("root");
    assert_eq!(root.fid, Some(1));
    assert!(!root.needs_rebind);
    let docs_node = nodes.node(docs).expect("docs");
    assert_eq!(docs_node.fid, None);
    assert!(docs_node.needs_rebind);
    let alpha_node = nodes.node(alpha).expect("alpha");
    assert_eq!(alpha_node.fid, None);
    assert!(alpha_node.needs_rebind);
}
