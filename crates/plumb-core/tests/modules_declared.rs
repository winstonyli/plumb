//! Every source file is declared as a module by its parent. An undeclared file compiles to nothing and its tests stop
//! running without a sound: commit 7bf86da dropped two `mod` lines and 17 tests went dark.
use std::fs;
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            rs_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// Source files (path under `crates/`, forward slashes) that are not declared by a parent on purpose. Only shrinks.
const ALLOWED: &[&str] = &[];

#[test]
fn every_source_file_is_declared_by_its_parent_module() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut undeclared = Vec::new();
    for c in fs::read_dir(crates).unwrap() {
        let src = c.unwrap().path().join("src");
        if !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rs_files(&src, &mut files);
        for f in files {
            let stem = f.file_stem().unwrap().to_str().unwrap().to_string();
            let dir = f.parent().unwrap();
            if dir == src && matches!(stem.as_str(), "lib" | "main") || stem == "mod" || dir.ends_with("bin") {
                continue;
            }
            let parents = if dir == src { vec![src.join("lib.rs"), src.join("main.rs")] } else { vec![dir.with_extension("rs"), dir.join("mod.rs")] };
            let declared = parents.iter().filter_map(|p| fs::read_to_string(p).ok()).any(|t| t.contains(&format!("mod {stem};")) || t.contains(&format!("mod {stem} {{")));
            let rel = f.strip_prefix(crates).unwrap().to_string_lossy().replace('\\', "/");
            if !declared && !ALLOWED.contains(&rel.as_str()) {
                undeclared.push(rel);
            }
        }
    }
    assert!(undeclared.is_empty(), "source files no module declares (their tests never run): {undeclared:?}");
}
