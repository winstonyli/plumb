use super::*;
use crate::node::NodeFlags;
use crate::tree::tests::sample;

// sample(): 0=C:, 1=big, 2=f1, 3=f2, 4=mid, 5=g, 6=small
fn u(s: &str) -> Vec<u16> {
    s.encode_utf16().collect()
}

fn matcher(text: &str, regex: bool) -> Matcher {
    Query { text: text.into(), regex }.compile().unwrap().unwrap()
}

fn hit_ids(h: &Hits) -> Vec<u32> {
    (0..h.len() as u32).filter(|&i| h.get(NodeId(i))).collect()
}

fn scan(t: &Tree, m: &Matcher) -> Hits {
    Names::of(t).scan(m, &AtomicU64::new(1), 1).unwrap()
}

#[test]
fn find_gives_every_span_in_the_original_text() {
    let spans = |m: Matcher, s: &str| m.find(s).collect::<Vec<_>>();
    assert_eq!(spans(matcher("FILE", false), "My file(1).txt"), vec![3..7]);
    assert_eq!(spans(matcher("a", false), "banana"), [1..2, 3..4, 5..6]);
    assert_eq!(spans(matcher(r"\d+", true), "a12b345"), [1..3, 4..7]);
    assert!(spans(matcher("zzz", false), "abc").is_empty());
    assert!(spans(matcher("x*", true), "abc").is_empty(), "empty matches are not spans");
}

#[test]
fn substring_mode_escapes_the_text_and_ignores_case() {
    let mut buf = String::new();
    let m = matcher("c++", false);
    assert!(m.is_match(&u("ABC++x"), &mut buf));
    assert!(!m.is_match(&u("abc"), &mut buf));
    assert!(matcher("file(1)", false).is_match(&u("My File(1).txt"), &mut buf));
}

#[test]
fn regex_mode_uses_the_pattern() {
    let mut buf = String::new();
    let m = matcher(r"^img_\d+$", true);
    assert!(m.is_match(&u("IMG_42"), &mut buf));
    assert!(!m.is_match(&u("img_x"), &mut buf));
}

#[test]
fn matching_folds_case_beyond_ascii() {
    let mut buf = String::new();
    assert!(matcher("ÉCOLE", false).is_match(&u("école.txt"), &mut buf));
}

#[test]
fn surrogate_pairs_match_and_lone_surrogates_do_not_panic() {
    let mut buf = String::new();
    let m = matcher("\u{1F4C1}", false);
    assert!(m.is_match(&u("\u{1F4C1}docs"), &mut buf));
    assert!(!m.is_match(&[0x61, 0xD800, 0x62], &mut buf));
}

#[test]
fn an_empty_query_is_no_filter() {
    assert!(Query { text: String::new(), regex: true }.compile().unwrap().is_none());
}

#[test]
fn bad_patterns_report_why() {
    assert!(matches!(Query { text: "(".into(), regex: true }.compile(), Err(FilterError::Invalid(_))));
    assert!(matches!(Query { text: "[a".into(), regex: true }.compile(), Err(FilterError::Invalid(_))));
    assert_eq!(
        Query { text: r"((\w{100}){100}){100}".into(), regex: true }.compile().unwrap_err(),
        FilterError::TooComplex
    );
}

#[test]
fn scan_finds_the_matching_names() {
    let t = sample();
    assert_eq!(hit_ids(&scan(&t, &matcher("m", false))), vec![4, 6]); // mid, small
    assert!(scan(&t, &matcher("zzz", false)).count() == 0);
}

#[test]
fn scan_stops_when_the_generation_moves_on() {
    let t = sample();
    let cancel = AtomicU64::new(2);
    assert!(Names::of(&t).scan(&matcher("m", false), &cancel, 1).is_none());
}

#[test]
fn keep_is_the_matches_and_their_ancestors() {
    let t = sample();
    let k = Keep::build(&t, &scan(&t, &matcher("f", false))); // f1, f2
    let kept: Vec<u32> = (0..7).filter(|&i| k.is_kept(NodeId(i))).collect();
    assert_eq!(kept, vec![0, 1, 2, 3]);
    assert!(k.is_ancestor(NodeId(1)) && !k.is_ancestor(NodeId(2)));
    assert!(k.is_hit(NodeId(2)) && !k.is_hit(NodeId(1)));
    assert_eq!(k.matches(), 2);
}

#[test]
fn a_matching_folder_with_a_matching_child_is_both() {
    let mut t = sample();
    t.add_child(NodeId(1), &u("bigger"), 1, NodeFlags::empty()); // id 7, under big
    let k = Keep::build(&t, &scan(&t, &matcher("big", false)));
    assert!(k.is_hit(NodeId(1)) && k.is_hit(NodeId(7)));
    assert!(k.is_ancestor(NodeId(1)), "big matches and so does something below it");
    assert!(k.is_ancestor(NodeId(0)) && !k.is_ancestor(NodeId(7)));
    assert!(!k.is_kept(NodeId(2)), "its other children are not kept");
}

#[test]
fn dead_nodes_are_never_kept() {
    let mut t = sample();
    let h = scan(&t, &matcher("f", false));
    t.remove(NodeId(2));
    let k = Keep::build(&t, &h);
    assert!(!k.is_kept(NodeId(2)));
    assert!(k.is_kept(NodeId(3)) && k.is_ancestor(NodeId(1)));
    t.remove(NodeId(3));
    let k = Keep::build(&t, &h);
    assert_eq!(k.matches(), 0);
    assert!(!k.is_kept(NodeId(0)));
}

#[test]
fn update_follows_adds_renames_and_removals() {
    let mut t = sample();
    let m = matcher("f", false);
    let mut h = scan(&t, &m);
    assert_eq!(hit_ids(&h), vec![2, 3]);
    let new = t.add_child(NodeId(4), &u("ff"), 5, NodeFlags::empty());
    t.rename(NodeId(5), &u("f-g"));
    t.remove(NodeId(2));
    h.update(&t, &m, t.touched());
    assert_eq!(hit_ids(&h), vec![3, 5, new.0]);
    assert_eq!(h.len(), t.len());
    t.rename(NodeId(5), &u("g"));
    h.update(&t, &m, t.touched());
    assert!(!h.get(NodeId(5)));
}

#[test]
fn update_covers_nodes_added_after_the_scan_even_if_untouched() {
    let mut t = sample();
    let m = matcher("f", false);
    let h0 = scan(&t, &m);
    t.add_child(NodeId(0), &u("fz"), 1, NodeFlags::empty());
    let _ = t.take_touched();
    let mut h = h0;
    h.update(&t, &m, &[]);
    assert!(h.get(NodeId(7)));
}
