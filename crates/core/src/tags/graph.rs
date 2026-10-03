//! Port of electron/cluster-core.ts. Input edge order and stable tie order are
//! retained; vectors reweight existing co-occurrence edges, never create edges.
use super::{js_cmp, vocabulary};
use crate::repo::{RepoError, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edge {
    pub a: String,
    pub b: String,
    pub c: u64,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Options {
    pub min_jaccard: f64,
    pub max_group_size: usize,
    pub iterations: usize,
    pub alpha: f64,
}
impl Default for Options {
    fn default() -> Self {
        Self {
            min_jaccard: 0.15,
            max_group_size: 14,
            iterations: 6,
            alpha: 0.5,
        }
    }
}
pub type Frequencies = HashMap<String, u64>;
pub type Vectors = HashMap<String, Vec<f64>>;
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct CandidateGroup {
    pub tags: Vec<String>,
    pub neighbors: std::collections::BTreeMap<String, Vec<String>>,
}
// Keep object key order identical to the desktop's insertion in tag order.
impl Serialize for CandidateGroup {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::{SerializeMap, SerializeStruct};
        struct Neighbors<'a>(&'a CandidateGroup);
        impl Serialize for Neighbors<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                let mut map = serializer.serialize_map(Some(self.0.tags.len()))?;
                for tag in &self.0.tags {
                    if let Some(neighbors) = self.0.neighbors.get(tag) {
                        map.serialize_entry(tag, neighbors)?;
                    }
                }
                map.end()
            }
        }
        let mut object = serializer.serialize_struct("CandidateGroup", 2)?;
        object.serialize_field("tags", &self.tags)?;
        object.serialize_field("neighbors", &Neighbors(self))?;
        object.end()
    }
}
pub fn cosine_sim(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    if dot.is_finite() {
        dot.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}
struct Weighted {
    a: usize,
    b: usize,
    w: f64,
}
struct Graph<'a> {
    names: Vec<&'a str>,
    freq: &'a Frequencies,
    edges: Vec<Weighted>,
    iterations: usize,
}
impl Graph<'_> {
    fn sort(&self, nodes: &mut [usize]) {
        nodes.sort_by(|a, b| {
            self.freq
                .get(self.names[*b])
                .unwrap_or(&0)
                .cmp(self.freq.get(self.names[*a]).unwrap_or(&0))
                .then_with(|| js_cmp(self.names[*a], self.names[*b]))
        });
    }
    fn propagate(&self, members: &[usize], threshold: f64) -> Vec<Vec<usize>> {
        let mut included = vec![false; self.names.len()];
        for n in members {
            included[*n] = true;
        }
        let mut adj = vec![Vec::<(usize, f64)>::new(); self.names.len()];
        for e in &self.edges {
            if e.w < threshold || !included[e.a] || !included[e.b] {
                continue;
            }
            // JS Map.set replaces duplicate neighbors without changing order.
            for (a, b) in [(e.a, e.b), (e.b, e.a)] {
                if let Some(hit) = adj[a].iter_mut().find(|(n, _)| *n == b) {
                    hit.1 = e.w;
                } else {
                    adj[a].push((b, e.w));
                }
            }
        }
        let mut nodes: Vec<_> = members
            .iter()
            .copied()
            .filter(|n| !adj[*n].is_empty())
            .collect();
        self.sort(&mut nodes);
        let mut labels: Vec<_> = (0..self.names.len()).collect();
        for _ in 0..self.iterations {
            let mut changed = false;
            for n in &nodes {
                let mut scores = Vec::<(usize, f64)>::new();
                for (m, w) in &adj[*n] {
                    let label = labels[*m];
                    if let Some((_, s)) = scores.iter_mut().find(|(l, _)| *l == label) {
                        *s += w;
                    } else {
                        scores.push((label, *w));
                    }
                }
                let mut best = labels[*n];
                let mut score = f64::NEG_INFINITY;
                for (l, s) in scores {
                    if s > score || (s == score && js_cmp(self.names[l], self.names[best]).is_lt())
                    {
                        best = l;
                        score = s;
                    }
                }
                if labels[*n] != best {
                    labels[*n] = best;
                    changed = true;
                }
            }
            if !changed {
                break;
            }
        }
        let mut groups = Vec::<Vec<usize>>::new();
        let mut positions = HashMap::new();
        for n in nodes {
            let next = groups.len();
            let idx = *positions.entry(labels[n]).or_insert(next);
            if idx == groups.len() {
                groups.push(Vec::new());
            }
            groups[idx].push(n);
        }
        groups
    }
}
/// Byte parity with buildTagCommunities for valid desktop options.
pub fn build_tag_communities(
    freq: &Frequencies,
    edges: &[Edge],
    vectors: Option<&Vectors>,
    opts: Options,
) -> Vec<Vec<String>> {
    if opts.max_group_size < 2 || !opts.min_jaccard.is_finite() || !opts.alpha.is_finite() {
        return Vec::new();
    }
    let mut graph = Graph {
        names: Vec::new(),
        freq,
        edges: Vec::new(),
        iterations: opts.iterations,
    };
    let mut indices = HashMap::new();
    for edge in edges {
        let fa = *freq.get(&edge.a).unwrap_or(&0) as f64;
        let fb = *freq.get(&edge.b).unwrap_or(&0) as f64;
        let c = edge.c as f64;
        let denom = fa + fb - c;
        let j = if denom > 0.0 { c / denom } else { 0.0 };
        let w = if let Some(vectors) = vectors {
            let cos = match (vectors.get(&edge.a), vectors.get(&edge.b)) {
                (Some(a), Some(b)) => cosine_sim(a, b),
                _ => 0.0,
            };
            opts.alpha * j + (1.0 - opts.alpha) * cos.max(0.0)
        } else {
            j
        };
        if w < opts.min_jaccard {
            continue;
        }
        let a = *indices.entry(edge.a.as_str()).or_insert_with(|| {
            graph.names.push(&edge.a);
            graph.names.len() - 1
        });
        let b = *indices.entry(edge.b.as_str()).or_insert_with(|| {
            graph.names.push(&edge.b);
            graph.names.len() - 1
        });
        graph.edges.push(Weighted { a, b, w });
    }
    let nodes: Vec<_> = (0..graph.names.len()).collect();
    let mut queue: VecDeque<_> = graph
        .propagate(&nodes, opts.min_jaccard)
        .into_iter()
        .filter(|g| g.len() >= 2)
        .map(|g| (g, opts.min_jaccard))
        .collect();
    let mut out = Vec::new();
    let mut guard = 0;
    while let Some((members, thr)) = queue.pop_front() {
        guard += 1;
        if guard > 10000 {
            break;
        }
        if members.len() <= opts.max_group_size {
            out.push(members);
            continue;
        }
        let next = thr + 0.1;
        let sub = graph.propagate(&members, next);
        if sub.len() > 1 && sub.iter().all(|g| g.len() < members.len()) && next <= 1.0 {
            for g in sub {
                if g.len() >= 2 {
                    queue.push_back((g, next));
                } else {
                    out.push(g);
                }
            }
        } else {
            let mut sorted = members;
            graph.sort(&mut sorted);
            out.extend(sorted.chunks(opts.max_group_size).map(<[usize]>::to_vec));
        }
    }
    out.retain(|g| g.len() >= 2);
    for g in &mut out {
        graph.sort(g);
    }
    let total = |g: &Vec<usize>| {
        g.iter()
            .map(|n| freq.get(graph.names[*n]).unwrap_or(&0))
            .sum::<u64>()
    };
    out.sort_by_key(|g| std::cmp::Reverse(total(g)));
    out.into_iter()
        .map(|g| g.into_iter().map(|n| graph.names[n].to_owned()).collect())
        .collect()
}
/// Candidate graph using distinct live posts in both layers. Accepted aliases
/// are already canonicalized at writes; no shared user cache exists.
pub fn candidate_groups(
    conn: &Connection,
    vectors: Option<&Vectors>,
    opts: Options,
) -> Result<Vec<CandidateGroup>> {
    if opts.max_group_size < 2
        || opts.max_group_size > 400
        || opts.iterations > 100
        || !opts.min_jaccard.is_finite()
        || !(0.0..=1.0).contains(&opts.min_jaccard)
        || !(0.0..=1.0).contains(&opts.alpha)
    {
        return Err(RepoError::Invalid {
            field: "options",
            reason: "invalid graph limits",
        });
    }
    let freq: Frequencies = vocabulary(conn)?
        .into_iter()
        .map(|v| (v.norm, v.count))
        .collect();
    let edges=conn.prepare_cached("WITH tags AS (SELECT DISTINCT t.post_id, t.tag_norm FROM post_tags t
        JOIN posts p ON p.id=t.post_id WHERE p.deleted_at IS NULL)
        SELECT a.tag_norm,b.tag_norm,COUNT(*) FROM tags a JOIN tags b ON b.post_id=a.post_id AND a.tag_norm<b.tag_norm
        GROUP BY a.tag_norm,b.tag_norm HAVING COUNT(*)>=2 ORDER BY a.tag_norm,b.tag_norm")?
        .query_map([],|r|Ok(Edge {a:r.get(0)?,b:r.get(1)?,c:r.get::<_,i64>(2)? as u64}))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(candidate_groups_from_graph(&freq, &edges, vectors, opts))
}
/// Coverage and compact neighbor context, matching getTagCandidateGroups.
pub fn candidate_groups_from_graph(
    freq: &Frequencies,
    edges: &[Edge],
    vectors: Option<&Vectors>,
    opts: Options,
) -> Vec<CandidateGroup> {
    let mut neighbors: HashMap<&str, Vec<(&str, u64)>> = HashMap::new();
    for e in edges {
        neighbors.entry(&e.a).or_default().push((&e.b, e.c));
        neighbors.entry(&e.b).or_default().push((&e.a, e.c));
    }
    for list in neighbors.values_mut() {
        list.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    }
    let top = |n: &str, tags: &[String]| {
        neighbors
            .get(n)
            .into_iter()
            .flatten()
            .take(4)
            .filter(|(t, _)| !tags.iter().any(|s| s == t))
            .map(|(t, _)| (*t).to_owned())
            .collect::<Vec<_>>()
    };
    let mut groups: Vec<_> = build_tag_communities(freq, edges, vectors, opts)
        .into_iter()
        .map(|tags| {
            let context = tags.iter().map(|t| (t.clone(), top(t, &tags))).collect();
            CandidateGroup {
                tags,
                neighbors: context,
            }
        })
        .collect();
    let mut group_of = HashMap::new();
    for (i, g) in groups.iter().enumerate() {
        for t in &g.tags {
            group_of.insert(t.clone(), i);
        }
    }
    // Frequency input from the desktop SQL is norm ordered; do not iterate HashMap.
    let mut leftover: Vec<_> = freq
        .keys()
        .filter(|t| !group_of.contains_key(*t))
        .cloned()
        .collect();
    leftover.sort_unstable(); // SQLite BINARY order, as the desktop frequency query.
    for t in leftover {
        for (n, _) in neighbors.get(t.as_str()).into_iter().flatten() {
            if let Some(&i) = group_of.get(*n)
                && groups[i].tags.len() < opts.max_group_size
            {
                groups[i].tags.push(t.clone());
                let context = top(&t, &groups[i].tags);
                groups[i].neighbors.insert(t.clone(), context);
                group_of.insert(t, i);
                break;
            }
        }
    }
    groups
}
