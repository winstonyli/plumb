use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use plumb_core::{NodeFlags, NodeId, Rect};
use plumb_core::treemap::{hit, layout};
use plumb_core::filter::{Keep, Names, Query};
use std::sync::atomic::AtomicU64;
use plumb_core::mft::{ParsedRecord, parse_record};
use plumb_core::rows::RowModel;
use plumb_core::synth::{RECORD_SIZE, RecordBuilder, flat_tree, kary_builder};
use std::hint::black_box;

/// 1024 records, each with two names and a data stream, back to back (1 MiB).
fn record_chunk() -> Vec<u8> {
    let one = RecordBuilder::file()
        .file_name(5, "document-with-a-typical-name.txt", 1)
        .file_name(5, "DOCUME~1.TXT", 2)
        .data_nonresident(1 << 20, 1 << 20)
        .build();
    one.repeat(1024)
}

fn bench_parse(c: &mut Criterion) {
    let chunk = record_chunk();
    let mut g = c.benchmark_group("mft");
    g.throughput(Throughput::Bytes(chunk.len() as u64));
    g.bench_function("parse_1024_records", |b| {
        let mut out = ParsedRecord::default();
        b.iter_batched_ref(
            || chunk.clone(),
            |buf| {
                for rec in buf.as_chunks_mut::<RECORD_SIZE>().0 {
                    parse_record(rec, &mut out).unwrap();
                    black_box(&out);
                }
            },
            BatchSize::LargeInput,
        );
    });
    g.finish();
}

fn bench_sums(c: &mut Criterion) {
    let mut g = c.benchmark_group("tree");
    g.sample_size(10);
    g.bench_function("finish_5m_nodes", |b| {
        b.iter_batched(
            || kary_builder(5_000_000, 8),
            |builder| black_box(builder.finish().unwrap()),
            BatchSize::LargeInput,
        );
    });
    g.finish();
}

fn bench_rows(c: &mut Criterion) {
    let tree = flat_tree(1_000_000);
    let mut g = c.benchmark_group("rows");
    g.sample_size(10);
    g.bench_function("expand_1m_children_sort_by_size", |b| {
        b.iter_batched_ref(
            || RowModel::new(&tree),
            |m| {
                m.expand(&tree, NodeId(0));
                black_box(m.rows().len());
            },
            BatchSize::LargeInput,
        );
    });
    let mut expanded = RowModel::new(&tree);
    expanded.expand(&tree, NodeId(0));
    g.bench_function("visible_window_of_50", |b| {
        b.iter(|| black_box(expanded.visible_window(black_box(500_000), 50).len()));
    });
    g.finish();
}

fn bench_mutation(c: &mut Criterion) {
    let mut g = c.benchmark_group("mutation");
    g.sample_size(10);
    g.throughput(Throughput::Elements(100_000));
    // Leaves of the 8-ary tree start at node 625_000; directories are below that.
    g.bench_function("apply_100k_deltas_to_5m_nodes", |b| {
        b.iter_batched_ref(
            || kary_builder(5_000_000, 8).finish().unwrap(),
            |t| {
                let name: Vec<u16> = "renamed".encode_utf16().collect();
                for i in 0..100_000u32 {
                    let leaf = NodeId(625_000 + i * 37);
                    match i % 4 {
                        0 => t.set_size(leaf, u64::from(i) * 13),
                        1 => {
                            t.add_child(NodeId(i % 600_000), &name, 4096, NodeFlags::empty());
                        }
                        2 => t.rename(leaf, &name),
                        _ => {
                            t.remove(leaf);
                        }
                    }
                }
                black_box(t.dead());
            },
            BatchSize::LargeInput,
        );
    });
    g.finish();
}

fn bench_refresh(c: &mut Criterion) {
    let mut g = c.benchmark_group("rows");
    g.sample_size(10);
    g.bench_function("refresh_after_one_add_in_1m_children", |b| {
        b.iter_batched_ref(
            || {
                let mut tree = flat_tree(1_000_000);
                let mut m = RowModel::new(&tree);
                m.expand(&tree, NodeId(0));
                m.select(m.rows()[500_000].id);
                tree.add_child(NodeId(0), &[0x61], 1 << 41, NodeFlags::empty());
                (tree, m)
            },
            |(tree, m)| black_box(m.refresh(tree, 499_990)),
            BatchSize::LargeInput,
        );
    });
    g.bench_function("refresh_nothing_touched_in_1m_children", |b| {
        let mut tree = flat_tree(1_000_000);
        let mut m = RowModel::new(&tree);
        m.expand(&tree, NodeId(0));
        m.select(m.rows()[500_000].id);
        b.iter(|| black_box(m.refresh(&mut tree, 499_990)));
    });
    g.finish();
}

fn bench_filter(c: &mut Criterion) {
    let mut g = c.benchmark_group("filter");
    g.sample_size(10);
    for n in [1_000_000u32, 5_000_000] {
        let tree = flat_tree(n);
        let sub = Query { text: "n99".into(), regex: false }.compile().unwrap().unwrap();
        let re = Query { text: r"^n9\d*7$".into(), regex: true }.compile().unwrap().unwrap();
        let names = Names::of(&tree);
        let cancel = AtomicU64::new(1);
        g.bench_function(format!("snapshot_{n}"), |b| b.iter(|| black_box(Names::of(&tree))));
        g.bench_function(format!("scan_substring_{n}"), |b| b.iter(|| black_box(names.scan(&sub, &cancel, 1))));
        g.bench_function(format!("scan_regex_{n}"), |b| b.iter(|| black_box(names.scan(&re, &cancel, 1))));
        let hits = names.scan(&sub, &cancel, 1).unwrap();
        g.bench_function(format!("keep_{n}"), |b| b.iter(|| black_box(Keep::build(&tree, &hits))));
    }
    // Deep tree, broad query: about a fifth of the names match, so most folders are kept.
    let tree = kary_builder(2_000_000, 3).finish().unwrap();
    let broad = Query { text: "1".into(), regex: false }.compile().unwrap().unwrap();
    let names = Names::of(&tree);
    let hits = names.scan(&broad, &AtomicU64::new(1), 1).unwrap();
    g.bench_function("keep_broad_deep_2000000", |b| b.iter(|| black_box(Keep::build(&tree, &hits))));
    let keep = std::rc::Rc::new(Keep::build(&tree, &hits));
    eprintln!("broad deep query: {} hits", keep.matches());
    g.bench_function("rows_filtered_broad_deep_2000000", |b| {
        b.iter_batched_ref(
            || RowModel::new(&tree),
            |m| {
                m.set_filter(&tree, Some(keep.clone()));
                black_box(m.rows().len());
            },
            BatchSize::LargeInput,
        )
    });
    // The same rebuild with the sorted child lists already cached: what a live refresh pays.
    let mut warm = RowModel::new(&tree);
    warm.set_filter(&tree, Some(keep.clone()));
    eprintln!("broad deep query: {} rows", warm.rows().len());
    g.bench_function("rows_refiltered_warm_broad_deep_2000000", |b| {
        b.iter(|| {
            warm.set_filter(&tree, Some(keep.clone()));
            black_box(warm.rows().len())
        })
    });
    g.finish();
}

fn bench_treemap(c: &mut Criterion) {
    let tree = kary_builder(5_000_000, 8).finish().unwrap();
    let mut g = c.benchmark_group("treemap");
    g.sample_size(20);
    for (name, w, h) in [("800x600", 800.0f32, 600.0f32), ("3840x2160", 3840.0, 2160.0)] {
        let rect = Rect { x: 0.0, y: 0.0, w, h };
        g.bench_function(format!("layout_root_{name}"), |b| b.iter(|| black_box(layout(&tree, NodeId(0), rect, 1.0))));
        g.bench_function(format!("layout_folder_{name}"), |b| b.iter(|| black_box(layout(&tree, NodeId(9), rect, 1.0))));
    }
    let tiles = layout(&tree, NodeId(0), Rect { x: 0.0, y: 0.0, w: 3840.0, h: 2160.0 }, 1.0);
    g.bench_function(format!("hit_{}_tiles", tiles.len()), |b| b.iter(|| black_box(hit(&tiles, black_box(1921.5), black_box(1080.5)))));
    g.finish();
}

criterion_group!(benches, bench_parse, bench_sums, bench_rows, bench_mutation, bench_refresh, bench_filter, bench_treemap);
criterion_main!(benches);
