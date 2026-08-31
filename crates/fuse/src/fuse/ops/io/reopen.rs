use crate::{
    error::{Error, Result},
    fuse::R9pFuse,
};
use r9p::{fid::Fid, qid::Qid, stat::Stat};
use session::{read_open_directory_entries, Client, DirEntry, OREAD};

impl R9pFuse {
    pub(super) fn reopen_read_binding(&mut self, nodeid: u64) -> Result<(Client, Fid, Stat)> {
        let (client, fid, stat) = match self.bound_node_fid(nodeid) {
            Ok((client, node_fid)) => {
                let stat = self.nodes()?.node(nodeid)?.stat.clone();
                let fid = client.clone_fid_timeout(node_fid, self.lookup_timeout())?;
                (client, fid, stat)
            }
            Err(error) if error.errno == libc::ENOENT => self.relocated_read_binding(nodeid)?,
            Err(error) => return Err(error),
        };
        if let Err(error) = client.open_timeout(fid, OREAD, self.lookup_timeout()) {
            let _ = client.clunk_timeout(fid, self.control_timeout());
            return Err(error.into());
        }
        Ok((client, fid, stat))
    }

    fn relocated_read_binding(&mut self, nodeid: u64) -> Result<(Client, Fid, Stat)> {
        let (parent_path, expected) = {
            let nodes = self.nodes()?;
            let node = nodes.node(nodeid)?;
            let (_, parent) = node
                .path
                .split_last()
                .ok_or_else(|| Error::new(libc::ESTALE, "root read handle cannot relocate"))?;
            (parent.to_vec(), node.qid)
        };
        let client = self.client_snapshot()?;
        let parent_fid = self.walk_from_source(&client, &parent_path, self.lookup_timeout())?;
        let result = (|| {
            let entries = self.read_directory_for_relocation(&client, parent_fid)?;
            let entry = unique_relocated_entry(&entries, expected)?;
            let fid = client.walk_one_timeout(parent_fid, &entry.name, self.lookup_timeout())?;
            match client.stat_timeout(fid, self.lookup_timeout()) {
                Ok(stat) if same_file_identity(stat.qid, expected) => {
                    Ok((client.clone(), fid, stat))
                }
                Ok(_) => {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    Err(Error::new(
                        libc::ESTALE,
                        "relocated read handle identity changed during reopen",
                    ))
                }
                Err(error) => {
                    let _ = client.clunk_timeout(fid, self.control_timeout());
                    Err(error.into())
                }
            }
        })();
        let _ = client.clunk_timeout(parent_fid, self.control_timeout());
        if result.is_ok() {
            self.record_mount_diagnostic(
                "read_handle_relocated",
                0,
                format!("qid_path={}", expected.path),
            );
        }
        result
    }

    fn read_directory_for_relocation(
        &self,
        client: &Client,
        parent_fid: Fid,
    ) -> Result<Vec<DirEntry>> {
        let fid = client.clone_fid_timeout(parent_fid, self.lookup_timeout())?;
        if let Err(error) = client.open_timeout(fid, OREAD, self.lookup_timeout()) {
            let _ = client.clunk_timeout(fid, self.control_timeout());
            return Err(error.into());
        }
        let result =
            read_open_directory_entries(client, fid, self.read_timeout()).map_err(Error::from);
        let _ = client.clunk_timeout(fid, self.control_timeout());
        result
    }
}

fn unique_relocated_entry(entries: &[DirEntry], expected: Qid) -> Result<&DirEntry> {
    let mut matches = entries
        .iter()
        .filter(|entry| same_file_identity(entry.qid, expected));
    let entry = matches.next().ok_or_else(|| {
        Error::new(
            libc::ENOENT,
            "open read handle identity is absent from its parent",
        )
    })?;
    if matches.next().is_some() {
        return Err(Error::new(
            libc::EPROTO,
            "open read handle identity is ambiguous within its parent",
        ));
    }
    Ok(entry)
}

fn same_file_identity(candidate: Qid, expected: Qid) -> bool {
    candidate.path == expected.path && candidate.qtype == expected.qtype
}

#[cfg(test)]
mod tests {
    use super::unique_relocated_entry;
    use r9p::{qid::Qid, stat::Stat};
    use session::DirEntry;

    fn entry(name: &str, qid: Qid) -> DirEntry {
        DirEntry {
            name: name.as_bytes().to_vec(),
            qid,
            stat: Stat::new(name, qid, 0o444),
        }
    }

    #[test]
    fn relocated_read_identity_selects_one_renamed_sibling() {
        let expected = Qid::new(0, 4, 7);
        let entries = vec![
            entry("other.mkv", Qid::new(0, 1, 8)),
            entry("readable.mkv", Qid::new(0, 9, 7)),
        ];

        assert_eq!(
            unique_relocated_entry(&entries, expected)
                .expect("relocated entry")
                .name,
            b"readable.mkv"
        );
    }

    #[test]
    fn relocated_read_identity_fails_closed_when_absent_or_ambiguous() {
        let expected = Qid::new(0, 4, 7);
        assert_eq!(
            unique_relocated_entry(&[], expected)
                .expect_err("absent identity")
                .errno,
            libc::ENOENT
        );
        let entries = vec![
            entry("first.mkv", Qid::new(0, 4, 7)),
            entry("second.mkv", Qid::new(0, 5, 7)),
        ];
        assert_eq!(
            unique_relocated_entry(&entries, expected)
                .expect_err("ambiguous identity")
                .errno,
            libc::EPROTO
        );
    }
}
