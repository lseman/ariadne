use ariadne_graph::extract;
use criterion::{criterion_group, criterion_main, Criterion};
use std::path::PathBuf;

fn crate_src_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn bench_extract_directory(c: &mut Criterion) {
    let root = crate_src_root();
    c.bench_function("extract_directory_ariadne_src", |b| {
        b.iter(|| {
            let mut graph = ariadne_graph::Graph::new();
            extract::extract_directory(&root, &mut graph).expect("extract failed")
        });
    });
}

criterion_group!(benches, bench_extract_directory);
criterion_main!(benches);
