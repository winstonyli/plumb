use super::*;
use plumb_core::Tree;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        // `rmdir` on the junction removes only the link, not its target.
        let _ = std::process::Command::new("cmd").args(["/C", "rmdir"]).arg(self.0.join("junc")).output();
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// root/{empty/, a/{small.txt, big.bin (100000 B)}, b/c/five.dat (5000 B), junc -> a}
fn fixture(tag: &str) -> Fixture {
    let root = std::env::temp_dir().join(format!("plumb-walk-{}-{tag}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("empty")).unwrap();
    fs::create_dir_all(root.join("a")).unwrap();
    fs::create_dir_all(root.join("b").join("c")).unwrap();
    fs::write(root.join("a").join("small.txt"), b"hi").unwrap();
    fs::write(root.join("a").join("big.bin"), vec![7u8; 100_000]).unwrap();
    fs::write(root.join("b").join("c").join("five.dat"), vec![1u8; 5000]).unwrap();
    let out = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(root.join("junc"))
        .arg(root.join("a"))
        .output()
        .unwrap();
    assert!(out.status.success(), "mklink /J failed");
    Fixture(root)
}

fn scan(root: &Path, threads: usize) -> (Tree, Progress) {
    let p = Progress::default();
    let mut b = TreeBuilder::new();
    WalkScanner { threads }.scan(root, &mut b, &p).unwrap();
    (b.finish().unwrap(), p)
}

/// Paths relative to the root node.
fn rel(t: &Tree, i: NodeId) -> String {
    t.path(i)[t.path(NodeId(0)).len() + 1..].to_string()
}

fn rel_paths(t: &Tree) -> BTreeSet<String> {
    (1..t.len() as u32).map(|i| rel(t, NodeId(i))).collect()
}

fn find(t: &Tree, path: &str) -> NodeId {
    (1..t.len() as u32).map(NodeId).find(|&i| rel(t, i) == path).unwrap()
}

#[test]
fn finds_every_entry_and_does_not_follow_junctions() {
    let f = fixture("paths");
    let (t, _) = scan(&f.0, 3);
    let want: BTreeSet<String> =
        ["empty", "a", "a\\small.txt", "a\\big.bin", "b", "b\\c", "b\\c\\five.dat", "junc"]
            .into_iter()
            .map(String::from)
            .collect();
    assert_eq!(rel_paths(&t), want);
    let junc = find(&t, "junc");
    assert!(t.flags[junc].contains(NodeFlags::REPARSE | NodeFlags::DIR));
    assert_eq!(t.children(junc).count(), 0);
    assert!(!t.flags[find(&t, "a")].contains(NodeFlags::REPARSE));
}

#[test]
fn sizes_are_allocated_bytes() {
    let f = fixture("sizes");
    let (t, _) = scan(&f.0, 2);
    let big = t.size[find(&t, "a\\big.bin")];
    assert!((100_000..=100_000 + 65_536).contains(&big) && big % 512 == 0, "big = {big}");
    let five = t.size[find(&t, "b\\c\\five.dat")];
    assert!((5000..=5000 + 65_536).contains(&five) && five % 512 == 0, "five = {five}");
    assert_eq!(t.size[find(&t, "a")], 0); // directories count as 0
    assert_eq!(t.size[find(&t, "a\\small.txt")], 0); // resident data uses no clusters
    assert_eq!(t.subtree_size[find(&t, "a")], t.size[find(&t, "a\\small.txt")] + big);
}

#[test]
fn thread_count_does_not_change_the_result() {
    let f = fixture("threads");
    let (a, _) = scan(&f.0, 1);
    let (b, _) = scan(&f.0, 8);
    assert_eq!(rel_paths(&a), rel_paths(&b));
    assert_eq!(a.subtree_size[NodeId(0)], b.subtree_size[NodeId(0)]);
}

#[test]
fn progress_counts_dirs_and_files() {
    let f = fixture("progress");
    let (t, p) = scan(&f.0, 2);
    let (dirs, files, bytes) = p.snapshot();
    assert_eq!(dirs, 6); // root, empty, a, b, c, junc
    assert_eq!(files, 3);
    assert_eq!(bytes, t.subtree_size[NodeId(0)]);
}

#[test]
fn root_is_named_without_prefix_or_trailing_backslash() {
    let f = fixture("name");
    let (t, _) = scan(&f.0, 1);
    let name = t.name_string(NodeId(0));
    assert!(!name.starts_with("\\\\?\\") && !name.ends_with('\\'), "{name}");
    assert!(t.flags[NodeId(0)].contains(NodeFlags::DIR));
}

#[test]
fn missing_root_is_an_error() {
    let p = Progress::default();
    let mut b = TreeBuilder::new();
    let r = WalkScanner::default().scan(Path::new("C:\\definitely\\not\\here\\plumb"), &mut b, &p);
    assert!(matches!(r, Err(ScanError::Root(_))));
}
