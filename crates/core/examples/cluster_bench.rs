//! Synthetic 5,000-tag acceptance benchmark; run with --release. No I/O/data.
use shelfy_core::tags::graph::{self, Edge, Options};
fn main() {
    let freq = (0..5000).map(|i| (format!("tag-{i:04}"), 10)).collect();
    let mut edges = Vec::new();
    for start in (0..5000).step_by(10) {
        for a in start..start + 10 {
            for b in a + 1..start + 10 {
                edges.push(Edge {
                    a: format!("tag-{a:04}"),
                    b: format!("tag-{b:04}"),
                    c: 10,
                });
            }
        }
    }
    let thread = std::thread::spawn(move || {
        let start = std::time::Instant::now();
        let groups = graph::build_tag_communities(&freq, &edges, None, Options::default());
        (start.elapsed(), groups)
    });
    let (elapsed, groups) = thread.join().unwrap();
    assert_eq!(groups.iter().map(Vec::len).sum::<usize>(), 5000);
    println!(
        "5000 tags, 22500 edges, {} groups: {:.3}s",
        groups.len(),
        elapsed.as_secs_f64()
    );
    assert!(elapsed < std::time::Duration::from_secs(2));
}
