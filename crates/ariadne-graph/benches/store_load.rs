use ariadne_graph::extract;
use ariadne_graph::store::Store;
use criterion::{criterion_group, criterion_main, Criterion};
use std::path::PathBuf;

fn crate_src_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn bench_store_load(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("bench.db");

    let mut graph = ariadne_graph::Graph::new();
    extract::extract_directory(&crate_src_root(), &mut graph).expect("extract failed");

    let mut store = Store::open(&db_path).expect("open store");
    store.save(&graph).expect("save graph");
    drop(store);

    c.bench_function("store_load_ariadne_src", |b| {
        b.iter(|| {
            let store = Store::open(&db_path).expect("reopen store");
            store.load().expect("load graph")
        });
    });
}

criterion_group!(benches, bench_store_load);
criterion_main!(benches);
