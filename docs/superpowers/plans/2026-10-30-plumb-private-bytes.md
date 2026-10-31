---
status: active
---

# Private bytes metric Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The Processes view's metric button offers private bytes (committed memory no other process shares) beside the working set, so the map and the first column can answer "who holds the most commit" and are not inflated by shared pages.

**Architecture:** `plumb-scan` already reads `ProcEntry.private_bytes` (`procs.rs:17,92`); `window/procs.rs:135` drops it. The value is carried through `Sample.private` into `Known.private`, and `Metric` gains a variant `Private` that `Known::value` selects, so the existing `set_metric` roll-up, the map, the sort and the first column follow with no new mechanism. The button cycles Memory, Private, CPU, I/O.

**Tech Stack:** Rust; `plumb-core::procs`, `plumb` (`app.rs`, `window/procs.rs`).

**Spec:** `ROADMAP.md` v0.10 (third part); decision m1 (a fourth metric) from the 2026-10-30 session; the old limitation is in `PARKED.md` ("Working-set memory counts shared pages once per process").

## Global Constraints

- `Metric::Memory` keeps meaning working set and keeps its label "Memory"; nothing about the disk views changes.
- No compatibility shims; no new column (the row table keeps its five columns).
- The metric is not saved across runs today (`proc_metric` starts at `Metric::default()`); do not start saving it.
- Build with `CARGO_TARGET_DIR=target/alt` while the user's `plumb.exe` runs; delete `target/alt` afterwards. Edits use the Edit tool; commits are small; nothing is pushed or tagged.

## Review Focus

- System Idle (pid 0) weighs nothing under Private too (as under Memory).
- Cycling the button four times returns to Memory; a click while on Private goes to CPU.
- Switching to Private re-rolls every parent's `subtree_size` (a parent's own private bytes plus its children's), not only the leaves.
- The first column title, the tooltip noun and the button label all name the active metric.

---

### Task 1: Carry private bytes into the tree (`plumb-core`)

**Files:**
- Modify: `crates/plumb-core/src/procs.rs` (`Sample`, `Metric`, `Metric::next`, `Known`, `Known::value`, the `Known` literal in `apply`, step 5 of `apply`, tests)

**Interfaces:**
- Produces: `Sample.private: u64`; `Metric::Private`; `Metric::next` order Memory, Private, Cpu, Io.

- [ ] **Step 1: Write the failing tests.** In `procs.rs` tests, change the helper `s` to set the new field and replace `the_metric_cycles_through_all_three`:

```rust
    fn s(pid: u32, parent: u32, created: u64, name: &str, memory: u64) -> Sample {
        Sample { pid, parent, created, name: name.into(), memory, private: memory / 2, cpu_100ns: 0, io_bytes: 0 }
    }
```

```rust
    #[test]
    fn the_metric_cycles_through_all_four() {
        let mut m = Metric::Memory;
        let mut seen = vec![m];
        for _ in 0..3 {
            m = m.next();
            seen.push(m);
        }
        assert_eq!(seen, [Metric::Memory, Metric::Private, Metric::Cpu, Metric::Io]);
        assert_eq!(m.next(), Metric::Memory);
    }

    #[test]
    fn private_bytes_weigh_each_process_and_roll_up_through_parents() {
        let mut p = ProcTree::new();
        p.apply(&[s(10, 1, 1, "a.exe", 100), s(11, 10, 2, "b.exe", 60)], 0);
        assert_eq!((p.tree.size[node(&p, 10)], p.tree.subtree_size[node(&p, 10)]), (100, 160));
        p.set_metric(Metric::Private);
        assert_eq!((p.tree.size[node(&p, 10)], p.tree.subtree_size[node(&p, 10)]), (50, 80), "own private bytes, and the family's");
        p.set_metric(Metric::Memory);
        assert_eq!(p.tree.subtree_size[node(&p, 10)], 160);
    }
```

In `system_idle_weighs_nothing_under_any_metric`, change the metric list to `[Metric::Memory, Metric::Private, Metric::Cpu, Metric::Io]`.

- [ ] **Step 2: Run to verify it fails.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb-core procs 2>&1 | grep -E "^error|test result" | head`
Expected: compile errors (`no field private`, `no variant Private`).

- [ ] **Step 3: Implement.** In `procs.rs`:
  - `Sample`: after `pub memory: u64,` add `/// Private bytes (commit), which no other process shares.\n    pub private: u64,`.
  - `Metric`: after `Memory,` add `/// Private bytes: committed memory no other process shares.\n    Private,`; update the doc of the enum's `Memory` variant to say "Working set." if it has none.
  - `Metric::next`: `Metric::Memory => Metric::Private, Metric::Private => Metric::Cpu, Metric::Cpu => Metric::Io, Metric::Io => Metric::Memory`; doc: "Memory, Private, CPU, I/O, and round again."
  - `Known`: add `private: u64,` after `memory: u64,`; `Known::value`: add `Metric::Private => self.private,`.
  - The `Known { id, memory: 0, ... }` literal in `apply`: add `private: 0,`.
  - In step 5 of `apply`, after `k.memory = ...;` add `k.private = if s.pid == 0 { 0 } else { s.private };`.

- [ ] **Step 4: Run to verify it passes.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb-core procs 2>&1 | grep -E "^error|test result"`
Expected: all `procs` tests pass.

- [ ] **Step 5: Commit** (the workspace does not build until Task 2 fixes the `Sample` literals, so run only `-p plumb-core` here).
```bash
git add crates/plumb-core/src/procs.rs
git commit -m "Procs: private bytes as a fourth metric in the tree bookkeeping"
```

---

### Task 2: Offer it in the window (`plumb`)

**Files:**
- Modify: `crates/plumb/src/app.rs` (`metric_label`, the metric button tooltip ~678, tests), `crates/plumb/src/window/procs.rs:135`, `crates/plumb/tests/render.rs` (three `sample` closures)
- Test: `crates/plumb/src/app.rs`

**Interfaces:**
- Consumes: `Sample.private`, `Metric::Private`, the new cycle order from Task 1.
- Produces: `metric_label(Metric::Private) == "Private"`; `metric_help(m: Metric) -> &'static str`.

- [ ] **Step 1: Write the failing tests.** In `app.rs`, update `clicking_the_metric_button_cycles_memory_cpu_io_and_back` (rename it `..._memory_private_cpu_io_and_back`) so the click sequence is Memory, Private, CPU, I/O, Memory:

```rust
        assert_eq!(a.proc_metric, Metric::Memory);
        assert_eq!(click(&mut a), Effect::Metric(Metric::Private));
        a.proc_metric = Metric::Private; // the window applies the effect
        assert_eq!(click(&mut a), Effect::Metric(Metric::Cpu));
        a.proc_metric = Metric::Cpu;
        assert_eq!(click(&mut a), Effect::Metric(Metric::Io));
        a.proc_metric = Metric::Io;
        assert_eq!(click(&mut a), Effect::Metric(Metric::Memory));
        assert_eq!(ViewKind::Processes.titles(Metric::Private)[1], "Private");
        assert_eq!(ViewKind::Processes.size_text(Metric::Private, 2048), fmt::size(2048));
```

(keep the existing CPU and I/O assertions that follow in that test) and add:

```rust
    #[test]
    fn every_metric_has_a_label_and_a_one_line_definition() {
        for m in [Metric::Memory, Metric::Private, Metric::Cpu, Metric::Io] {
            assert!(!metric_label(m).is_empty() && !metric_help(m).is_empty(), "{m:?}");
        }
        assert!(metric_help(Metric::Private).contains("not shared"));
        assert!(metric_help(Metric::Memory).contains("working set"));
    }
```

Also update the three `Sample` literals in `app.rs` (lines ~1555 and ~2365; add `private: 0` / `private: memory / 2`) and the three `sample` closures in `tests/render.rs` (add `private: memory / 2,`). Make the first edits now so the tests compile except for the missing `metric_help`.

- [ ] **Step 2: Run to verify it fails.**
Run: `CARGO_TARGET_DIR=target/alt cargo test -p plumb --lib metric 2>&1 | grep -E "^error|test result" | head`
Expected: compile errors (`cannot find function metric_help`; `non-exhaustive patterns: Private` in `metric_label`).

- [ ] **Step 3: Implement.**

`metric_label` (`app.rs`):

```rust
        Metric::Memory => "Memory",
        Metric::Private => "Private",
```

Add below it:

```rust
/// One line saying what a map metric counts, for the metric button's tooltip.
pub fn metric_help(m: Metric) -> &'static str {
    match m {
        Metric::Memory => "the working set: resident pages, shared ones counted once per process",
        Metric::Private => "private bytes: committed memory not shared with other processes",
        Metric::Cpu => "share of one core",
        Metric::Io => "bytes transferred per second",
    }
}
```

The `Hit::MetricButton` tooltip becomes:

```rust
format!("The map and the first column weigh processes by {}: {}; click for the next", metric_label(self.proc_metric), metric_help(self.proc_metric))
```

`window/procs.rs:135`: add `private: e.private_bytes,` after `memory: e.working_set,`.

`process_tip` and `tip` lines that lowercase `metric_label` ("400 KB private in this and its children") stay as they are; they read acceptably and the button tooltip carries the definition.

- [ ] **Step 4: Run to verify it passes.**
Run: `CARGO_TARGET_DIR=target/alt cargo test --workspace --locked 2>&1 | grep -E "^error|test result|FAILED"` then `CARGO_TARGET_DIR=target/alt cargo clippy --workspace --all-targets 2>&1 | tail -3`
Expected: all pass, clippy clean.

- [ ] **Step 5: Check the button fits.** `METRIC_W` is 72 DIPs (`layout.rs:40`). Run `PLUMB_SHOT_DIR=<scratch> CARGO_TARGET_DIR=target/alt cargo test -p plumb --test render the_processes_map_follows_the_metric_button`, then add the Private shot by temporarily looping `[Metric::Memory, Metric::Private, Metric::Cpu, Metric::Io]` in that test: open `plumb-processes-private.png` and confirm the label "Private" is not clipped. Keep the four-metric loop if the test's area assertions still hold (they compare `big-memory.exe` between metrics; if the assertion is written for three values, extend it to the Private value, which is `memory / 2` for every process, so the area equals Memory's).

- [ ] **Step 6: Commit.**
```bash
git add crates/plumb
git commit -m "Processes: the metric button offers private bytes beside the working set"
```

---

### Task 3: Docs

**Files:**
- Modify: `PARKED.md` (delete "Working-set memory counts shared pages once per process"), `docs/superpowers/specs/2026-10-16-plumb-views-design.md` (the metric description), this plan's front matter.

- [ ] **Step 1:** Delete the PARKED entry (the trigger "a decision to offer private bytes as a metric" is met; the working-set total in the status line still exceeds RAM by design and the tooltip says why).
- [ ] **Step 2: Grep for stale wording.**
Run: `grep -rniE "memory, cpu, i/o|three metrics|cycles through all three|Memory, CPU, I/O" PARKED.md ROADMAP.md CLAUDE.md docs crates --include=*.md --include=*.rs | grep -v "docs/superpowers/plans/"`
Expected: no remaining statement that there are three metrics; fix each hit.
- [ ] **Step 3:** Set `status: done`, commit.
```bash
git add PARKED.md docs
git commit -m "Docs: private bytes metric landed; PARKED and the views spec follow"
```
