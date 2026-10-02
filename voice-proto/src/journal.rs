use crate::frame::{valid_call_id, CallBody};
use crate::stream::Outbox;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Line {
    Frame { seq: u64, body: CallBody },
    Ack { ack: u64 },
}

#[derive(Default)]
struct Journal {
    last: u64,
    frames: BTreeMap<u64, CallBody>,
}

/// One append-only file per call, `<call_id>.out.ndjson`, synced on every
/// write.
pub struct FileOutbox {
    dir: PathBuf,
    calls: HashMap<String, Journal>,
}

fn bad_id(id: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("unsafe call id {id:?}"),
    )
}

impl FileOutbox {
    pub fn open(dir: &Path) -> io::Result<FileOutbox> {
        std::fs::create_dir_all(dir)?;
        let mut calls = HashMap::new();
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(id) = name.strip_suffix(".out.ndjson") else {
                continue;
            };
            if !valid_call_id(id) {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            let mut j = Journal::default();
            let mut good = 0;
            for line in bytes.split_inclusive(|b| *b == b'\n') {
                if !line.ends_with(b"\n") {
                    break;
                }
                match serde_json::from_slice::<Line>(line) {
                    Ok(Line::Frame { seq, body }) => {
                        j.last = j.last.max(seq);
                        j.frames.insert(seq, body);
                    }
                    Ok(Line::Ack { ack }) => j.frames.retain(|s, _| *s > ack),
                    Err(_) => break,
                }
                good += line.len();
            }
            if good < bytes.len() {
                let f = std::fs::OpenOptions::new().write(true).open(&path)?;
                f.set_len(good as u64)?;
                f.sync_data()?;
            }
            calls.insert(id.to_string(), j);
        }
        Ok(FileOutbox {
            dir: dir.to_path_buf(),
            calls,
        })
    }

    fn path(&self, call_id: &str) -> PathBuf {
        self.dir.join(format!("{call_id}.out.ndjson"))
    }

    /// Appends `line` durably; a failed write is cut back off the file so the
    /// next line starts clean.
    fn write_line(&self, call_id: &str, line: &Line) -> io::Result<()> {
        let path = self.path(call_id);
        let mut bytes = serde_json::to_vec(line)?;
        bytes.push(b'\n');
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let len = f.metadata()?.len();
        if let Err(e) = f.write_all(&bytes).and_then(|()| f.sync_data()) {
            let _ = f.set_len(len);
            return Err(e);
        }
        if !self.calls.contains_key(call_id) {
            std::fs::File::open(&self.dir)?.sync_all()?;
        }
        Ok(())
    }
}

impl Outbox for FileOutbox {
    fn append(&mut self, call_id: &str, body: &CallBody) -> io::Result<u64> {
        if !valid_call_id(call_id) {
            return Err(bad_id(call_id));
        }
        let seq = self.calls.get(call_id).map_or(0, |j| j.last) + 1;
        self.write_line(
            call_id,
            &Line::Frame {
                seq,
                body: body.clone(),
            },
        )?;
        let j = self.calls.entry(call_id.to_string()).or_default();
        j.last = seq;
        j.frames.insert(seq, body.clone());
        Ok(seq)
    }

    fn unacked(&self, call_id: &str, after: u64) -> io::Result<Vec<(u64, CallBody)>> {
        Ok(self
            .calls
            .get(call_id)
            .map(|j| {
                j.frames
                    .range(after.saturating_add(1)..)
                    .map(|(s, b)| (*s, b.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    fn ack(&mut self, call_id: &str, upto: u64) -> io::Result<()> {
        if !self.calls.contains_key(call_id) {
            return Ok(());
        }
        self.write_line(call_id, &Line::Ack { ack: upto })?;
        if let Some(j) = self.calls.get_mut(call_id) {
            j.frames.retain(|s, _| *s > upto);
        }
        Ok(())
    }

    fn pending_calls(&self) -> io::Result<Vec<String>> {
        let mut ids: Vec<String> = self
            .calls
            .iter()
            .filter(|(_, j)| !j.frames.is_empty())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        Ok(ids)
    }

    fn forget(&mut self, call_id: &str) -> io::Result<()> {
        if !valid_call_id(call_id) {
            return Err(bad_id(call_id));
        }
        self.calls.remove(call_id);
        match std::fs::remove_file(self.path(call_id)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// The last applied seq of each call Note sent, `<call_id>.in`, replaced
/// atomically.
pub struct AppliedFile {
    dir: PathBuf,
}

impl AppliedFile {
    pub fn new(dir: &Path) -> AppliedFile {
        AppliedFile {
            dir: dir.to_path_buf(),
        }
    }

    fn path(&self, call_id: &str) -> PathBuf {
        self.dir.join(format!("{call_id}.in"))
    }

    pub fn applied(&self, call_id: &str) -> u64 {
        if !valid_call_id(call_id) {
            return 0;
        }
        std::fs::read_to_string(self.path(call_id))
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    }

    pub fn set_applied(&self, call_id: &str, seq: u64) -> io::Result<()> {
        if !valid_call_id(call_id) {
            return Err(bad_id(call_id));
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!("{call_id}.in.tmp"));
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(seq.to_string().as_bytes())?;
        f.sync_data()?;
        std::fs::rename(&tmp, self.path(call_id))?;
        std::fs::File::open(&self.dir)?.sync_all()
    }

    pub fn forget(&self, call_id: &str) -> io::Result<()> {
        if !valid_call_id(call_id) {
            return Err(bad_id(call_id));
        }
        match std::fs::remove_file(self.path(call_id)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Removes the records last written more than `age` ago and returns how
    /// many. A record outlives its call so a frame still in flight for it is
    /// acknowledged rather than mistaken for the start of a new call.
    pub fn prune(&self, age: std::time::Duration) -> io::Result<usize> {
        let cutoff = std::time::SystemTime::now() - age;
        let entries = match std::fs::read_dir(&self.dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            other => other?,
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry?;
            let is_record = entry.path().extension().is_some_and(|x| x == "in");
            if is_record && entry.metadata()?.modified()? < cutoff {
                std::fs::remove_file(entry.path())?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn frames_survive_a_reopen_and_numbering_continues() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut o = FileOutbox::open(dir.path()).unwrap();
            assert_eq!(o.append("c1", &CallBody::Ringing).unwrap(), 1);
            assert_eq!(o.append("c1", &CallBody::Ended).unwrap(), 2);
            o.ack("c1", 1).unwrap();
        }
        let mut o = FileOutbox::open(dir.path()).unwrap();
        assert_eq!(o.unacked("c1", 0).unwrap(), vec![(2, CallBody::Ended)]);
        assert_eq!(o.pending_calls().unwrap(), vec!["c1".to_string()]);
        assert_eq!(o.append("c1", &CallBody::Ringing).unwrap(), 3);
    }

    #[test]
    fn a_torn_last_line_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut o = FileOutbox::open(dir.path()).unwrap();
            o.append("c1", &CallBody::Ringing).unwrap();
        }
        let path = dir.path().join("c1.out.ndjson");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"seq\":2,\"bo")
            .unwrap();
        let o = FileOutbox::open(dir.path()).unwrap();
        assert_eq!(o.unacked("c1", 0).unwrap(), vec![(1, CallBody::Ringing)]);
    }

    #[test]
    fn a_frame_appended_after_a_torn_line_survives_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut o = FileOutbox::open(dir.path()).unwrap();
            o.append("c1", &CallBody::Ringing).unwrap();
        }
        let path = dir.path().join("c1.out.ndjson");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"seq\":2,\"bo\xe2")
            .unwrap();
        {
            let mut o = FileOutbox::open(dir.path()).unwrap();
            assert_eq!(o.append("c1", &CallBody::Ended).unwrap(), 2);
        }
        let o = FileOutbox::open(dir.path()).unwrap();
        assert_eq!(
            o.unacked("c1", 0).unwrap(),
            vec![(1, CallBody::Ringing), (2, CallBody::Ended)]
        );
    }

    #[test]
    fn forget_removes_the_journal() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = FileOutbox::open(dir.path()).unwrap();
        o.append("c1", &CallBody::Ringing).unwrap();
        o.forget("c1").unwrap();
        assert!(!dir.path().join("c1.out.ndjson").exists());
        assert!(FileOutbox::open(dir.path())
            .unwrap()
            .pending_calls()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pruning_drops_only_old_applied_records() {
        let dir = tempfile::tempdir().unwrap();
        let a = AppliedFile::new(dir.path());
        a.set_applied("old", 3).unwrap();
        a.set_applied("new", 2).unwrap();
        std::fs::write(dir.path().join("old.out.ndjson"), b"").unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for name in ["old.in", "old.out.ndjson"] {
            std::fs::File::options().write(true).open(dir.path().join(name)).unwrap().set_modified(past).unwrap();
        }
        assert_eq!(a.prune(std::time::Duration::from_secs(60)).unwrap(), 1);
        assert_eq!((a.applied("old"), a.applied("new")), (0, 2));
        assert!(dir.path().join("old.out.ndjson").exists(), "only applied records go");
        assert_eq!(AppliedFile::new(&dir.path().join("absent")).prune(std::time::Duration::ZERO).unwrap(), 0);
    }

    #[test]
    fn applied_is_durable_and_forgettable() {
        let dir = tempfile::tempdir().unwrap();
        let a = AppliedFile::new(dir.path());
        assert_eq!(a.applied("c1"), 0);
        a.set_applied("c1", 4).unwrap();
        assert_eq!(AppliedFile::new(dir.path()).applied("c1"), 4);
        a.forget("c1").unwrap();
        assert_eq!(a.applied("c1"), 0);
    }

    #[test]
    fn an_unsafe_call_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = FileOutbox::open(dir.path()).unwrap();
        assert!(o.append("../x", &CallBody::Ringing).is_err());
    }

    #[test]
    fn forget_refuses_an_unsafe_call_id() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("journal");
        std::fs::write(root.path().join("x.out.ndjson"), b"").unwrap();
        std::fs::write(root.path().join("x.in"), b"1").unwrap();
        let mut o = FileOutbox::open(&dir).unwrap();
        assert_eq!(
            o.forget("../x").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        let a = AppliedFile::new(&dir);
        assert_eq!(
            a.forget("../x").unwrap_err().kind(),
            std::io::ErrorKind::InvalidInput
        );
        assert!(root.path().join("x.out.ndjson").exists());
        assert!(root.path().join("x.in").exists());
    }

    #[test]
    fn unacked_after_the_last_possible_seq_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut o = FileOutbox::open(dir.path()).unwrap();
        o.append("c1", &CallBody::Ringing).unwrap();
        assert!(o.unacked("c1", u64::MAX).unwrap().is_empty());
    }
}
