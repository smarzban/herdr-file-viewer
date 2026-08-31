//! Session Transcript Reader — file-level behavior (`src/session.rs`): transcript discovery
//! ordering, the incremental follow (append / partial line / shrink / vanish), and the bounded
//! title scan. Pure-parse behavior lives in the module's unit tests; these cover the seams
//! that touch a real filesystem.

mod common;

use common::TempDir;
use herdr_file_viewer::session::{Category, Follower, list_sessions, session_title};
use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::thread::sleep;
use std::time::Duration;

fn create_line(path: &str) -> String {
    format!("{{\"toolUseResult\":{{\"type\":\"create\",\"filePath\":\"{path}\"}}}}\n")
}

#[test]
fn list_sessions_orders_newest_first_and_ignores_non_transcripts() {
    let dir = TempDir::new();
    fs::write(dir.path().join("older.jsonl"), "{}\n").unwrap();
    sleep(Duration::from_millis(20)); // distinct mtimes on coarse filesystems
    fs::write(dir.path().join("newer.jsonl"), "{}\n").unwrap();
    fs::write(dir.path().join("notes.txt"), "not a transcript").unwrap();
    fs::create_dir(dir.path().join("sub.jsonl")).unwrap(); // a dir with the extension is skipped

    let rows = list_sessions(dir.path());
    let names: Vec<_> = rows
        .iter()
        .map(|s| s.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["newer.jsonl", "older.jsonl"]);

    // Fail-soft discovery: a missing directory is an empty list, not an error.
    assert!(list_sessions(&dir.path().join("absent")).is_empty());
}

#[test]
fn follower_consumes_appends_incrementally() {
    let dir = TempDir::new();
    let t = dir.path().join("s.jsonl");
    fs::write(&t, create_line("/r/a.rs")).unwrap();

    let mut f = Follower::new(&t);
    assert!(f.poll(), "first poll must ingest the existing line");
    assert_eq!(
        f.set_for(Path::new("/r")).in_root.get(Path::new("a.rs")),
        Some(&Category::Created)
    );
    assert!(!f.poll(), "an untouched file is one stat, no change");

    let mut fh = fs::OpenOptions::new().append(true).open(&t).unwrap();
    fh.write_all(create_line("/r/b.rs").as_bytes()).unwrap();
    drop(fh);
    assert!(f.poll(), "an appended line must be picked up");
    assert_eq!(f.set_for(Path::new("/r")).in_root.len(), 2);
}

#[test]
fn follower_carries_a_partial_line_until_it_completes() {
    let dir = TempDir::new();
    let t = dir.path().join("s.jsonl");
    let line = create_line("/r/late.rs");
    let (head, tail) = line.split_at(line.len() / 2);
    fs::write(&t, head).unwrap();

    let mut f = Follower::new(&t);
    assert!(!f.poll(), "half a line is carried, not applied");
    assert!(f.set_for(Path::new("/r")).is_empty());

    let mut fh = fs::OpenOptions::new().append(true).open(&t).unwrap();
    fh.write_all(tail.as_bytes()).unwrap();
    drop(fh);
    assert!(f.poll(), "the completed line must apply exactly once");
    assert_eq!(f.set_for(Path::new("/r")).in_root.len(), 1);
}

#[test]
fn follower_reparses_from_scratch_when_the_file_shrinks() {
    let dir = TempDir::new();
    let t = dir.path().join("s.jsonl");
    fs::write(
        &t,
        format!("{}{}", create_line("/r/a.rs"), create_line("/r/b.rs")),
    )
    .unwrap();
    let mut f = Follower::new(&t);
    assert!(f.poll());
    assert_eq!(f.set_for(Path::new("/r")).in_root.len(), 2);

    fs::write(&t, create_line("/r/only.rs")).unwrap(); // rewritten shorter
    assert!(f.poll(), "a shrunk file must be re-parsed");
    let set = f.set_for(Path::new("/r"));
    assert_eq!(set.in_root.len(), 1);
    assert!(set.in_root.contains_key(Path::new("only.rs")));
}

#[test]
fn follower_empties_the_set_when_the_transcript_vanishes() {
    let dir = TempDir::new();
    let t = dir.path().join("s.jsonl");
    fs::write(&t, create_line("/r/a.rs")).unwrap();
    let mut f = Follower::new(&t);
    assert!(f.poll());

    fs::remove_file(&t).unwrap();
    assert!(f.poll(), "losing the transcript empties the set (a change)");
    assert!(f.set_for(Path::new("/r")).is_empty());
    assert!(!f.poll(), "already empty — a still-missing file is quiet");
}

#[test]
fn session_title_finds_the_last_rename_and_none_without_one() {
    let dir = TempDir::new();
    let t = dir.path().join("s.jsonl");
    fs::write(
        &t,
        concat!(
            "{\"type\":\"custom-title\",\"customTitle\":\"first-name\"}\n",
            "{\"type\":\"user\",\"message\":{}}\n",
            "{\"type\":\"custom-title\",\"customTitle\":\"final-name\"}\n",
        ),
    )
    .unwrap();
    assert_eq!(session_title(&t).as_deref(), Some("final-name"));

    let untitled = dir.path().join("u.jsonl");
    fs::write(&untitled, "{\"type\":\"user\"}\n").unwrap();
    assert_eq!(session_title(&untitled), None);
    assert_eq!(session_title(&dir.path().join("absent.jsonl")), None);
}

#[test]
fn subagent_transcripts_contribute_members() {
    let dir = TempDir::new();
    let t = dir.path().join("s.jsonl");
    fs::write(&t, create_line("/r/main.rs")).unwrap();
    // Task-tool sidechains live in separate files under `<dir>/<session-id>/subagents/`.
    let subs = dir.path().join("s").join("subagents");
    fs::create_dir_all(&subs).unwrap();
    fs::write(subs.join("agent-a.jsonl"), create_line("/r/sub.rs")).unwrap();

    let mut f = Follower::new(&t);
    assert!(f.poll());
    let set = f.set_for(Path::new("/r"));
    assert!(set.in_root.contains_key(Path::new("main.rs")));
    assert!(
        set.in_root.contains_key(Path::new("sub.rs")),
        "a subagent's file activity is part of the session's work"
    );

    // A subagent transcript appearing LATER is discovered on a subsequent poll.
    fs::write(subs.join("agent-b.jsonl"), create_line("/r/late.rs")).unwrap();
    assert!(f.poll());
    assert!(
        f.set_for(Path::new("/r"))
            .in_root
            .contains_key(Path::new("late.rs"))
    );

    // A vanished subagent file rebuilds the merged set without its members.
    fs::remove_file(subs.join("agent-b.jsonl")).unwrap();
    assert!(f.poll(), "losing a contributor is a change");
    let set = f.set_for(Path::new("/r"));
    assert!(!set.in_root.contains_key(Path::new("late.rs")));
    assert!(set.in_root.contains_key(Path::new("main.rs")));
    assert!(set.in_root.contains_key(Path::new("sub.rs")));
}

#[test]
fn a_huge_transcript_streams_in_over_successive_polls() {
    let dir = TempDir::new();
    let t = dir.path().join("s.jsonl");
    // First member up front, then > one read-chunk (4 MiB) of valid-but-irrelevant lines, then
    // the second member — so one bounded poll cannot see both.
    let filler_line = format!("{{\"type\":\"filler\",\"pad\":\"{}\"}}\n", "x".repeat(200));
    let mut body = create_line("/r/first.rs");
    while body.len() < 5 * 1024 * 1024 {
        body.push_str(&filler_line);
    }
    body.push_str(&create_line("/r/last.rs"));
    fs::write(&t, body).unwrap();

    let mut f = Follower::new(&t);
    assert!(f.poll(), "the first chunk lands the first member");
    let after_one = f.set_for(Path::new("/r"));
    assert!(after_one.in_root.contains_key(Path::new("first.rs")));
    assert!(
        !after_one.in_root.contains_key(Path::new("last.rs")),
        "one poll is bounded: the tail has not been read yet"
    );
    // Convergence: successive polls catch the follower up without any file change.
    for _ in 0..4 {
        f.poll();
    }
    assert!(
        f.set_for(Path::new("/r"))
            .in_root
            .contains_key(Path::new("last.rs")),
        "the remainder streams in over the following polls"
    );
}
