use super::*;
use crate::layout::{COL_COUNT, COL_PCT, COL_SCORE};
use crate::outcome::Failure;

/// The text-returning forms of the tooltip and status builders, which write into a buffer.
fn tip_str(a: &App, x: f32, y: f32) -> Option<String> {
    let mut s = String::new();
    a.tip_at(x, y, &mut s).then_some(s)
}

fn target_str(a: &App, x: f32, y: f32) -> Option<(TipKey, String)> {
    let mut s = String::new();
    a.tip_target(x, y, &mut s).map(|k| (k, s))
}

fn tile_str(tree: &Tree, t: &Tile, now: u32) -> String {
    let mut s = String::new();
    tile_tip(tree, t, now, Attrs::default(), &mut s);
    s
}

fn proc_tip_str(a: &App, tree: &Tree, t: &Tile) -> String {
    let mut s = String::new();
    a.process_tip(tree, t, &mut s);
    s
}

fn toast_str(a: &App) -> String {
    a.toast.lines().join("\n")
}

fn status_str(a: &App, full: bool) -> String {
    let mut s = String::new();
    a.write_status(full, true, &mut s);
    s
}

#[test]
fn a_disk_view_keeps_its_titles_sorts_and_last_two_columns() {
    assert_eq!(ViewKind::Disk.spec().titles(Metric::Cpu), ["Name", "Stale", "% of parent", "Size", "Items", "Modified"]);
    let keys: Vec<_> = (0..COL_COUNT).map(|c| ViewKind::Disk.spec().cols[c].sort).collect();
    assert_eq!(keys, [SortKey::Name, SortKey::Score, SortKey::Size, SortKey::Size, SortKey::Items, SortKey::Modified]);
    let mut b = plumb_core::TreeBuilder::new();
    let r = b.push_str(NodeId::NONE, "r", 0, plumb_core::NodeFlags::DIR);
    let f = b.push_str(r, "f", 5, plumb_core::NodeFlags::empty());
    b.set_mtime(f, std::num::NonZeroU32::new(1_700_000_000));
    let t = b.finish().unwrap();
    assert_eq!((ViewKind::Disk.spec().last_two)(&t, f), (fmt::count(1), Some(fmt::date(fmt::local_day(1_700_000_000)))));
    assert_eq!((ViewKind::Disk.spec().last_two)(&t, r), (fmt::count(1), None), "no date, no fourth cell");
}

#[test]
fn only_the_score_header_has_a_tooltip() {
    let a = scanned();
    let (l, y) = (&a.layout, a.layout.header().y + 4.0);
    let (key, text) = target_str(&a, l.column(crate::layout::COL_SCORE).0 + 20.0, y).expect("a tip over the Score header");
    assert_eq!(key, TipKey::Area(l.header_cell(crate::layout::COL_SCORE)));
    assert_eq!(text, ViewKind::Disk.spec().score_help);
    assert!(target_str(&a, l.column(crate::layout::COL_SIZE).0 + 20.0, y).is_none());
    let (p, _) = process_app();
    let mut s = String::new();
    p.tip_target(p.layout.column(crate::layout::COL_SCORE).0 + 20.0, p.layout.header().y + 4.0, &mut s);
    assert_eq!(s, ViewKind::Processes.spec().score_help);
}

#[test]
fn the_score_cell_reads_as_bytes_for_a_disk_and_as_a_percent_for_processes() {
    let mut t = plumb_core::TreeBuilder::new();
    let r = t.push_str(NodeId::NONE, "r", 0, plumb_core::NodeFlags::DIR);
    let f = t.push_str(r, "old.bin", 4096, plumb_core::NodeFlags::empty());
    let now = plumb_core::age::now_secs();
    t.set_mtime(f, std::num::NonZeroU32::new(now - 3 * 365 * 86_400));
    t.set_now(now);
    let t = t.finish().unwrap();
    assert_eq!((ViewKind::Disk.spec().score_text)(&t, f), fmt::size(4096));
    let mut p = plumb_core::TreeBuilder::new();
    let root = p.push_str(NodeId::NONE, "Processes", 0, plumb_core::NodeFlags::DIR);
    let mut p = p.finish().unwrap();
    p.add_score(root, 12);
    assert_eq!((ViewKind::Processes.spec().score_text)(&p, root).to_string(), "1.2%");
}

/// An app with a Processes view as its third view, showing one process `ping.exe` (pid 77) under the root.
fn process_app() -> (App, NodeId) {
    use plumb_core::procs::{Metric, Procs, Sample};
    let mut a = app();
    a.views.push(ViewInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
    a.begin_scan(2);
    let (mut procs, mut tree) = Procs::new(Metric::Memory, 4, 1 << 30);
    procs.apply(&mut tree, &[Sample { pid: 77, parent: 1, created: 5, name: "ping.exe".into(), memory: 4096, private: 2048, cpu_100ns: 0, io_bytes: 0 }], 0);
    let id = tree.find("Processes\\ping.exe").expect("the process is under the root");
    a.finish_scan(tree, "x".into());
    (a, id)
}

#[test]
fn a_process_is_coloured_by_where_its_executable_lives() {
    let sys = r"C:\Windows";
    assert_eq!(location_class(Some(r"C:\Windows\System32\svchost.exe"), sys), Class::Executable);
    assert_eq!(location_class(Some(r"c:\windows\explorer.exe"), sys), Class::Executable);
    assert_eq!(location_class(Some(r"C:\WindowsApps\x.exe"), sys), Class::Other, "a sibling folder is not the Windows folder");
    assert_eq!(location_class(Some(r"C:\Program Files (x86)\A\a.exe"), sys), Class::Document);
    assert_eq!(location_class(Some(r"C:\Users\me\AppData\Local\b.exe"), sys), Class::Code);
    assert_eq!(location_class(Some(r"D:\tools\c.exe"), sys), Class::Other);
    assert_eq!(location_class(None, sys), Class::Other);
    assert_eq!((ViewKind::Processes.spec().class_label)(Class::Code), "User profile");
    assert_eq!((ViewKind::Disk.spec().class_label)(Class::Code), "Code");
}

#[test]
fn clicking_the_metric_button_cycles_memory_private_cpu_io_and_back() {
    let (mut a, _) = process_app();
    a.layout.map_on = true;
    let click = |a: &mut App| {
        let b = Layout { procs: true, ..a.layout.clone() }.metric_button();
        a.handle(Input::Click(b.x + 5.0, b.y + 5.0, Default::default()))
    };
    assert_eq!(a.slot.proc_metric, Metric::Memory);
    assert_eq!(click(&mut a), Effect::Metric(Metric::Private));
    a.slot.proc_metric = Metric::Private; // the window applies the effect
    assert_eq!(click(&mut a), Effect::Metric(Metric::Cpu));
    a.slot.proc_metric = Metric::Cpu; // the window applies the effect
    assert_eq!(click(&mut a), Effect::Metric(Metric::Io));
    a.slot.proc_metric = Metric::Io;
    assert_eq!(click(&mut a), Effect::Metric(Metric::Memory));
    assert_eq!(ViewKind::Processes.spec().titles(Metric::Io)[3], "I/O");
    assert_eq!(ViewKind::Processes.spec().titles(Metric::Private)[3], "Private");
    assert_eq!((ViewKind::Processes.spec().size_text)(Metric::Private, 2048), fmt::size(2048));
    assert_eq!((ViewKind::Processes.spec().size_text)(Metric::Cpu, 123), "12.3%");
    assert_eq!((ViewKind::Processes.spec().size_text)(Metric::Io, 2048), fmt::rate(2048));
    assert_eq!((ViewKind::Disk.spec().size_text)(Metric::Cpu, 2048), fmt::size(2048), "a disk ignores the metric");
}

#[test]
fn every_metric_has_a_label_and_a_one_line_definition() {
    for m in [Metric::Memory, Metric::Private, Metric::Cpu, Metric::Io] {
        assert!(!metric_label(m).is_empty() && !metric_help(m).is_empty(), "{m:?}");
    }
    assert!(metric_help(Metric::Private).contains("not shared"));
    assert!(metric_help(Metric::Memory).contains("working set"));
}

#[test]
fn who_is_using_this_is_offered_for_a_file_on_a_drive_only() {
    let a = scanned();
    let tree = a.slot.tree.as_ref().unwrap();
    let dir = a.slot.model.as_ref().unwrap().rows()[0].id;
    let mut file = dir;
    while tree.flags[file].contains(NodeFlags::DIR) {
        file = tree.children(file).next().expect("every folder in the tree has something in it");
    }
    assert!(a.menu_target(file).unwrap().3, "a file");
    assert!(!a.menu_target(dir).unwrap().3, "a folder has no holders to ask about");
}

#[test]
fn selecting_a_process_requests_its_files_once_it_settles() {
    let (mut a, id) = process_app();
    a.slot.model.as_mut().unwrap().select(id);
    assert!(a.held_sync(), "the selection is a process");
    assert!(!a.held_sync(), "the same selection again changes nothing");
    assert_eq!(a.held_request().map(|r| r.1), Some(77));
    assert_eq!(a.held_request(), None, "asked once");
    assert!(a.held_text().unwrap().0.contains("ping.exe (PID 77)"), "{:?}", a.held_text());
}

#[test]
fn a_lookup_in_flight_survives_slot_swaps_and_epochs_never_repeat() {
    let (mut a, id) = process_app();
    a.slot.model.as_mut().unwrap().select(id);
    a.held_sync();
    let (first, _) = a.held_request().unwrap();
    let mut other = Slot::default();
    a.swap_slot(&mut other); // a parked view's journal event swaps a slot in and out
    a.swap_slot(&mut other);
    a.held_done(first, Ok(HandleList { files: vec!["C:/x".into()], skipped: 0 }));
    assert!(a.held_text().unwrap().0.ends_with("1 file"), "{:?}", a.held_text());
    let root = a.slot.model.as_ref().unwrap().rows()[0].id;
    a.slot.model.as_mut().unwrap().select(root);
    a.held_sync();
    a.slot.model.as_mut().unwrap().select(id);
    a.held_sync();
    let (second, _) = a.held_request().unwrap();
    assert!(second > first, "{second} after {first}");
}

#[test]
fn a_stale_result_for_a_previous_selection_is_ignored() {
    let (mut a, id) = process_app();
    a.slot.model.as_mut().unwrap().select(id);
    a.held_sync();
    let (old, _) = a.held_request().unwrap();
    let root = a.slot.model.as_ref().unwrap().rows()[0].id;
    a.slot.model.as_mut().unwrap().select(root); // the root: pid 0, no pane
    assert!(!a.held_sync());
    assert!(a.held_text().is_none());
    a.held_done(old, Ok(HandleList { files: vec!["C:/x".into()], skipped: 0 }));
    assert!(a.held_text().is_none(), "the answer for the old selection is dropped");
}

#[test]
fn the_pane_reports_files_denied_and_overflow() {
    let (mut a, id) = process_app();
    a.slot.model.as_mut().unwrap().select(id);
    a.held_sync();
    let (g, _) = a.held_request().unwrap();
    a.held_done(g, Err(HandleError::Denied));
    assert!(a.held_text().unwrap().0.contains("run plumb as administrator"));
    let (mut a, id) = process_app();
    a.slot.model.as_mut().unwrap().select(id);
    a.held_sync();
    let (g, _) = a.held_request().unwrap();
    let files: Vec<String> = (0..9).map(|i| format!("C:/f{i}")).collect();
    a.held_done(g, Ok(HandleList { files, skipped: 2 }));
    let (head, lines) = a.held_text().unwrap();
    assert!(head.ends_with("9 files (1\u{2013}6), 2 not readable"), "{head}");
    assert_eq!((lines.len(), lines.last().unwrap().as_str()), (crate::layout::HELD_ROWS, "C:/f5"));
    // The wheel over the pane scrolls it, clamped to the list; elsewhere it scrolls the rows; a new selection starts over.
    let pane = a.layout.held_pane();
    let (px, py) = (pane.x + 10.0, pane.y + 10.0);
    a.handle(Input::Wheel(2, px, py));
    let (head, lines) = a.held_text().unwrap();
    assert_eq!((head.contains("(3\u{2013}8)"), lines[0].as_str(), a.slot.scroll), (true, "C:/f2", 0), "{head}");
    a.handle(Input::Wheel(100, px, py));
    assert_eq!(a.held_text().unwrap().1[0], "C:/f3", "clamped so the last line is the last file");
    a.handle(Input::Wheel(-100, px, py));
    assert_eq!(a.held_text().unwrap().1[0], "C:/f0");
    a.handle(Input::Wheel(1, px, 5.0));
    assert_eq!(a.held_text().unwrap().1[0], "C:/f0", "over the rows it does not scroll the pane");
    a.handle(Input::Wheel(2, px, py));
    a.held.pid = None;
    a.held_sync();
    assert_eq!(a.held.scroll, 0, "a new selection starts at the top");
    // A process with nothing open says so in its title and has no lines.
    let (mut a, id) = process_app();
    a.slot.model.as_mut().unwrap().select(id);
    a.held_sync();
    let (g, _) = a.held_request().unwrap();
    a.held_done(g, Ok(HandleList { files: Vec::new(), skipped: 0 }));
    let (head, lines) = a.held_text().unwrap();
    assert!(head.ends_with("0 files") && lines.is_empty(), "{head}");
}

/// One Processes snapshot as `set_procs` sees it: pid 77 with `handles` open handles.
fn snapshot_with(a: &mut App, handles: u32) {
    let info = HashMap::from([(77u32, ProcInfo { path: None, created: 5, handles, suspended: false, elevated: false })]);
    a.set_procs(2, info, (16 << 30, 4 << 30));
}

/// `process_app` with pid 77 selected, its first lookup asked and answered with `files`.
fn held_ready(files: &[&str]) -> App {
    let (mut a, id) = process_app();
    snapshot_with(&mut a, 100);
    a.slot.model.as_mut().unwrap().select(id);
    a.held_sync();
    let (epoch, _) = a.held_request().unwrap();
    a.held_done(epoch, Ok(HandleList { files: files.iter().map(|s| s.to_string()).collect(), skipped: 0 }));
    a
}

#[test]
fn a_changed_handle_count_refreshes_the_list_after_ten_snapshots() {
    let mut a = held_ready(&["C:/a"]);
    for _ in 0..HELD_REFRESH_SNAPSHOTS - 1 {
        snapshot_with(&mut a, 120);
        assert_eq!(a.held_take_refresh(), None, "changed, but not ten snapshots since the lookup");
    }
    snapshot_with(&mut a, 120);
    let (epoch, pid) = a.held_take_refresh().expect("ten snapshots and a changed count");
    assert_eq!(pid, 77);
    assert_eq!(a.held_take_refresh(), None, "asked once");
    assert_eq!(a.held_text().unwrap().1, vec!["C:/a".to_string()], "the old list stays until the answer");
    a.held_done(epoch, Ok(HandleList { files: vec!["C:/b".into()], skipped: 0 }));
    assert_eq!(a.held_text().unwrap().1, vec!["C:/b".to_string()]);
}

#[test]
fn an_unchanged_handle_count_never_refreshes() {
    let mut a = held_ready(&["C:/a"]);
    for _ in 0..3 * HELD_REFRESH_SNAPSHOTS {
        snapshot_with(&mut a, 100);
        assert_eq!(a.held_take_refresh(), None);
    }
}

#[test]
fn a_busy_process_is_looked_up_at_most_once_per_ten_snapshots_and_never_twice_at_once() {
    let mut a = held_ready(&["C:/a"]);
    let mut asked = 0;
    for n in 0..4 * HELD_REFRESH_SNAPSHOTS {
        snapshot_with(&mut a, 200 + n); // a different count every snapshot
        if let Some((epoch, _)) = a.held_take_refresh() {
            asked += 1;
            snapshot_with(&mut a, 500 + n);
            assert_eq!(a.held_take_refresh(), None, "one is already in flight");
            a.held_done(epoch, Ok(HandleList::default()));
        }
    }
    assert!(asked <= 4, "{asked} lookups in {} snapshots", 4 * HELD_REFRESH_SNAPSHOTS);
    assert!(asked >= 3, "{asked}: a changing count must keep refreshing");
}

#[test]
fn clicking_the_selected_row_again_refreshes_at_once() {
    let mut a = held_ready(&["C:/a"]);
    a.held_mark_stale(); // what a click on the selected row does
    let (epoch, pid) = a.held_take_refresh().expect("a re-click asks again without waiting for ten snapshots");
    assert_eq!(pid, 77);
    a.held_done(epoch, Ok(HandleList { files: vec!["C:/c".into()], skipped: 0 }));
    assert_eq!(a.held_text().unwrap().1, vec!["C:/c".to_string()]);
}

#[test]
fn a_refresh_keeps_the_scroll_clamped_and_a_late_answer_for_another_selection_is_dropped() {
    let many: Vec<String> = (0..20).map(|i| format!("C:/f{i:02}")).collect();
    let refs: Vec<&str> = many.iter().map(String::as_str).collect();
    let mut a = held_ready(&refs);
    a.held_scroll(100); // as far down as it goes
    a.held_mark_stale();
    let (epoch, _) = a.held_take_refresh().unwrap();
    let shorter = HandleList { files: many[..crate::layout::HELD_ROWS + 1].to_vec(), skipped: 0 };
    a.held_done(epoch, Ok(shorter));
    let (_, lines) = a.held_text().unwrap();
    assert_eq!((lines.len(), lines[0].as_str()), (crate::layout::HELD_ROWS, "C:/f01"), "the offset is clamped to the shorter list");
    // a refresh answer arriving after the selection moved is dropped
    let mut b = held_ready(&["C:/x"]);
    b.held_mark_stale();
    let (e2, _) = b.held_take_refresh().unwrap();
    let root = b.slot.model.as_ref().unwrap().rows()[0].id;
    b.slot.model.as_mut().unwrap().select(root);
    b.held_sync();
    b.held_done(e2, Ok(HandleList { files: vec!["C:/y".into()], skipped: 0 }));
    assert!(b.held_text().is_none(), "no pane for the root, and the old answer did not revive one");
}

#[test]
fn a_denied_or_failed_lookup_is_not_refreshed() {
    let (mut a, id) = process_app();
    snapshot_with(&mut a, 100);
    a.slot.model.as_mut().unwrap().select(id);
    a.held_sync();
    let (epoch, _) = a.held_request().unwrap();
    a.held_done(epoch, Err(HandleError::Denied));
    for _ in 0..2 * HELD_REFRESH_SNAPSHOTS {
        snapshot_with(&mut a, 999);
        assert_eq!(a.held_take_refresh(), None);
    }
    a.held_mark_stale();
    assert_eq!(a.held_take_refresh(), None, "nothing to refresh when it could not be read");
}

#[test]
fn a_process_tip_names_its_metric_and_its_scope() {
    let (a, id) = process_app(); // a leaf: no children
    let tree = a.slot.tree.as_ref().unwrap();
    let own = Tile { node: id, rect: plumb_core::Rect::default(), depth: 0, class: Class::Other, kind: Kind::Frame, bytes: 4096, count: 1, mtime: None };
    let tip = proc_tip_str(&a, tree, &own);
    assert!(tip.contains("memory in this process; own CPU"), "{tip}");
    assert!(!tip.contains("children"), "a process with none does not mention them: {tip}");
    for (m, noun) in [(Metric::Memory, "memory"), (Metric::Private, "private"), (Metric::Cpu, "CPU"), (Metric::Io, "I/O")] {
        assert_eq!(metric_noun(m), noun);
    }
}

#[test]
fn the_rest_tile_of_a_process_view_counts_processes_and_names_a_merged_own_size() {
    let (mut a, id) = process_app(); // ping.exe: 4096 own bytes under the root
    let rest = |count| Tile { node: id, rect: plumb_core::Rect::default(), depth: 0, class: Class::Other, kind: Kind::Rest, bytes: 4096, count, mtime: None };
    let tree = a.slot.tree.take().unwrap();
    let tip = |a: &App, t: &Tile| proc_tip_str(a, &tree, t);
    assert_eq!(tip(&a, &rest(1)), format!("its own memory, {}", fmt::size(4096)), "only its own size merged");
    assert_eq!(tip(&a, &rest(3)), format!("2 smaller processes and its own memory, {}", fmt::size(4096)));
    a.map_tiles = vec![Tile { kind: Kind::Own, count: 1, ..rest(1) }]; // the own tile is drawn: the merged items are processes
    assert_eq!(tip(&a, &rest(1)), format!("1 smaller process, {}", fmt::size(4096)));
    assert_eq!(tip(&a, &rest(2)), format!("2 smaller processes, {}", fmt::size(4096)));
}

#[test]
fn a_process_view_cannot_mark() {
    let (a, id) = process_app();
    assert_eq!(a.mark_state(id), MarkState::Refused("it is a process"));
}

#[test]
fn the_process_status_line_totals_what_the_metric_counts() {
    let (mut a, _) = process_app();
    a.set_procs(2, HashMap::from([(77u32, ProcInfo::default())]), (16 << 30, 3 << 30));
    assert!(status_str(&a, true).ends_with("1 process, sized by memory"), "{}", status_str(&a, true));
    assert_eq!(a.views[2].free, 3 << 30, "the chip follows the machine's free memory");
}

#[test]
fn a_process_row_names_its_pid_and_its_menu_acts_on_its_executable() {
    let (mut a, id) = process_app();
    a.slot.model.as_mut().unwrap().select(id);
    assert!(a.selection().unwrap().starts_with("ping.exe  (PID 77)"), "{:?}", a.selection());
    assert_eq!(a.menu_target(id).map(|t| t.1), Some(String::new()), "no executable known: a menu with no path");
    a.set_procs(2, HashMap::from([(77, ProcInfo { path: Some("C:\\Windows\\ping.exe".into()), created: 0, handles: 0, suspended: false, elevated: false })]), (1, 1));
    let (raw, path, mark, who) = a.menu_target(id).unwrap();
    assert!(!who, "a process has no holders to ask about");
    assert_eq!((raw.as_str(), path.as_str()), ("", "C:\\Windows\\ping.exe"));
    assert_eq!(mark, MarkState::Refused("it is a process"));
}

#[test]
fn a_set_of_processes_has_nothing_to_copy_and_cannot_be_marked() {
    let (mut a, id) = process_app();
    let root = a.slot.model.as_ref().unwrap().rows()[0].id;
    {
        let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_mut().unwrap());
        m.expand(t, root);
        m.select(root);
        m.extend_to(id);
    }
    let menu = a.row_menu(root).unwrap();
    assert_eq!(menu.count, 2);
    assert!(menu.paths.is_empty(), "no executable path is known: no Copy paths entry");
    assert_eq!(menu.mark, MarkState::Refused("it is a process"));
    assert_eq!(a.copy_text(), None);
}

#[test]
fn suspend_is_offered_for_a_process_and_resume_once_every_thread_of_it_is_suspended() {
    let (mut a, id) = process_app();
    assert!(a.pause_target(id).is_none(), "not in the snapshot yet: no creation time to act by");
    let info = |suspended| HashMap::from([(77, ProcInfo { path: Some("C:\\Windows\\ping.exe".into()), created: 5, handles: 0, suspended, elevated: false })]);
    a.set_procs(2, info(false), (1, 1));
    assert_eq!(a.pause_target(id), Some((Victim { pid: 77, created: 5, name: "ping.exe".into() }, false)));
    a.set_procs(2, info(true), (1, 1));
    assert!(a.pause_target(id).is_some_and(|t| t.1), "Resume is offered");
    a.slot.model.as_mut().unwrap().select(id);
    assert!(!a.selection().unwrap().contains("suspended"), "the row's tag says it, the line does not: {:?}", a.selection());
    let root = a.slot.model.as_ref().unwrap().rows()[0].id;
    assert!(a.pause_target(root).is_none(), "the root is pid 0");
}

#[test]
fn end_process_is_offered_for_a_process_and_asks_before_it_acts() {
    let (mut a, id) = process_app();
    let v = |pid, name: &str| Victim { pid, created: 5, name: name.into() };
    assert!(a.tree_targets(id).is_none(), "the process is not in the snapshot yet: no creation time to end it by");
    a.set_procs(2, HashMap::from([(77, ProcInfo { path: None, created: 5, handles: 0, suspended: false, elevated: false })]), (1, 1));
    assert_eq!(a.tree_targets(id), Some(vec![v(77, "ping.exe")]));
    let root = a.slot.model.as_ref().unwrap().rows()[0].id;
    assert!(a.tree_targets(root).is_none(), "the root is pid 0");
    a.ask(Modal::end(vec![v(77, "ping.exe")]));
    assert_eq!(a.answer(Pick::No), Effect::None);
    a.ask(Modal::end(vec![v(77, "ping.exe")]));
    assert_eq!(a.answer(Pick::Yes), Effect::End(vec![v(77, "ping.exe")]));
    assert!(a.modal().is_none());
}

#[test]
fn end_process_tree_lists_the_process_then_its_descendants_top_down() {
    use plumb_core::procs::{Metric, Procs, Sample};
    let mut a = app();
    a.views.push(ViewInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
    a.begin_scan(2);
    let (mut procs, mut tree) = Procs::new(Metric::Memory, 4, 1 << 30);
    let s = |pid, parent, name: &str| Sample { pid, parent, created: 5, name: name.into(), memory: 4096, private: 2048, cpu_100ns: 0, io_bytes: 0 };
    procs.apply(&mut tree, &[s(77, 1, "a.exe"), s(78, 77, "b.exe"), s(79, 78, "c.exe"), s(80, 77, "d.exe"), s(90, 1, "other.exe")], 0);
    let top = tree.find(r"Processes\a.exe").expect("the top process");
    a.finish_scan(tree, "x".into());
    let info = |pids: &[u32]| pids.iter().map(|&p| (p, ProcInfo { path: None, created: 5, handles: 0, suspended: false, elevated: false })).collect::<HashMap<_, _>>();
    a.set_procs(2, info(&[77, 78, 79, 80, 90]), (1, 1));
    let pids = |a: &App| a.tree_targets(top).unwrap().iter().map(|v| v.pid).collect::<Vec<_>>();
    let all = pids(&a);
    assert_eq!(all[0], 77, "the process first");
    let at = |p: u32| all.iter().position(|&x| x == p).unwrap();
    assert!(at(78) < at(79), "a child before its own child");
    assert_eq!(all.len(), 4);
    assert!(!all.contains(&90), "not another top-level process");
    a.set_procs(2, info(&[77, 78, 80, 90]), (1, 1));
    let mut left_out = pids(&a);
    left_out.sort();
    assert_eq!(left_out, [77, 78, 80], "a descendant missing from the snapshot is left out");
}

#[test]
fn suspend_and_resume_tree_act_on_the_matching_processes_and_hide_when_they_would_repeat_the_row() {
    use plumb_core::procs::{Metric, Procs, Sample};
    let mut a = app();
    a.views.push(ViewInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
    a.begin_scan(2);
    let (mut procs, mut tree) = Procs::new(Metric::Memory, 4, 1 << 30);
    let s = |pid, parent, name: &str| Sample { pid, parent, created: 5, name: name.into(), memory: 4096, private: 2048, cpu_100ns: 0, io_bytes: 0 };
    procs.apply(&mut tree, &[s(77, 1, "a.exe"), s(78, 77, "b.exe"), s(79, 77, "c.exe")], 0);
    let top = tree.find(r"Processes\a.exe").expect("the top process");
    a.finish_scan(tree, "x".into());
    let with = |a: &mut App, suspended: &[u32]| {
        let info = [77, 78, 79].map(|p| (p, ProcInfo { path: None, created: 5, handles: 0, suspended: suspended.contains(&p), elevated: false }));
        a.set_procs(2, info.into_iter().collect(), (1, 1));
        let pids = |l: Vec<Victim>| { let mut p = l.iter().map(|v| v.pid).collect::<Vec<_>>(); p.sort(); p };
        let (suspend, resume) = a.pause_trees(top);
        (pids(suspend), pids(resume))
    };
    assert_eq!(with(&mut a, &[]), (vec![77, 78, 79], vec![]), "nothing suspended: Suspend acts on all three");
    assert_eq!(with(&mut a, &[78]), (vec![77, 79], vec![77, 78, 79]), "a mixed tree offers both: Suspend on the running ones, Resume on all");
    assert_eq!(with(&mut a, &[77, 78, 79]), (vec![], vec![77, 78, 79]), "all suspended: Resume acts on all three");
    assert_eq!(with(&mut a, &[77]), (vec![78, 79], vec![77, 78, 79]), "the row suspended, its children not: both are offered");
}

#[test]
fn a_process_view_sorts_cpu_and_io_by_aux_and_shows_them_as_rates() {
    assert_eq!(ViewKind::Processes.spec().titles(Metric::Memory)[4..], ["CPU", "I/O"]);
    assert_eq!((ViewKind::Processes.spec().cols[1].sort, ViewKind::Processes.spec().cols[4].sort, ViewKind::Processes.spec().cols[5].sort), (SortKey::Score, SortKey::Aux(0), SortKey::Aux(1)));
    let mut b = plumb_core::TreeBuilder::new();
    let r = b.push_str(NodeId::NONE, "r", 0, plumb_core::NodeFlags::DIR);
    let mut t = b.finish().unwrap();
    assert_eq!((ViewKind::Processes.spec().last_two)(&t, r), (fmt::tenths(0), Some(fmt::rate(0))), "no aux yet");
    t.set_aux(r, [255, 2048, 99]);
    assert_eq!((ViewKind::Processes.spec().last_two)(&t, r), (fmt::tenths(255), Some(fmt::rate(2048))));
}
use crate::layout::{CHIP_GAP, CHIP_W, HEADER_H, PICKER_H, ROW_H};
use plumb_core::synth::kary_builder;

fn app() -> App {
    let views = vec![
        ViewInfo { root: "C:\\".into(), total: 1000, free: 250, kind: ViewKind::Disk },
        ViewInfo { root: "D:\\".into(), total: 1000, free: 900, kind: ViewKind::Disk },
    ];
    App::new(Layout::new(800.0, 600.0), views, "test".into())
}

#[test]
fn the_status_line_counts_a_set_and_says_nothing_when_nothing_is_picked() {
    let mut a = scanned();
    {
        let m = a.slot.model.as_mut().unwrap();
        let r: Vec<NodeId> = m.rows().iter().map(|r| r.id).collect();
        m.select(r[0]);
        m.toggle_pick(r[0]);
    }
    assert_eq!(a.selection(), None, "focus without a pick");
    a.handle(Input::Edit(Edit::SelectAll));
    let line = a.selection().unwrap();
    let n = a.slot.model.as_ref().unwrap().picked_count();
    let tree = a.slot.tree.as_ref().unwrap();
    let root = fmt::size(tree.subtree_size[tree.roots[0]]);
    assert!(n > 1 && line == format!("{n} selected, {root}"), "the root and every row under it count once: {line:?}");
    // Two siblings add up.
    let (x, y) = { let t = a.slot.tree.as_ref().unwrap(); let kids: Vec<NodeId> = t.children(t.roots[0]).take(2).collect(); (kids[0], kids[1]) };
    let want = fmt::size(a.slot.tree.as_ref().unwrap().subtree_size[x] + a.slot.tree.as_ref().unwrap().subtree_size[y]);
    {
        let m = a.slot.model.as_mut().unwrap();
        m.select(x);
        m.toggle_pick(y);
    }
    let line = a.selection().unwrap();
    assert!(line.ends_with(&format!(", {want}")) && line.starts_with("2 selected"), "{line:?} wants {want}");
}

fn scanned() -> App {
    let mut a = app();
    a.begin_scan(0);
    a.finish_scan(kary_builder(300, 3).finish().unwrap(), "done".into());
    a
}

#[test]
fn chips_are_sized_to_their_labels() {
    let mut a = app();
    a.views[1].root = "D:\\Mounts\\A long mount name\\".into();
    a.fit_chips(&|s| s.chars().count() as f32 * 7.0);
    let w: Vec<f32> = (0..2).map(|i| a.layout.chip(i).w).collect();
    let labels: Vec<usize> = a.chips().iter().map(|c| c.label.chars().count()).collect();
    assert_eq!(w, vec![(labels[0] as f32 * 7.0 + 16.0).ceil(), (labels[1] as f32 * 7.0 + 16.0).ceil()]);
    assert!(w[1] > w[0], "a longer label gets a wider chip");
    assert_eq!(a.handle(Input::Click(a.layout.chip(1).x + 2.0, 10.0, Default::default())), Effect::Scan(1), "the wide chip is clickable along its width");
}

#[test]
fn a_mount_point_is_titled_by_its_path_and_chipped_by_its_name() {
    let view = |root: &str| ViewInfo { root: root.into(), total: 1, free: 1, kind: ViewKind::Disk };
    let (c, m) = (view("C:\\"), view("D:\\Mounts\\Data\\"));
    assert_eq!((c.title(), c.chip_name()), ("C:", "C:"));
    assert_eq!((m.title(), m.chip_name()), ("D:\\Mounts\\Data", "Data"));
}

/// The status line minus the selection details that lead it.
fn status_tail(a: &App) -> String {
    let s = status_str(a, true);
    match a.selection() {
        Some(sel) => s.strip_prefix(&sel).map_or(s.clone(), |t| t.strip_prefix("   |   ").unwrap_or(t).to_string()),
        None => s,
    }
}

fn row_y(i: usize) -> f32 {
    PICKER_H + HEADER_H + i as f32 * ROW_H + 1.0
}

#[test]
fn clicking_a_chip_requests_a_scan_and_another_drive_replaces_one_in_flight() {
    let mut a = app();
    assert_eq!(a.handle(Input::Click(10.0 + CHIP_W + CHIP_GAP, 10.0, Default::default())), Effect::Scan(1));
    a.begin_scan(1);
    assert_eq!(a.handle(Input::Click(10.0, 10.0, Default::default())), Effect::Scan(0), "another view replaces the scan");
    assert_eq!(a.handle(Input::Click(10.0 + CHIP_W + CHIP_GAP, 10.0, Default::default())), Effect::None, "the same view is left alone");
}

#[test]
fn layout_now_follows_the_app_state() {
    let mut a = app();
    let l = a.layout_now();
    assert!(!l.procs && !l.banner, "a disk view has neither the Processes flags nor a banner");
    a.set_live(LiveStatus::Stale("x".into()));
    assert!(a.layout_now().banner);
    assert_eq!(a.layout_now().cancel, a.cancelable());
    assert_eq!(a.layout_now().toast, a.layout.toast);
    let (p, _) = process_app();
    assert!(p.layout_now().procs);
}

#[test]
fn the_cancel_button_and_escape_cancel_a_scan_only_while_one_runs() {
    let mut a = app();
    assert_eq!(a.handle(Input::Escape), Effect::None);
    a.begin_scan(0);
    let b = Layout { cancel: true, ..a.layout.clone() }.cancel_button();
    assert_eq!(a.handle(Input::Click(b.x + 5.0, b.y + 5.0, Default::default())), Effect::Cancel);
    assert_eq!(a.handle(Input::Escape), Effect::Cancel);
    // Esc that clears the filter does not cancel.
    a.slot.filter.ui.focus = true;
    assert_ne!(a.handle(Input::Escape), Effect::Cancel);
    a.cancel_scan();
    assert!(a.message().starts_with("Scan cancelled") && a.slot.tree.is_none() && !a.cancelable());
    assert_eq!(a.handle(Input::Escape), Effect::None);
}

#[test]
fn switching_drives_keeps_each_drives_tree_selection_and_scan_apart() {
    let mut a = scanned();
    let sel = a.slot.model.as_ref().and_then(|m| m.selected());
    a.slot.scan = Scan::Refreshing;
    let mut away = Slot::default();
    a.switch_slot(&mut away);
    assert!(a.slot.tree.is_none() && a.slot.model.is_none() && !a.cancelable() && !a.slot.refreshing());
    assert!(away.has_tree());
    a.begin_scan(1);
    assert!(matches!(a.slot.scan, Scan::Running { view: 1, .. }) && a.cancelable());
    let mut other = Slot::default();
    a.switch_slot(&mut other); // the running scan goes away, then the first view comes back
    a.switch_slot(&mut away);
    assert!(a.slot.tree.is_some() && a.slot.refreshing() && a.cancelable());
    assert_eq!(a.slot.model.as_ref().and_then(|m| m.selected()), sel);
    assert!(matches!(other.scan, Scan::Running { view: 1, .. }) && !other.has_tree());
}

#[test]
fn switching_drives_takes_the_filter_off_the_rows_and_refilters_the_other_drive() {
    let mut a = scanned();
    let all = a.total_rows();
    a.slot.filter.ui.text = "zzz".into();
    a.filter_changed();
    let job = a.start_filter().expect("a run");
    let h = job.names.scan(&job.matcher, &job.cancel, job.generation).unwrap();
    a.filter_ready(job.generation, h);
    assert!(a.filter_installed());
    let mut away = Slot::default();
    a.switch_slot(&mut away);
    assert!(!a.filter_installed() && a.filter_ui().text == "zzz");
    a.switch_slot(&mut away);
    assert!(!a.filter_installed() && a.filter_dirty()); // asks for a fresh run on the returning tree
    assert_eq!(a.total_rows(), all);
}

#[test]
fn the_scan_line_counts_files_and_bytes_only_until_the_tree_lists_them() {
    let mut a = app();
    a.begin_scan(0);
    a.slot.scan = Scan::Running { view: 0, dirs: 3, files: 40, bytes: 2048, paused: false };
    assert!(a.message().ends_with("3 folders, 40 files, 2.0 KB"), "{}", a.message());
    a.slot.tree = Some(kary_builder(30, 3).finish().unwrap());
    assert!(a.message().ends_with("3 folders"), "the root row has the rest: {}", a.message());
}

#[test]
fn a_first_scan_replaces_a_rescan_and_a_cancel_ends_either() {
    let mut a = scanned();
    a.slot.scan = Scan::Refreshing;
    a.begin_scan(1);
    assert!(matches!(a.slot.scan, Scan::Running { view: 1, paused: false, .. }) && !a.slot.refreshing() && a.cancelable());
    a.cancel_scan();
    assert!(matches!(a.slot.scan, Scan::Cancelled) && !a.cancelable() && a.slot.tree.is_none());
    a.cancel_scan();
    assert!(matches!(a.slot.scan, Scan::Cancelled), "cancelling nothing changes nothing");
}

#[test]
fn cancelling_a_background_rescan_keeps_the_tree() {
    let mut a = scanned();
    a.slot.scan = Scan::Refreshing;
    assert!(a.cancelable());
    a.cancel_scan();
    assert!(a.slot.tree.is_some() && !a.slot.refreshing() && matches!(a.slot.scan, Scan::Idle));
}

#[test]
fn admin_button_only_works_when_shown() {
    let mut a = app();
    let b = a.layout.admin_button();
    let (x, y) = (b.x + 5.0, b.y + 5.0);
    assert_eq!(a.handle(Input::Click(x, y, Default::default())), Effect::None);
    a.can_elevate = true;
    assert_eq!(a.handle(Input::Click(x, y, Default::default())), Effect::Elevate);
}

#[test]
fn a_chip_shows_what_its_drive_holds_unless_it_is_on_screen() {
    let mut a = app();
    a.marks = vec![Mark::Kept, Mark::Scanning];
    let m = |a: &App| a.chips().iter().map(|c| c.mark).collect::<Vec<_>>();
    assert_eq!(m(&a)[..2], [Mark::Kept, Mark::Scanning]);
    a.active = Some(0);
    assert_eq!(m(&a)[..2], [Mark::None, Mark::Scanning]);
}

#[test]
fn chips_show_free_space_and_used_fraction() {
    let c = app().chips();
    assert_eq!(c[0].label, "C:  250 B");
    assert!((c[0].used - 0.75).abs() < 1e-6);
}

#[test]
fn a_preview_tree_is_browsable_and_the_final_tree_keeps_the_view() {
    let mut a = app();
    a.begin_scan(0);
    a.slot.tree = Some(kary_builder(60, 3).finish().unwrap());
    a.preview_installed();
    assert!(matches!(a.slot.scan, Scan::Running { .. }), "the scan is still running");
    assert!(a.slot.model.as_ref().unwrap().rows().len() > 1, "the root is expanded");
    assert!(status_tail(&a).starts_with("Scanning"));
    // The user selects another row, then the exact tree (the same shape) arrives.
    let second = a.slot.model.as_ref().unwrap().rows()[1].id;
    a.slot.model.as_mut().unwrap().select(second);
    let name = a.slot.tree.as_ref().unwrap().path(second);
    a.swap_tree(kary_builder(60, 3).finish().unwrap(), "done".into());
    assert!(matches!(a.slot.scan, Scan::Idle));
    let sel = a.slot.model.as_ref().unwrap().selected().unwrap();
    assert_eq!(a.slot.tree.as_ref().unwrap().path(sel), name, "the selection survives by path");
    assert!(status_tail(&a).starts_with("done"));
}

#[test]
fn a_failed_scan_drops_the_preview() {
    let mut a = app();
    a.begin_scan(0);
    a.slot.tree = Some(kary_builder(10, 2).finish().unwrap());
    a.preview_installed();
    a.fail_scan("boom".into());
    assert!(a.slot.tree.is_none() && a.slot.model.is_none());
}

#[test]
fn a_dropped_preview_tree_clears_the_view_but_not_the_scan() {
    let mut a = app();
    a.begin_scan(0);
    a.slot.tree = Some(kary_builder(10, 2).finish().unwrap());
    a.preview_installed();
    a.slot.tree = None; // what the applier does on a Reset
    a.preview_installed();
    assert!(a.slot.model.is_none());
    assert!(matches!(a.slot.scan, Scan::Running { .. }));
}

#[test]
fn finishing_a_scan_shows_an_expanded_root() {
    let a = scanned();
    let m = a.slot.model.as_ref().unwrap();
    assert!(m.rows().len() > 1);
    assert_eq!(m.selected(), Some(m.rows()[0].id));
    assert!(matches!(a.slot.scan, Scan::Idle));
}

#[test]
fn click_selects_and_double_click_toggles() {
    let mut a = scanned();
    let before = a.slot.model.as_ref().unwrap().rows().len();
    a.handle(Input::Click(20.0, row_y(1), Default::default()));
    let id = a.slot.model.as_ref().unwrap().rows()[1].id;
    assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(id));
    a.handle(Input::DoubleClick(20.0, row_y(1)));
    assert!(a.slot.model.as_ref().unwrap().rows().len() > before);
    a.handle(Input::DoubleClick(20.0, row_y(1)));
    assert_eq!(a.slot.model.as_ref().unwrap().rows().len(), before);
}

#[test]
fn clicking_a_folders_icon_toggles_it_and_a_double_click_on_it_toggles_again() {
    let mut a = scanned();
    let before = a.slot.model.as_ref().unwrap().rows().len();
    let depth = a.slot.model.as_ref().unwrap().rows()[1].depth as f32;
    let icon_x = a.layout.column(0).0 + 8.0 + depth * INDENT + 4.0;
    a.handle(Input::Click(icon_x, row_y(1), Default::default()));
    let opened = a.slot.model.as_ref().unwrap().rows().len();
    assert!(opened > before, "one click on the icon opens the folder");
    a.handle(Input::DoubleClick(icon_x, row_y(1)));
    assert_eq!(a.slot.model.as_ref().unwrap().rows().len(), before, "the second press of a pair toggles again");
    a.handle(Input::Click(icon_x + 200.0, row_y(1), Default::default()));
    assert_eq!(a.slot.model.as_ref().unwrap().rows().len(), before, "a click on the name only selects");
}

#[test]
fn dragging_a_column_edge_widens_the_column_and_release_ends_it() {
    let mut a = scanned();
    let y = a.layout.header().y + 4.0;
    let x = a.layout.column(COL_SCORE).0;
    let w = a.layout.column(COL_SCORE).1;
    assert!(a.over_divider(x, y));
    a.handle(Input::Click(x, y, Default::default()));
    a.handle(Input::Drag(x - 30.0, y));
    assert_eq!(a.layout.column(COL_SCORE).1, w + 30.0);
    assert!(a.over_divider(0.0, 0.0), "while dragging the cursor stays a resize cursor");
    a.handle(Input::Release);
    a.handle(Input::Drag(x - 60.0, y));
    assert_eq!(a.layout.column(COL_SCORE).1, w + 30.0, "released: further moves do nothing");
    assert!(!a.over_divider(0.0, 0.0));
}

#[test]
fn the_unscanned_bytes_are_the_drives_used_space_less_what_the_scan_found_and_vanish_past_it() {
    let mut a = app();
    assert_eq!(a.unscanned(), None, "no scan, nothing to estimate");
    a.begin_scan(0); // drive C: 1000 total, 250 free
    assert_eq!(a.unscanned(), Some(750));
    a.progress(1, 1, 700);
    assert_eq!(a.unscanned(), Some(50));
    a.progress(1, 1, 751);
    assert_eq!(a.unscanned(), None, "the scan counted more than the view says is used");
}

#[test]
fn a_roots_share_is_of_the_drives_used_space_and_only_for_a_disk() {
    let mut a = app();
    assert_eq!(a.used(0), Some(750), "drive C: 1000 total, 250 free");
    assert_eq!(a.used(99), None, "no such view");
    a.views[0].total = 0;
    assert_eq!(a.used(0), None, "a drive that reports no size");
}

#[test]
fn the_icon_buttons_have_tooltips_and_nothing_else_does() {
    let mut a = app();
    a.can_elevate = true;
    let (admin, regex) = (a.layout.admin_button(), a.layout.filter_regex());
    assert_eq!(tip_str(&a, admin.x + 5.0, admin.y + 5.0).as_deref(), Some("Run as administrator"));
    assert_eq!(tip_str(&a, regex.x + 5.0, regex.y + 5.0).as_deref(), Some("Regular expression"));
    assert_eq!(tip_str(&a, a.layout.chip(0).x + 5.0, 10.0).as_deref(), Some("C: 250 B free of 1000 B"), "a chip gives its free space in full");
    assert_eq!(tip_str(&a, a.layout.header().x + 5.0, a.layout.header().y + 5.0), None);
    a.can_elevate = false;
    assert_eq!(tip_str(&a, admin.x + 5.0, admin.y + 5.0), None, "a hidden button has none");
}

#[test]
fn the_icon_button_under_the_pointer_is_hot() {
    let mut a = app();
    a.can_elevate = true;
    let admin = a.layout.admin_button();
    a.handle(Input::Hover(admin.x + 5.0, admin.y + 5.0));
    assert_eq!(a.hot, Some(admin));
    a.handle(Input::Hover(a.layout.chip(0).x + 5.0, 10.0));
    assert_eq!(a.hot, Some(a.layout.chip(0)), "a drive chip lights up too");
    a.handle(Input::Hover(5.0, a.layout.header().y + 5.0));
    assert_eq!(a.hot, Some(a.layout.header_cell(0)), "a header cell lights up: it sorts");
    a.handle(Input::Hover(5.0, a.layout.rows_area().y + 5.0));
    assert_eq!(a.hot, None);
}

#[test]
fn a_press_on_the_hot_control_is_pressed_until_the_button_is_released() {
    let mut a = app();
    let chip = a.layout.chip(0);
    let (x, y) = (chip.x + 5.0, chip.y + 5.0);
    a.handle(Input::Hover(x, y));
    a.handle(Input::Click(x, y, Default::default()));
    assert!(a.pressed);
    a.handle(Input::Release);
    assert!(!a.pressed);
    a.handle(Input::Hover(5.0, a.layout.rows_area().y + 5.0));
    a.handle(Input::Click(5.0, a.layout.rows_area().y + 5.0, Default::default()));
    assert!(!a.pressed, "nothing under the pointer to press");
}

#[test]
fn only_empty_picker_bar_space_is_title_bar() {
    let mut a = app();
    a.can_elevate = true;
    let chip = a.layout.chip(0);
    let admin = a.layout.admin_button();
    assert!(!a.is_caption(chip.x + 5.0, chip.y + 5.0), "a drive chip is a control");
    assert!(!a.is_caption(admin.x + 5.0, admin.y + 5.0), "so is the administrator button");
    assert!(a.is_caption(a.layout.chip(5).x, 3.0), "past the chips the bar is draggable");
    assert!(!a.is_caption(300.0, PICKER_H + 5.0), "below the bar is content");
    a.can_elevate = false;
    assert!(a.is_caption(admin.x + 5.0, admin.y + 5.0), "no button, no control");
}

#[test]
fn a_double_click_on_a_header_sorts_again() {
    let mut a = scanned();
    let h = a.layout.column(COL_SCORE).0 + 20.0;
    let y = a.layout.header().y + 4.0;
    let first = a.sort();
    a.handle(Input::Click(h, y, Default::default()));
    let once = a.sort();
    a.handle(Input::DoubleClick(h, y));
    assert_ne!(first, once);
    assert_ne!(once, a.sort(), "the second press of the pair flips the sort again");
}

#[test]
fn right_click_selects_and_asks_for_a_menu() {
    let mut a = scanned();
    let id = a.slot.model.as_ref().unwrap().rows()[2].id;
    assert_eq!(a.handle(Input::RightClick(20.0, row_y(2))), Effect::Menu(id));
    assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(id));
}

#[test]
fn wheel_and_keys_keep_the_selection_visible_and_scroll_in_range() {
    let mut a = scanned();
    let m = a.slot.model.as_mut().unwrap();
    let t = a.slot.tree.as_ref().unwrap();
    for _ in 0..8 {
        let r = m.rows().iter().find(|r| m.is_expandable(t, r.id) && !m.is_expanded(r.id)).map(|r| r.id);
        if let Some(r) = r { m.expand(t, r); }
    }
    let n = a.slot.model.as_ref().unwrap().rows().len();
    let vis = a.layout.rows_visible();
    assert!(n > vis, "need more rows than fit: {n} vs {vis}");
    a.handle(Input::Wheel(10_000, 5.0, 100.0));
    assert_eq!(a.slot.scroll, n - vis);
    a.handle(Input::Wheel(-10_000, 5.0, 100.0));
    assert_eq!(a.slot.scroll, 0);
    a.handle(Input::Key(Key::End, Default::default()));
    assert_eq!(a.slot.scroll, n - vis);
    a.handle(Input::Key(Key::Home, Default::default()));
    assert_eq!(a.slot.scroll, 0);
}

#[test]
fn header_click_sorts_and_second_click_reverses() {
    let mut a = scanned();
    let name = |a: &App, i: usize| a.slot.tree.as_ref().unwrap().name_string(a.slot.model.as_ref().unwrap().rows()[i].id);
    let hy = PICKER_H + 2.0;
    assert_eq!(a.sort(), (COL_SCORE, true)); // default: Score descending
    a.handle(Input::Click(5.0, hy, Default::default()));
    assert_eq!(a.sort(), (0, false)); // Name ascending
    let first = name(&a, 1);
    a.handle(Input::Click(5.0, hy, Default::default()));
    assert_eq!(a.sort(), (0, true)); // Name descending
    assert_ne!(name(&a, 1), first);
    let score_x = a.layout.column(COL_SCORE).0 + 20.0;
    a.handle(Input::Click(score_x, hy, Default::default()));
    assert_eq!(a.sort(), (COL_SCORE, true), "the Score header sorts by Score, largest first");
    a.handle(Input::Click(score_x, hy, Default::default()));
    assert_eq!(a.sort(), (COL_SCORE, false));
}

#[test]
fn dragging_the_thumb_scrolls() {
    let mut a = scanned();
    let t = a.slot.tree.as_ref().unwrap();
    let m = a.slot.model.as_mut().unwrap();
    for _ in 0..8 {
        let r = m.rows().iter().find(|r| m.is_expandable(t, r.id) && !m.is_expanded(r.id)).map(|r| r.id);
        if let Some(r) = r { m.expand(t, r); }
    }
    let total = m.rows().len();
    let th = a.layout.thumb(total, 0).unwrap();
    a.handle(Input::Hover(795.0, th.y + 2.0));
    assert_eq!(a.with_scene(false, |s| (s.hot, s.pressed)), (Some(th), false), "the thumb lights up under the pointer");
    a.handle(Input::Click(795.0, th.y + 2.0, Default::default()));
    assert!(a.with_scene(false, |s| s.pressed), "and is pressed while dragged");
    a.handle(Input::Drag(795.0, th.y + 2.0 + 100.0));
    assert!(a.slot.scroll > 0);
    a.handle(Input::Release);
    assert!(a.with_scene(false, |s| !s.pressed));
    a.handle(Input::Hover(5.0, th.y));
    assert_eq!(a.with_scene(false, |s| s.hot), None, "off the thumb it is not hot");
    let s = a.slot.scroll;
    a.handle(Input::Drag(795.0, th.y + 300.0));
    assert_eq!(a.slot.scroll, s);
}

#[test]
fn a_stale_banner_shifts_the_table_and_its_button_asks_for_a_rescan() {
    let mut a = scanned();
    let rows = a.layout.rows_visible();
    a.set_live(LiveStatus::Stale("the journal is off".into()));
    assert!(a.layout.banner && a.layout.rows_visible() < rows);
    assert_eq!(a.banner().unwrap(), "live updates stopped: the journal is off; the tree may be out of date");
    let b = a.layout.rescan_button();
    assert_eq!(a.handle(Input::Click(b.x + 2.0, b.y + 2.0, Default::default())), Effect::Rescan(0));
    a.slot.scan = Scan::Refreshing;
    assert_eq!(a.handle(Input::Click(b.x + 2.0, b.y + 2.0, Default::default())), Effect::None);
    a.handle(Input::Resize(900.0, 700.0));
    assert!(a.layout.banner, "a resize keeps the banner");
    a.set_live(LiveStatus::Off);
    assert!(!a.layout.banner && a.banner().is_none());
}

#[test]
fn remembered_columns_and_sort_come_back_per_kind() {
    let mut a = app();
    a.prefs[ViewKind::Disk as usize] = Prefs { sort: (4, false), col_w: [101.0, 140.0, 103.0, 104.0, 105.0] };
    a.prefs[ViewKind::Processes as usize] = Prefs { sort: (5, true), col_w: [201.0, 202.0, 203.0, 204.0, 205.0] };
    let mut b = app();
    b.restore(&a.remembered());
    for k in [ViewKind::Disk, ViewKind::Processes] {
        let (x, y) = (a.prefs[k as usize], b.prefs[k as usize]);
        assert_eq!((x.sort, x.col_w), (y.sort, y.col_w), "{k:?}");
    }
    assert_eq!(b.layout.col_w, a.prefs[0].col_w, "the working widths are the active kind's (a disk when none is)");
}

#[test]
fn nonsense_in_a_state_file_is_ignored() {
    let mut c = app();
    c.restore(&State::parse("[disk.widths]\nscore = nan\npct = 1.0e9\nsize = -5.0\nbogus = 7.0\n[disk.sort]\ncol = \"cpu\"\ndescending = false\n"));
    let p = c.prefs[ViewKind::Disk as usize];
    assert_eq!(p.sort, (COL_SCORE, true), "a column the kind lacks leaves the default sort");
    assert_eq!(p.col_w[0], crate::layout::COL_W[0], "a non-finite width is skipped");
    assert_eq!(p.col_w[1], 4000.0, "an absurd width is clamped");
    assert_eq!(p.col_w[2], MIN_COL_W);
}

#[test]
fn restore_applies_both_kinds_before_any_view_is_active() {
    let mut a = app();
    a.views.push(ViewInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
    a.restore(&State::parse("[disk.sort]\ncol = \"items\"\ndescending = false\n[processes.sort]\ncol = \"io\"\ndescending = true\n[processes.widths]\nio = 111.0\n"));
    assert_eq!(a.sort(), (4, false), "no view is active, so the disk's");
    a.set_active(Some(2));
    assert_eq!((a.sort(), a.layout.col_w[4]), ((5, true), 111.0));
}

#[test]
fn each_view_kind_keeps_its_own_sort_and_widths() {
    let (mut a, _) = process_app();
    assert_eq!((a.kind(), a.sort(), a.layout.col_w), (ViewKind::Processes, (COL_SCORE, true), crate::layout::COL_W));
    let click = |a: &mut App, c: usize| {
        let (x, y) = (a.layout.column(c).0 + 20.0, a.layout.header().y + 4.0);
        a.handle(Input::Click(x, y, Default::default()));
    };
    a.set_active(Some(0));
    click(&mut a, 4);
    a.prefs[ViewKind::Disk as usize].col_w[0] = 150.0;
    a.set_active(Some(0));
    assert_eq!((a.sort(), a.layout.col_w[0]), ((4, true), 150.0));
    a.set_active(Some(2));
    assert_eq!((a.sort(), a.layout.col_w), ((COL_SCORE, true), crate::layout::COL_W), "the Processes view is untouched");
    click(&mut a, 5);
    a.set_active(None);
    assert_eq!((a.sort(), a.layout.col_w[0]), ((4, true), 150.0), "back on the disk");
    a.set_active(Some(2));
    assert_eq!(a.sort(), (5, true));
}

#[test]
fn a_column_drag_then_a_switch_is_remembered() {
    let mut a = app();
    let w0 = a.layout.col_w[COL_PCT - 1];
    a.col_drag = Some((COL_PCT, 500.0, w0));
    a.handle(Input::Drag(450.0, 0.0));
    a.handle(Input::Release);
    a.set_active(Some(1));
    a.set_active(Some(0));
    assert_eq!(a.layout.col_w[COL_PCT - 1], w0 + 50.0);
    assert_eq!(a.remembered().disk.widths["pct"], w0 + 50.0);
    assert_eq!(a.remembered().processes.widths["pct"], crate::layout::COL_W[COL_PCT - 1], "the other kind keeps its default");
}

#[test]
fn a_click_on_a_divider_in_a_small_window_stores_nothing() {
    let mut a = app();
    a.prefs[ViewKind::Disk as usize].col_w = [200.0; 5];
    a.layout.w = 600.0;
    a.set_active(Some(0));
    a.col_drag = Some((1, 500.0, a.layout.col_w[0]));
    a.handle(Input::Release);
    assert_eq!(a.prefs[ViewKind::Disk as usize].col_w, [200.0; 5]);
}

#[test]
fn a_switch_during_a_column_drag_keeps_the_dragged_width() {
    let mut a = app();
    let w0 = a.layout.col_w[COL_PCT - 1];
    a.col_drag = Some((COL_PCT, 500.0, w0));
    a.handle(Input::Drag(450.0, 0.0));
    a.set_active(Some(1)); // a watch event borrows another view's slot (`in_slot`)
    a.set_active(Some(0));
    a.handle(Input::Release);
    assert_eq!(a.prefs[ViewKind::Disk as usize].col_w[COL_PCT - 1], w0 + 50.0);
}

#[test]
fn a_small_window_does_not_shrink_the_stored_widths() {
    let mut a = app();
    a.prefs[ViewKind::Disk as usize].col_w = [200.0; 5];
    a.layout.w = 600.0;
    a.set_active(Some(0));
    assert!(a.layout.col_w.iter().sum::<f32>() < 1000.0, "the narrow window clamps the working copy");
    a.layout.w = 3000.0;
    a.set_active(Some(0));
    assert_eq!(a.layout.col_w, [200.0; 5]);
    assert_eq!(a.prefs[ViewKind::Disk as usize].col_w, [200.0; 5]);
}

#[test]
fn the_status_line_leads_with_the_selected_rows_path_size_and_items() {
    let mut a = scanned();
    let id = a.slot.model.as_ref().unwrap().rows()[1].id;
    a.slot.model.as_mut().unwrap().select(id);
    let tree = a.slot.tree.as_ref().unwrap();
    assert_eq!(status_str(&a, true), format!("{}   |   done", fmt::node_path(tree, id)), "the path alone: the row has the size and items");
}

#[test]
fn the_status_line_shows_the_live_marker_and_refreshing() {
    let mut a = scanned();
    assert_eq!(status_tail(&a), "done");
    a.set_live(LiveStatus::On { last: None });
    assert_eq!(status_tail(&a), "done   |   live");
    a.live_changed("12:03:41".into());
    a.slot.scan = Scan::Refreshing;
    assert_eq!(status_tail(&a), "done   |   live, last change 12:03:41   |   refreshing\u{2026}");
    a.set_live(LiveStatus::Stale("x".into()));
    a.live_changed("12:04:00".into());
    assert!(status_str(&a, true).contains("live updates stopped") && !status_str(&a, true).contains("12:04"));
    assert!(!status_str(&a, false).contains("live updates stopped"), "the banner says it");
}

#[test]
fn the_status_bar_shows_a_compact_line_and_its_tooltip_the_whole_one_and_the_adapter() {
    let mut a = scanned();
    let id = a.slot.model.as_ref().unwrap().rows()[1].id;
    a.slot.model.as_mut().unwrap().select(id);
    a.set_live(LiveStatus::On { last: None });
    a.live_changed("12:03:41".into());
    let tree = a.slot.tree.as_ref().unwrap();
    let path = fmt::node_path(tree, id).to_string();
    assert_eq!(status_str(&a, false), format!("{path}   |   done   |   live"), "no time of the last change");
    assert!(status_str(&a, true).contains("12:03:41"));
    a.adapter = "Test GPU".into();
    let bar = a.layout.status();
    assert_eq!(tip_str(&a, bar.x + 40.0, bar.y + 5.0), Some(format!("{}
Test GPU", status_str(&a, true))));
}

#[test]
fn a_tip_keeps_its_target_while_its_text_changes_and_loses_it_on_leaving() {
    let mut a = scanned();
    let bar = a.layout.status();
    let (x, y) = (bar.x + 40.0, bar.y + 5.0);
    let (key, before) = target_str(&a, x, y).unwrap();
    a.adapter = "Test GPU".into();
    let (same, after) = target_str(&a, x, y).unwrap();
    assert_eq!(key, same, "the status bar is the same target");
    assert_ne!(before, after, "whose text moved on");
    a.can_elevate = true;
    let other = target_str(&a, a.layout.admin_button().x + 5.0, a.layout.admin_button().y + 5.0).map(|t| t.0);
    assert!(other.is_some() && other != Some(key), "another target is another key");
}

#[test]
fn finishing_a_rescan_leaves_nothing_running() {
    let mut a = scanned();
    a.slot.scan = Scan::Refreshing;
    if let Scan::Running { paused, .. } = &mut a.slot.scan {
        *paused = true;
    }
    a.set_live(LiveStatus::On { last: None });
    a.finish_scan(kary_builder(10, 3).finish().unwrap(), "done".into());
    assert!(matches!(a.slot.scan, Scan::Idle));
    assert!(a.slot.live == LiveStatus::Off && a.slot.summary == "done");
}

#[test]
fn installing_a_preview_tree_leaves_the_scan_state_alone() {
    let mut a = scanned();
    a.begin_scan(0);
    a.slot.summary = "kept".into();
    if let Scan::Running { paused, .. } = &mut a.slot.scan {
        *paused = true;
    }
    a.slot.tree = Some(kary_builder(10, 3).finish().unwrap());
    a.preview_installed();
    assert!(matches!(a.slot.scan, Scan::Running { paused: true, .. }) && a.slot.summary == "kept");
    assert!(a.slot.model.as_ref().and_then(|m| m.selected()).is_some());
}

#[test]
fn swapping_in_a_tree_with_nothing_to_keep_settles_like_a_finish() {
    let mut a = scanned();
    a.slot.tree = None;
    a.slot.model = None;
    a.slot.scan = Scan::Refreshing;
    a.swap_tree(kary_builder(10, 3).finish().unwrap(), "again".into());
    assert!(matches!(a.slot.scan, Scan::Idle) && !a.slot.refreshing() && a.slot.summary == "again");
    assert!(a.slot.model.is_some());
}

#[test]
fn a_rescanned_tree_keeps_sort_expansion_selection_and_screen_row() {
    let mut a = scanned();
    a.handle(Input::Click(5.0, PICKER_H + 2.0, Default::default())); // sort by Name ascending
    let t = a.slot.tree.as_ref().unwrap();
    let m = a.slot.model.as_mut().unwrap();
    let first = m.rows()[1].id;
    m.expand(t, first);
    let kid = m.rows()[2].id;
    m.select(kid);
    a.slot.scroll = 0;
    let path = a.slot.tree.as_ref().unwrap().path(kid);
    a.slot.scan = Scan::Refreshing;
    a.swap_tree(kary_builder(300, 3).finish().unwrap(), "again".into());
    let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_ref().unwrap());
    assert_eq!(t.path(m.selected().unwrap()), path);
    assert_eq!(a.sort(), (0, false));
    assert!(m.is_expanded(m.rows()[1].id));
    assert_eq!(a.slot.summary, "again");
    assert!(!a.slot.refreshing());
}

#[test]
fn refresh_rows_picks_up_tree_changes_and_keeps_the_selection_in_place() {
    let mut a = scanned();
    let sel = a.slot.model.as_ref().unwrap().rows()[3].id;
    a.slot.model.as_mut().unwrap().select(sel);
    let n = a.slot.model.as_ref().unwrap().rows().len();
    let root = a.slot.model.as_ref().unwrap().rows()[0].id;
    a.slot.tree.as_mut().unwrap().add_child(root, &[b'n' as u16, b'w' as u16], 1 << 40, plumb_core::NodeFlags::empty());
    a.refresh_rows();
    let m = a.slot.model.as_ref().unwrap();
    assert_eq!(m.rows().len(), n + 1);
    assert_eq!(m.selected(), Some(sel));
}

#[test]
fn changed_rows_are_tinted_until_they_fade_and_a_new_scan_forgets_them() {
    let mut a = scanned();
    let root = a.slot.model.as_ref().unwrap().rows()[0].id;
    let t0 = Instant::now();
    let new = a.slot.tree.as_mut().unwrap().add_child(root, &[b'n' as u16], 1, plumb_core::NodeFlags::empty());
    a.flash_changes(t0);
    a.refresh_rows();
    assert!(a.flashing());
    assert!(a.slot.flash.alpha(new, t0) > 0.9);
    assert!(a.flash_tick(t0 + flash::FADE / 2));
    assert!(!a.flash_tick(t0 + flash::FADE));
    assert!(!a.flashing());
    a.slot.flash.mark(&[new], t0);
    a.begin_scan(0);
    assert!(!a.flashing());
}

#[test]
fn resize_reclamps_scroll() {
    let mut a = scanned();
    a.slot.scroll = 5;
    a.handle(Input::Resize(800.0, 2000.0));
    assert_eq!(a.slot.scroll, 0);
}

fn with_map() -> App {
    let mut a = scanned();
    a.layout.map_on = true;
    a.update_map(1.0);
    a
}

fn centre(t: &treemap::Tile) -> (f32, f32) {
    (t.rect.x + t.rect.w / 2.0, t.rect.y + t.rect.h / 2.0)
}

#[test]
fn the_map_follows_the_trees_selection_and_ignores_its_own_clicks() {
    let mut a = with_map();
    assert_eq!(a.map_root, Some(NodeId(0)));
    a.handle(Input::Click(20.0, row_y(1), Default::default()));
    a.update_map(1.0); // the hook that notices the selection moved
    let sel = a.slot.model.as_ref().unwrap().selected();
    assert_eq!(a.map_root, treemap::root_for(a.slot.tree.as_ref().unwrap(), sel), "a click in the tree re-roots the map");
    let (i, t) = a.map_tiles().iter().copied().enumerate().find(|(_, t)| t.kind == Kind::File).unwrap();
    let before = a.map_root;
    let (x, y) = centre(&t);
    a.handle(Input::Click(x, y, Default::default()));
    a.update_map(1.0);
    assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(t.node));
    assert_eq!(a.map_root, before, "a click on the map keeps the root");
    assert_eq!(a.map_selected_tile(), Some(i));
    assert!(a.slot.model.as_ref().unwrap().selected_index().is_some(), "its row is revealed in the tree");
}

#[test]
fn double_clicking_a_folder_tile_roots_the_map_there() {
    let mut a = with_map();
    let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::Frame).unwrap();
    let (x, y) = (t.rect.x + 0.5, t.rect.y + 0.5); // the frame's border, clear of its children
    a.handle(Input::Click(x, y, Default::default()));
    a.handle(Input::DoubleClick(x, y));
    assert_eq!(a.map_root, Some(t.node));
    assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(t.node));
    a.update_map(1.0);
    assert!(a.map_tiles().iter().all(|k| k.depth > 0 || a.slot.tree.as_ref().unwrap().parent[k.node] == t.node || k.kind == Kind::Rest));
}

#[test]
fn a_double_click_on_a_file_tile_only_selects_it() {
    let mut a = with_map();
    let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
    let (x, y) = centre(&t);
    let before = a.map_root;
    a.handle(Input::DoubleClick(x, y));
    assert_eq!((a.map_root, a.slot.model.as_ref().unwrap().selected()), (before, Some(t.node)));
}

#[test]
fn hovering_builds_every_kind_of_tip_without_allocating_once_the_buffer_has_grown() {
    let mut a = with_map();
    let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
    let (bar, admin) = (a.layout.status(), a.layout.admin_button());
    a.can_elevate = true;
    let mut buf = String::new();
    let spots = [centre(&t), (bar.x + 40.0, bar.y + 5.0), (admin.x + 5.0, admin.y + 5.0), (a.layout.chip(0).x + 5.0, 10.0)];
    for (x, y) in spots {
        assert!(a.tip_at(x, y, &mut buf), "a tip at {x},{y}"); // grows the buffer
        let want = buf.clone();
        let (found, n) = crate::alloc_count::allocations(|| a.tip_at(x, y, &mut buf));
        assert!(found && buf == want, "{buf}");
        assert_eq!(n, 0, "allocations for the tip {buf:?}");
    }
}

#[test]
fn hovering_a_tile_gives_a_tip_and_leaving_the_map_clears_it() {
    let mut a = with_map();
    let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
    let (x, y) = centre(&t);
    a.handle(Input::Hover(x, y));
    assert!(a.map_hover.is_some());
    let tip = tip_str(&a, x, y).unwrap();
    assert!(tip.contains(&a.slot.tree.as_ref().unwrap().name_string(t.node)), "{tip}");
    a.handle(Input::Hover(5.0, 5.0));
    assert_eq!((a.map_hover, tip_str(&a, 5.0, 5.0)), (None, None));
}

#[test]
fn a_rest_tile_says_how_many_items_it_holds() {
    let tree = plumb_core::synth::flat_tree(3);
    let t = treemap::Tile { node: NodeId(0), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::Rest, bytes: 2048, count: 7, mtime: None };
    assert!(tile_str(&tree, &t, 150_000).starts_with("7 smaller items, "));
    let f = treemap::Tile { node: NodeId(1), kind: Kind::File, count: 1, ..t };
    assert!(tile_str(&tree, &f, 150_000).contains(&tree.name_string(NodeId(1))));
}

#[test]
fn an_own_tile_tip_names_the_folder_itself() {
    let tree = plumb_core::synth::flat_tree(3);
    let t = treemap::Tile { node: NodeId(0), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::Own, bytes: 2048, count: 1, mtime: None };
    let tip = tile_str(&tree, &t, 150_000);
    assert!(tip.contains(&tree.name_string(NodeId(0))) && tip.contains("itself"), "{tip}");
}

#[test]
fn a_parent_processs_own_memory_has_a_tile_that_selects_it_but_is_not_outlined() {
    use plumb_core::procs::{Metric, Procs, Sample};
    let mut a = app();
    a.views.push(ViewInfo { root: "Processes".into(), total: 16 << 30, free: 4 << 30, kind: ViewKind::Processes });
    a.begin_scan(2);
    let (mut procs, mut tree) = Procs::new(Metric::Memory, 4, 1 << 30);
    let s = |pid, parent, name: &str, memory| Sample { pid, parent, created: 5, name: name.into(), memory, private: memory / 2, cpu_100ns: 0, io_bytes: 0 };
    procs.apply(&mut tree, &[s(77, 1, "parent.exe", 400_000), s(78, 77, "child.exe", 100_000)], 0);
    let parent = tree.find("Processes\\parent.exe").expect("the parent is under the root");
    a.finish_scan(tree, "x".into());
    a.layout.map_on = true;
    a.update_map(1.0);
    let i = a.map_tiles().iter().position(|t| t.kind == Kind::Own && t.node == parent).expect("an own tile for the parent");
    assert_eq!(a.map_tiles()[i].bytes, 400_000);
    assert_eq!(a.select_from_map(i), Some(parent), "a click on the own tile selects the process");
    let sel = a.map_selected_tile().expect("the process is outlined");
    assert_eq!(a.map_tiles()[sel].kind, Kind::Frame, "by its frame, not by the own tile");
    let tip = proc_tip_str(&a, a.slot.tree.as_ref().unwrap(), &a.map_tiles()[i]);
    assert!(tip.contains("parent.exe") && tip.contains("itself"), "{tip}");
}

#[test]
fn a_tile_tip_says_when_the_file_was_last_written() {
    let mut tree = plumb_core::synth::flat_tree(3);
    let t = treemap::Tile { node: NodeId(1), rect: Rect::default(), depth: 0, class: treemap::Class::Other, kind: Kind::File, bytes: 2048, count: 1, mtime: None };
    assert!(!tile_str(&tree, &t, 1_700_000_000).contains("modified"));
    let written = 19_791 * 86_400 + 12 * 3600; // noon UTC on 2024-03-09
    tree.set_mtime(NodeId(1), std::num::NonZeroU32::new(written));
    let tip = tile_str(&tree, &t, written + 766 * 86_400);
    let date = fmt::date(fmt::local_day(written));
    assert!(tip.ends_with(&format!(", modified {date}, 2.1 y ago")), "{tip}");
}

#[test]
fn right_clicking_a_tile_selects_it_and_asks_for_the_menu() {
    let mut a = with_map();
    let t = a.map_tiles().iter().copied().find(|t| t.kind == Kind::File).unwrap();
    let (x, y) = centre(&t);
    assert_eq!(a.handle(Input::RightClick(x, y)), Effect::Menu(t.node));
    assert_eq!(a.slot.model.as_ref().unwrap().selected(), Some(t.node));
}

#[test]
fn the_toggle_and_the_divider_work_and_stay_within_limits() {
    let mut a = scanned();
    assert!(!a.layout.map_on);
    a.handle(Input::ToggleMap);
    assert!(a.layout.map_on);
    let b = a.layout.map_button();
    a.handle(Input::Click(b.x + 2.0, b.y + 2.0, Default::default()));
    assert!(!a.layout.map_on, "the status bar button toggles too");
    a.layout.map_on = true;
    a.layout.set_map_w(162.0); // at 800 DIPs the pane can reach 175: room to grow by 10
    let (d, w0) = (a.layout.map_divider().unwrap(), a.layout.map_w);
    assert!(a.over_divider(d.x + 1.0, d.y + 5.0));
    a.handle(Input::Click(d.x + 1.0, d.y + 5.0, Default::default()));
    a.handle(Input::Drag(d.x - 9.0, d.y + 5.0));
    assert_eq!(a.layout.map_w, w0 + 10.0, "dragging left widens the pane");
    a.handle(Input::Drag(-10_000.0, 0.0));
    assert!(a.layout.column(0).1 >= crate::layout::MIN_NAME_W, "the tree keeps its minimum");
    a.handle(Input::Drag(10_000.0, 0.0));
    assert_eq!(a.layout.map().unwrap().w, crate::layout::MAP_MIN_W, "and the pane keeps its own");
    a.handle(Input::Release);
    let before = a.layout.map_w;
    a.handle(Input::Drag(0.0, 0.0));
    assert_eq!(a.layout.map_w, before, "released");
}

#[test]
fn the_map_survives_a_tree_swap_and_an_emptied_root() {
    let mut a = with_map();
    a.swap_tree(kary_builder(50, 3).finish().unwrap(), "again".into());
    a.update_map(1.0);
    assert!(a.map_root.is_some() && !a.map_tiles().is_empty());
    a.begin_scan(1);
    a.update_map(1.0);
    assert!(a.map_tiles().is_empty() && a.map_hover.is_none(), "no tree, no tiles");
}

#[test]
fn the_map_pane_state_is_remembered() {
    let mut a = app();
    a.layout.map_on = false;
    a.layout.map_w = 300.0;
    let s = a.remembered();
    let mut b = app();
    b.restore(&s);
    assert_eq!((b.layout.map_on, b.layout.map_w), (false, 300.0));
    let mut c = app();
    c.restore(&crate::state::State { map: crate::state::MapState { visible: true, width: f32::NAN }, ..Default::default() });
    assert!(c.layout.map_on && c.layout.map_w.is_finite() && c.layout.map_w >= crate::layout::MAP_MIN_W);
}

/// A non-root file and a non-root folder that has children, and one of that folder's children.
fn picks(a: &App) -> (NodeId, NodeId, NodeId) {
    let t = a.slot.tree.as_ref().unwrap();
    let all = || (0..t.len() as u32).map(NodeId).filter(|&i| !t.parent[i].is_none());
    let file = all().find(|&i| !t.has_children(i)).unwrap();
    let dir = all().find(|&i| t.has_children(i)).unwrap();
    (file, dir, t.children(dir).next().unwrap())
}

fn select(a: &mut App, id: NodeId) {
    let (t, m) = (a.slot.tree.as_ref().unwrap(), a.slot.model.as_mut().unwrap());
    m.reveal(t, id);
    m.select(id);
}

#[test]
fn del_marks_and_unmarks_the_selection_and_the_filter_box_keeps_its_own_del() {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    select(&mut a, file);
    a.handle(Input::Edit(Edit::Delete));
    assert_eq!(a.basket().len(), 1);
    a.handle(Input::Edit(Edit::Delete));
    assert!(a.basket().is_empty(), "the second Del unmarks");
    a.handle(Input::Find); // focus the filter box
    a.handle(Input::Edit(Edit::Delete));
    assert!(a.basket().is_empty(), "with the filter box focused, Del edits text and marks nothing");
    let mut none = app();
    none.handle(Input::Edit(Edit::Delete));
    assert!(none.basket().is_empty(), "no tree, nothing selected: nothing happens");
}

#[test]
fn a_folder_and_its_child_marked_together_count_the_folder_once() {
    let mut a = scanned();
    let (_, dir, child) = picks(&a);
    a.toggle_mark(child);
    a.toggle_mark(dir);
    a.update_map(1.0);
    let t = a.slot.tree.as_ref().unwrap();
    assert_eq!(a.basket_label(), Some(format!("1 marked, up to {}", fmt::size(t.subtree_size[dir]))));
    let (dir_path, dir_size) = (t.path(dir), t.subtree_size[dir]);
    let req = a.commit_request().unwrap();
    assert_eq!(req.paths, vec![dir_path], "the commit lists the folder, not its child");
    assert_eq!(req.bytes, dir_size);
}

#[test]
fn roots_and_scans_are_refused_with_a_reason_that_reaches_the_status_line() {
    let mut a = scanned();
    let root = a.slot.tree.as_ref().unwrap().roots[0];
    assert_eq!(a.mark_state(root), MarkState::Refused("it is a drive root"));
    a.toggle_mark(root);
    assert!(a.basket().is_empty());
    let line = a.toast.outcome.line.clone();
    assert!(line.starts_with("could not mark ") && line.contains(": it is a drive root"), "{line}");
    a.handle(Input::Click(5.0, a.layout.header().y + 5.0, Default::default()));
    assert!(a.toast.active(), "a click elsewhere leaves the toast to its timer");
    a.layout.toast = a.toast.lines().len();
    let t = a.layout.toast_rect();
    a.handle(Input::Click(t.x + 4.0, t.y + 4.0, Default::default()));
    assert!(!a.toast.active(), "a click on the toast dismisses it");
    a.toggle_mark(root);
    a.handle(Input::Escape);
    assert!(!a.toast.active(), "Esc dismisses it");
    a.layout.toast = 0;
    let (file, ..) = picks(&a);
    a.slot.scan = Scan::Refreshing;
    assert_eq!(a.mark_state(file), MarkState::Refused("the drive is being scanned"));
}

#[test]
fn a_slot_swaps_whole_and_the_query_text_stays() {
    let mut a = scanned();
    a.slot.scroll = 7;
    a.slot.scan = Scan::Refreshing;
    a.slot.summary = "mine".into();
    a.slot.filter.ui.text = "query".into();
    let mut other = Slot { summary: "theirs".into(), ..Slot::default() };
    a.swap_slot(&mut other);
    assert!(a.slot.tree.is_none() && a.slot.model.is_none(), "the other view has no tree");
    assert_eq!((a.slot.refreshing(), a.slot.summary.as_str()), (false, "theirs"));
    assert_eq!(a.slot.filter.ui.text, "query", "the query text belongs to the window, not the view");
    assert_eq!((other.scroll, other.refreshing(), other.summary.as_str()), (7, true, "mine"));
    assert!(other.filter.ui.text.is_empty());
    a.swap_slot(&mut other);
    assert!(a.slot.tree.is_some() && a.slot.model.is_some());
    assert_eq!((a.slot.refreshing(), a.slot.summary.as_str()), (true, "mine"));
    assert_eq!(a.slot.filter.ui.text, "query");
    // Naming every field means a new one fails to compile until this test covers it.
    let Slot { tree: _, model: _, scroll: _, scan: _, summary: _, live: _, filter: _, basket: _, flash: _, proc_info: _, proc_metric: _ } = &a.slot;
}

#[test]
fn marks_survive_a_drive_switch_a_rescan_keeps_them_and_a_first_scan_clears_them() {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    a.toggle_mark(file);
    let mut other = Slot::default();
    a.swap_slot(&mut other);
    assert!(a.basket().is_empty(), "the other view has no marks");
    a.swap_slot(&mut other);
    assert_eq!(a.basket().len(), 1, "coming back finds them");
    let t = plumb_core::synth::kary_builder(300, 3).finish().unwrap();
    a.swap_tree(t, "rescanned".into());
    a.update_map(1.0);
    assert_eq!(a.basket().len(), 1, "a background rescan leaves them");
    assert!(a.basket_label().is_some());
    a.begin_scan(0);
    assert_eq!(a.basket().len(), 1, "rescanning the view on screen keeps its marks");
    a.begin_scan(1);
    assert!(a.basket().is_empty(), "a first scan of another view starts clean");
}

#[test]
fn a_mark_that_can_no_longer_be_deleted_is_dropped_and_reported_with_the_result() {
    let mut a = scanned();
    let (bad, good) = {
        let t = a.slot.tree.as_ref().unwrap();
        let mut leaves = (0..t.len() as u32).map(NodeId).filter(|&i| !t.parent[i].is_none() && !t.has_children(i));
        (leaves.next().unwrap(), leaves.next().unwrap())
    };
    a.toggle_mark(bad);
    a.toggle_mark(good);
    let bad_path = a.slot.tree.as_ref().unwrap().path(bad);
    a.system_root = bad_path.clone(); // now the first one is refused, as if renamed to an unsafe name
    let req = a.commit_request().expect("the other mark still goes");
    assert_eq!(req.paths.len(), 1);
    assert_eq!(a.basket().len(), 1, "the refused mark is dropped");
    a.finish_delete(&req.paths, 0, &[]);
    assert!(a.toast.outcome.line.contains("1 skipped: the Windows folder is protected"), "{}", a.toast.outcome.line);
    assert!(a.toast.outcome.detail.iter().any(|d| d.starts_with(&bad_path)));
    // Alone, it gives no request, only the reason.
    let mut a = scanned();
    a.toggle_mark(bad);
    a.system_root = a.slot.tree.as_ref().unwrap().path(bad);
    assert!(a.commit_request().is_none());
    assert!(a.toast.outcome.line.starts_with("could not delete ") && a.toast.outcome.line.ends_with(": the Windows folder is protected"), "{}", a.toast.outcome.line);
    assert!(a.basket().is_empty());
}

#[test]
fn the_marks_of_a_drive_whose_state_was_let_go_come_back_with_its_next_scan() {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    a.toggle_mark(file);
    let mut gone = Slot::default();
    a.swap_slot(&mut gone); // the view goes away with its mark
    a.stash_basket(2, &mut gone); // and is let go
    a.begin_scan(3);
    assert!(a.basket().is_empty(), "another view does not get them");
    a.begin_scan(2);
    assert_eq!(a.basket().len(), 1, "its own scan does");
}

#[test]
fn a_marked_path_that_is_gone_at_commit_is_dropped_not_deleted() {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    a.toggle_mark(file);
    assert!(a.slot.tree.as_mut().unwrap().remove(file));
    assert!(a.commit_request().is_none());
    assert!(a.basket().is_empty());
    assert!(toast_str(&a).starts_with("the marked items are already gone"), "{}", toast_str(&a));
}

#[test]
fn a_commit_refuses_a_busy_drive_and_a_second_delete() {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    a.toggle_mark(file);
    a.slot.scan = Scan::Refreshing;
    assert!(a.commit_request().is_none());
    a.slot.scan = Scan::Idle;
    assert!(a.commit_request().is_some());
    a.set_deleting(true);
    assert!(a.commit_request().is_none(), "one delete at a time");
}

#[test]
fn a_finished_delete_unmarks_what_went_and_keeps_what_failed() {
    let mut a = scanned();
    let (file, dir, _) = picks(&a);
    a.toggle_mark(file);
    a.toggle_mark(dir);
    let t = a.slot.tree.as_ref().unwrap();
    let (gone, kept) = (t.path(file), t.path(dir));
    a.set_deleting(true);
    a.finish_delete(std::slice::from_ref(&gone), 0, &[(kept.clone(), "access denied".into())]);
    assert!(a.basket().contains(&kept) && !a.basket().contains(&gone));
    assert!(toast_str(&a).starts_with("1 of 2 not deleted: access denied; still marked"), "{}", toast_str(&a));
    assert!(a.commit_request().is_some(), "the delete is over, so a new commit may start");
}

#[test]
fn the_chip_is_clickable_only_while_something_is_marked() {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    let chip = Layout { basket: true, ..a.layout.clone() }.basket_chip();
    assert!(matches!(a.handle(Input::Click(chip.x + 5.0, chip.y + 5.0, Default::default())), Effect::None), "nothing marked: no chip, no effect");
    a.toggle_mark(file);
    a.update_map(1.0);
    assert_eq!(a.handle(Input::Click(chip.x + 5.0, chip.y + 5.0, Default::default())), Effect::None);
    assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Delete(_))), "the chip asks how to delete");
}

/// A modal over a scanned app with one file marked, and the middle of one of its buttons.
fn asking() -> (App, impl Fn(&App, Pick) -> (f32, f32)) {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    a.toggle_mark(file);
    a.ask_delete();
    let centre = |a: &App, p| {
        let r = a.modal_button(p).expect("the button is there");
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    };
    (a, centre)
}

#[test]
fn copy_on_the_holders_modal_copies_and_stays_open_until_closed() {
    let mut a = scanned();
    a.ask(Modal::holders("f.txt", &[plumb_scan::holders::Holder { pid: 7, name: "x.exe".into() }]));
    let r = a.modal_button(Pick::Copy).expect("a Copy button");
    let e = a.handle(Input::Click(r.x + r.w / 2.0, r.y + r.h / 2.0, Default::default()));
    assert_eq!(e, Effect::Copy("f.txt\nx.exe  (PID 7)".into()));
    assert!(a.modal().is_some_and(|m| m.buttons[0].label == "Copied"), "the button says so");
    let r = a.modal_button(Pick::Close).unwrap();
    assert_eq!(a.handle(Input::Click(r.x + r.w / 2.0, r.y + r.h / 2.0, Default::default())), Effect::None);
    assert!(a.modal().is_none());
}

#[test]
fn answering_a_modal_keeps_the_toast() {
    let (mut a, centre) = asking();
    a.report(Outcome::problem("could not delete C:\\f: in use; still marked"));
    let (x, y) = centre(&a, Pick::Cancel);
    a.handle(Input::Click(x, y, Default::default()));
    assert!(a.modal().is_none() && a.toast.outcome.line.starts_with("could not delete"), "{:?}", a.toast.outcome.line);
}

#[test]
fn a_modal_takes_all_input_and_enter_escape_tab_and_clicks_answer_it() {
    let (mut a, centre) = asking();
    let (scroll, marks) = (a.slot.scroll, a.slot.basket.len());
    // Everything but its own buttons is swallowed, including the keys that would unmark or scroll the list.
    for i in [Input::Wheel(3, 5.0, 100.0), Input::Edit(Edit::Delete), Input::Key(Key::Down, Default::default()), Input::Find] {
        assert_eq!(a.handle(i), Effect::None);
    }
    assert!(a.modal().is_some() && a.slot.scroll == scroll && a.slot.basket.len() == marks);
    // A click on the scrim cancels (the marks stay); one inside the panel, off its buttons, does nothing.
    let p = a.modal().map(|m| m.geometry(a.layout.w, a.layout.h).panel).unwrap();
    a.handle(Input::Click(p.x + 2.0, p.y + 2.0, Default::default()));
    assert!(a.modal().is_some());
    assert_eq!(a.handle(Input::Click(5.0, 5.0, Default::default())), Effect::None);
    assert!(a.modal().is_none() && a.slot.basket.len() == marks);
    a.ask_delete();
    // ... but not the "Close anyway?" question.
    a.handle(Input::Escape);
    a.ask_close();
    a.handle(Input::Click(5.0, 5.0, Default::default()));
    assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Close)));
    a.handle(Input::Escape);
    a.ask_delete();
    // Escape cancels; the marks stay.
    assert_eq!(a.handle(Input::Escape), Effect::None);
    assert!(a.modal().is_none() && a.slot.basket.len() == marks);
    // Enter presses the default: Recycle.
    a.ask_delete();
    let Effect::Delete { permanent, paths, .. } = a.handle(Input::Enter) else { panic!("Enter on the default button") };
    assert!(!permanent && paths.len() == 1 && a.modal().is_none());
    // Tab moves the focus; Enter then presses that button (Delete permanently asks a second question, defaulting to Cancel).
    a.ask_delete();
    a.handle(Input::Tab);
    assert_eq!(a.handle(Input::Enter), Effect::None);
    assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Permanent(_))));
    assert_eq!(a.handle(Input::Enter), Effect::None, "the default of the second question is Cancel");
    assert!(a.modal().is_none() && a.slot.basket.len() == marks);
    // A click on a button answers it: permanently, then Yes.
    a.ask_delete();
    let at = centre(&a, Pick::Permanent);
    a.handle(Input::Click(at.0, at.1, Default::default()));
    let at = centre(&a, Pick::Yes);
    assert!(matches!(a.handle(Input::Click(at.0, at.1, Default::default())), Effect::Delete { permanent: true, .. }));
    // Unmark all clears the basket.
    a.ask_delete();
    let at = centre(&a, Pick::Unmark);
    a.handle(Input::Click(at.0, at.1, Default::default()));
    assert!(a.slot.basket.is_empty() && a.modal().is_none());
}

#[test]
fn a_hovered_modal_button_is_hot_and_modals_queue_behind_each_other() {
    let (mut a, centre) = asking();
    let at = centre(&a, Pick::Cancel);
    a.handle(Input::Hover(at.0, at.1));
    assert_eq!(a.with_scene(false, |s| s.hot), a.modal_button(Pick::Cancel));
    a.handle(Input::Hover(1.0, 1.0));
    assert_eq!(a.with_scene(false, |s| s.hot), None);
    // A failure and a close question arriving over the delete question wait their turn, the close question once.
    a.ask(Modal::failed(0, false, &Failure::new("delete", "deleted", &[("C:\\f".into(), "in use".into())], 1, ""), vec!["C:\\f".into()]));
    a.ask_close();
    a.ask_close();
    assert_eq!(a.modals.len(), 3);
    a.handle(Input::Escape);
    assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Failed { .. })));
    let Effect::Delete { paths, .. } = a.handle(Input::Enter) else { panic!("Retry") };
    assert_eq!(paths, ["C:\\f"]);
    assert!(matches!(a.modal().map(|m| &m.ask), Some(Ask::Close)));
    assert_eq!(a.handle(Input::Escape), Effect::None, "Esc keeps the window open");
    a.ask_close();
    a.handle(Input::Key(Key::Left, Default::default()));
    assert_eq!(a.handle(Input::Enter), Effect::Quit);
}

#[test]
fn a_deleted_folder_takes_its_childs_mark_and_a_stale_mark_is_pruned_at_commit() {
    let mut a = scanned();
    let (file, dir, child) = picks(&a);
    let t = a.slot.tree.as_ref().unwrap();
    let (fp, dp) = (t.path(file), t.path(dir));
    a.toggle_mark(child);
    a.toggle_mark(dir);
    a.toggle_mark(file);
    a.set_deleting(true);
    a.finish_delete(std::slice::from_ref(&dp), 0, &[]);
    assert_eq!(a.basket().len(), 1, "the child's mark went with its folder");
    assert!(a.basket().contains(&fp));
    let mut b = scanned();
    let (file, dir, _) = picks(&b);
    b.toggle_mark(file);
    b.toggle_mark(dir);
    let fid = file;
    assert!(b.slot.tree.as_mut().unwrap().remove(fid));
    let req = b.commit_request().unwrap();
    assert_eq!(req.paths.len(), 1);
    assert_eq!(b.basket().len(), 1, "the mark that no longer resolves is dropped, not kept to come back later");
}

#[test]
fn a_mark_from_the_menu_is_found_again_by_path_and_a_vanished_path_marks_nothing() {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    let p = a.slot.tree.as_ref().unwrap().path(file);
    a.toggle_mark_path(&p);
    assert!(a.basket().contains(&p));
    a.toggle_mark_path("nowhere\\at all");
    assert_eq!(a.basket().len(), 1);
    assert!(toast_str(&a).starts_with("that item is gone"));
}

#[test]
fn del_with_a_tree_but_nothing_selected_marks_nothing() {
    let mut a = scanned();
    a.slot.model = Some(RowModel::new(a.slot.tree.as_ref().unwrap()));
    assert!(a.slot.model.as_ref().unwrap().selected().is_none());
    a.handle(Input::Edit(Edit::Delete));
    assert!(a.basket().is_empty());
}

#[test]
fn a_renamed_marked_item_drops_off_the_chip_and_the_tiles() {
    let mut a = scanned();
    let (file, ..) = picks(&a);
    a.toggle_mark(file);
    a.update_map(1.0);
    assert!(a.basket_label().is_some());
    let n: Vec<u16> = "renamed".encode_utf16().collect();
    a.slot.tree.as_mut().unwrap().rename(file, &n);
    a.update_map(1.0);
    assert!(a.basket_label().is_none(), "the mark no longer names anything, so no chip");
    assert!(!a.basket_active());
}

#[test]
fn a_delete_report_counts_missing_apart_and_keeps_every_reason_for_the_hover() {
    let mut a = scanned();
    a.set_deleting(true);
    let gone: Vec<String> = vec!["X-a".into(), "X-b".into()];
    a.finish_delete(&gone, 1, &[("X-c".into(), "in use".into()), ("X-d".into(), "denied".into())]);
    let shown = toast_str(&a);
    assert!(shown.starts_with("2 of 4 not deleted: in use; still marked"), "{shown}");
    assert!(a.toast.outcome.detail.iter().any(|d| d.contains("X-d: denied")), "every reason is kept: {:?}", a.toast.outcome.detail);
    let mut b = scanned();
    b.finish_delete(&gone, 1, &[]);
    assert!(toast_str(&b).starts_with("deleted 1 item, 1 already gone"), "{}", toast_str(&b));
}

#[test]
fn begin_delete_refuses_a_scanning_drive_and_a_second_delete() {
    let mut a = scanned();
    assert!(a.begin_delete());
    assert!(a.deleting());
    assert!(!a.begin_delete(), "one at a time");
    a.set_deleting(false);
    a.slot.scan = Scan::Refreshing;
    assert!(!a.begin_delete());
    assert!(toast_str(&a).starts_with("cannot delete: the drive is being scanned"));
}
