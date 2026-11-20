//! Tails files that other programs write (agent transcripts). Polling (250 ms) keeps it
//! std-only; inotify would need FFI.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use argus_core::ledger::SourceCursor;
use argus_core::plan::WatchSpec;
use argus_core::source::{Framer, RawFrame, SourceKind};

use crate::ledger_thread::Cmd;

/// The state of following one file, without threads or timers: each `poll` reads what
/// is new and returns the frames to store.
///
/// A file shorter than what was read, or with other bytes where the last stored frame
/// was, was truncated or rewritten under us (an agent compacting its log). That becomes
/// a `Discontinuity` frame, and the whole new content follows from offset 0. Stored
/// frames are never edited, so every reference stays valid; records the new content
/// repeats are dropped by `ledger::Applied`, so nothing is counted twice.
pub(crate) struct Tailer {
    path: PathBuf,
    kind: SourceKind,
    framer: Framer,
    /// File offset up to which bytes were handed to the framer.
    read_to: u64,
    /// End of the bytes stored so far (complete frames only).
    stored_end: u64,
    /// The last stored frame, compared with the file to detect a rewrite.
    last: Option<(u64, Vec<u8>)>,
}

impl Tailer {
    pub(crate) fn new(path: PathBuf, cursor: &SourceCursor) -> Self {
        let kind = cursor.meta.kind;
        Tailer {
            path,
            kind,
            framer: kind.framer(cursor.resume_at),
            read_to: cursor.resume_at,
            stored_end: cursor.resume_at,
            last: cursor.last.clone(),
        }
    }

    /// New complete frames, or, when the file was truncated or rewritten under us, a
    /// `Discontinuity` followed by the whole new content. A file that does not exist yet
    /// yields nothing.
    pub(crate) fn poll(&mut self) -> std::io::Result<Vec<RawFrame>> {
        let mut file = match File::open(&self.path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        let mut out = Vec::new();
        if self.rewritten(&mut file, len)? {
            out.push(RawFrame::Discontinuity {
                stored_end: self.stored_end,
                file_len: len,
            });
            self.framer = self.kind.framer(0);
            (self.read_to, self.stored_end, self.last) = (0, 0, None);
        }
        if len > self.read_to {
            let mut buf = Vec::new();
            file.seek(SeekFrom::Start(self.read_to))?;
            file.take(len - self.read_to).read_to_end(&mut buf)?;
            self.read_to += buf.len() as u64;
            let frames = self.framer.push(&buf);
            out.extend(self.store(frames));
        }
        Ok(out)
    }

    /// The last poll, then whatever the framer held back (a final line without `\n`).
    pub(crate) fn finish(&mut self) -> std::io::Result<Vec<RawFrame>> {
        let mut out = self.poll()?;
        let rest = self.framer.finish().into_iter().collect();
        out.extend(self.store(rest));
        Ok(out)
    }

    /// Shorter than what was read, or different bytes where the last stored frame was.
    fn rewritten(&self, file: &mut File, len: u64) -> std::io::Result<bool> {
        if len < self.read_to {
            return Ok(true);
        }
        let Some((offset, bytes)) = &self.last else {
            return Ok(false);
        };
        let mut now = Vec::with_capacity(bytes.len());
        file.seek(SeekFrom::Start(*offset))?;
        file.take(bytes.len() as u64).read_to_end(&mut now)?;
        Ok(&now != bytes)
    }

    fn store(&mut self, frames: Vec<RawFrame>) -> Vec<RawFrame> {
        if let Some(RawFrame::Bytes { offset, bytes }) = frames.last() {
            self.stored_end = offset + bytes.len() as u64;
            self.last = Some((*offset, bytes.clone()));
        }
        frames
    }
}

pub(crate) fn tail(
    path: PathBuf,
    cursor: SourceCursor,
    ledger: Sender<Cmd>,
) -> std::thread::JoinHandle<()> {
    todo!("not implemented")
}

/// Files under `root` matching `pattern`, a `/`-separated path whose segments may use
/// `*` (any run of characters within one segment). Sorted, so polls agree.
pub(crate) fn expand(root: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut found = vec![root.to_path_buf()];
    let segments: Vec<&str> = pattern.split('/').filter(|s| !s.is_empty()).collect();
    for (i, segment) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        let mut next = Vec::new();
        for dir in &found {
            if !segment.contains('*') {
                let path = dir.join(segment);
                if path.exists() {
                    next.push(path);
                }
                continue;
            }
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
                if wildcard(segment, name) && (last || is_dir) {
                    next.push(entry.path());
                }
            }
        }
        found = next;
    }
    found.retain(|p| p.is_file());
    found.sort();
    found
}

/// `*` matches any run of characters; everything else matches itself.
fn wildcard(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, rest)) => {
            let Some(name) = name.strip_prefix(head) else {
                return false;
            };
            (0..=name.len())
                .filter(|i| name.is_char_boundary(*i))
                .any(|i| wildcard(rest, &name[i..]))
        }
    }
}

/// Expands `watch` globs under $HOME on each poll; every new match becomes a source
/// (`SourceOrigin::File(path)`, deterministic id) and gets a tailer. This is how subagent
/// transcripts that appear mid-run are picked up.
pub(crate) fn watch(specs: Vec<WatchSpec>, ledger: Sender<Cmd>) -> std::thread::JoinHandle<()> {
    todo!("not implemented")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_watch_pattern_finds_main_and_subagent_transcripts() {
        let home = scratch_file().parent().unwrap().to_path_buf();
        let run = "0f8fad5d-d9cb-469f-a165-70867728950e";
        let files = [
            format!(".claude/projects/-w-jobs-1/{run}.jsonl"),
            format!(".claude/projects/-w-jobs-1/{run}/subagents/agent-a.jsonl"),
            format!(".claude/projects/-w-jobs-1/{run}/subagents/agent-b.jsonl"),
            format!(".claude/projects/-other/{run}.jsonl"),
            ".claude/projects/-w-jobs-1/someone-else.jsonl".to_owned(),
            format!(".claude/projects/-w-jobs-1/{run}/subagents/notes.txt"),
        ];
        for f in &files {
            let path = home.join(f);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let rel = |paths: Vec<PathBuf>| -> Vec<String> {
            paths
                .iter()
                .map(|p| {
                    p.strip_prefix(&home)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        };
        assert_eq!(
            rel(expand(&home, &format!(".claude/projects/*/{run}.jsonl"))),
            vec![files[3].clone(), files[0].clone()]
        );
        assert_eq!(
            rel(expand(
                &home,
                &format!(".claude/projects/*/{run}/subagents/*.jsonl")
            )),
            vec![files[1].clone(), files[2].clone()]
        );
        assert!(expand(&home, ".codex/sessions/*.jsonl").is_empty());
    }
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    use argus_core::ids::{Attempt, Id128, RunId};
    use argus_core::source::{SourceKind, SourceMeta, SourceOrigin};

    fn scratch_file() -> PathBuf {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "argus-tail-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("s.jsonl")
    }

    fn cursor(path: &Path, resume_at: u64, last: Option<(u64, &[u8])>) -> SourceCursor {
        let origin = SourceOrigin::File(path.to_path_buf());
        let run = RunId(Id128(1));
        SourceCursor {
            meta: SourceMeta {
                id: SourceMeta::derive_id(run, Attempt(0), &origin),
                run,
                attempt: Attempt(0),
                kind: SourceKind::ClaudeTranscript,
                origin,
            },
            resume_at,
            last: last.map(|(o, b)| (o, b.to_vec())),
        }
    }

    fn bytes(offset: u64, b: &str) -> RawFrame {
        RawFrame::Bytes {
            offset,
            bytes: b.as_bytes().to_vec(),
        }
    }

    fn append(path: &Path, b: &str) {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        f.write_all(b.as_bytes()).unwrap();
    }

    #[test]
    fn appended_lines_are_stored_once_they_are_complete() {
        let path = scratch_file();
        let mut t = Tailer::new(path.clone(), &cursor(&path, 0, None));
        // The agent has not created its transcript yet.
        assert_eq!(t.poll().unwrap(), vec![]);
        append(&path, "a\n");
        assert_eq!(t.poll().unwrap(), vec![bytes(0, "a\n")]);
        assert_eq!(t.poll().unwrap(), vec![]);
        append(&path, "{\"b\"");
        assert_eq!(t.poll().unwrap(), vec![]);
        append(&path, ":1}\n");
        assert_eq!(t.poll().unwrap(), vec![bytes(2, "{\"b\":1}\n")]);
        append(&path, "tail");
        assert_eq!(t.finish().unwrap(), vec![bytes(10, "tail")]);
    }

    #[test]
    fn a_restart_resumes_after_the_last_stored_frame() {
        let path = scratch_file();
        append(&path, "a\nb\nc");
        let mut t = Tailer::new(path.clone(), &cursor(&path, 2, Some((0, b"a\n"))));
        assert_eq!(t.poll().unwrap(), vec![bytes(2, "b\n")]);
        assert_eq!(t.finish().unwrap(), vec![bytes(4, "c")]);
    }

    #[test]
    fn a_truncated_file_is_a_discontinuity_then_its_new_content() {
        let path = scratch_file();
        append(&path, "a\nb\n");
        let mut t = Tailer::new(path.clone(), &cursor(&path, 0, None));
        assert_eq!(t.poll().unwrap().len(), 2);
        fs::write(&path, "x\n").unwrap();
        assert_eq!(
            t.poll().unwrap(),
            vec![
                RawFrame::Discontinuity {
                    stored_end: 4,
                    file_len: 2
                },
                bytes(0, "x\n"),
            ]
        );
        append(&path, "y\n");
        assert_eq!(t.poll().unwrap(), vec![bytes(2, "y\n")]);
    }

    #[test]
    fn a_rewrite_that_keeps_the_length_is_still_seen() {
        let path = scratch_file();
        append(&path, "a\nb\n");
        let mut t = Tailer::new(path.clone(), &cursor(&path, 0, None));
        t.poll().unwrap();
        fs::write(&path, "a\nc\nd\n").unwrap();
        let frames = t.poll().unwrap();
        assert_eq!(
            frames,
            vec![
                RawFrame::Discontinuity {
                    stored_end: 4,
                    file_len: 6
                },
                bytes(0, "a\n"),
                bytes(2, "c\n"),
                bytes(4, "d\n"),
            ]
        );
    }

    #[test]
    fn a_file_rewritten_while_the_host_was_down_is_caught_on_resume() {
        let path = scratch_file();
        append(&path, "z\nb\nnew\n");
        // The log says "a\n" was stored at 0, but the file now holds something else there.
        let mut t = Tailer::new(path.clone(), &cursor(&path, 4, Some((0, b"a\n"))));
        let frames = t.poll().unwrap();
        assert_eq!(
            frames[0],
            RawFrame::Discontinuity {
                stored_end: 4,
                file_len: 8
            }
        );
        assert_eq!(
            &frames[1..],
            &[bytes(0, "z\n"), bytes(2, "b\n"), bytes(4, "new\n")]
        );
    }
}
