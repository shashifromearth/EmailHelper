//! Hybrid search: vector similarity + keyword (BM25) + metadata filters,
//! merged with reciprocal-rank fusion.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::embed::{Embedder, Kind};
use crate::error::Result;
use crate::store::{self, Store, StoredChunk};

/// Candidates taken from each retriever before filtering.
const CANDIDATES: usize = 80;
/// Reciprocal-rank-fusion constant.
const RRF_K: f64 = 60.0;
/// Extra score for chunks from the requested stack's resume.
const STACK_BOOST: f64 = 0.005;
const QUERY_CACHE: usize = 512;
/// A model's vectors are used only when they cover this share of the chunks
/// (a half-embedded model would hide the rest of the knowledge).
const MIN_COVERAGE: f64 = 0.95;
/// At most this many chunks from one document in the results.
const PER_DOCUMENT: usize = 2;

/// Lower-cased letters/digits only, for spotting near-identical chunks
/// (the same summary in several resume variants).
fn fingerprint(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).take(240).collect()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Query {
    pub query: String,
    /// Only this person's knowledge (plus shared/). None = everyone.
    #[serde(default)]
    pub person: Option<String>,
    /// job_seeking / consulting / freelancing: other purposes' files are left out.
    #[serde(default)]
    pub purpose: Option<String>,
    /// Prefer this stack's resume (dotnet, python, …).
    #[serde(default)]
    pub stack: Option<String>,
    /// Only these document types (resume, skills, compensation, …).
    #[serde(default)]
    pub doc_types: Vec<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    #[serde(flatten)]
    pub chunk: StoredChunk,
    pub score: f64,
    /// Which retrievers found it: "vector", "keyword".
    pub via: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Results {
    pub hits: Vec<Hit>,
    /// Embedding model used for the vector part (None = keyword only).
    pub model: Option<String>,
}

pub struct Searcher {
    pub store: Store,
    /// In preference order (Gemini first, local fallback).
    pub embedders: Vec<Arc<dyn Embedder>>,
    cache: Mutex<HashMap<(String, String), Vec<f32>>>,
}

/// Does a chunk belong to the query's scope?
pub fn in_scope(c: &StoredChunk, q: &Query) -> bool {
    if let Some(p) = &q.person {
        // a person's own files, or shared ones
        if c.person_id.as_deref().is_some_and(|x| x != p) {
            return false;
        }
    }
    if let (Some(want), Some(have)) = (&q.purpose, &c.purpose) {
        if want != have {
            return false;
        }
    }
    q.doc_types.is_empty() || q.doc_types.iter().any(|d| d == &c.doc_type)
}

/// Fuse ranked id lists: score = Σ 1 / (k + rank).
pub fn rrf(lists: &[(&'static str, Vec<i64>)]) -> Vec<(i64, f64, Vec<&'static str>)> {
    let mut acc: HashMap<i64, (f64, Vec<&'static str>)> = HashMap::new();
    for (name, ids) in lists {
        for (rank, id) in ids.iter().enumerate() {
            let e = acc.entry(*id).or_insert((0.0, Vec::new()));
            e.0 += 1.0 / (RRF_K + rank as f64 + 1.0);
            e.1.push(name);
        }
    }
    let mut v: Vec<(i64, f64, Vec<&'static str>)> = acc.into_iter().map(|(id, (s, via))| (id, s, via)).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

impl Searcher {
    pub fn new(store: Store, embedders: Vec<Arc<dyn Embedder>>) -> Self {
        Self { store, embedders, cache: Mutex::new(HashMap::new()) }
    }

    /// Models in search order: those covering nearly all chunks first (in
    /// preference order), then the rest by coverage.
    async fn usable_models(&self) -> Vec<Arc<dyn Embedder>> {
        let ids: Vec<String> = self.embedders.iter().map(|e| e.id().to_string()).collect();
        let counts = self
            .store
            .call(move |c| {
                let total: i64 = store::status(c)?.chunks;
                let per: Vec<i64> = ids.iter().map(|m| store::embedded_count(c, m).unwrap_or(0)).collect();
                Ok((total, per))
            })
            .await;
        let Ok((total, per)) = counts else { return vec![] };
        if total == 0 {
            return vec![];
        }
        let mut ranked: Vec<(bool, i64, usize)> = per
            .iter()
            .enumerate()
            .filter(|(_, n)| **n > 0)
            .map(|(i, n)| ((*n as f64 / total as f64) >= MIN_COVERAGE, *n, i))
            .collect();
        // full coverage first, keeping preference order; then by coverage
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(if a.0 && b.0 { a.2.cmp(&b.2) } else { b.1.cmp(&a.1) }));
        ranked.into_iter().map(|(_, _, i)| self.embedders[i].clone()).collect()
    }

    /// Embed the query with the best usable model that answers.
    async fn query_vector(&self, text: &str) -> Option<(String, Vec<f32>)> {
        for e in self.usable_models().await {
            let model = e.id().to_string();
            let key = (model.clone(), text.to_string());
            if let Some(v) = self.cache.lock().ok().and_then(|c| c.get(&key).cloned()) {
                return Some((model, v));
            }
            match e.embed(&[text.to_string()], Kind::Query).await {
                Ok(mut v) if !v.is_empty() => {
                    let vec = v.remove(0);
                    if let Ok(mut c) = self.cache.lock() {
                        if c.len() >= QUERY_CACHE {
                            c.clear();
                        }
                        c.insert(key, vec.clone());
                    }
                    return Some((model, vec));
                }
                Ok(_) => {}
                Err(err) => tracing::warn!(model = %model, error = %err, "query embedding failed; trying the next model"),
            }
        }
        None
    }

    pub async fn search(&self, q: &Query) -> Result<Results> {
        let limit = q.limit.unwrap_or(6).clamp(1, 50);
        let vector = self.query_vector(&q.query).await;
        let fts = eh_knowledge::text::fts_query(&q.query, 24);
        let (model, qvec) = match vector {
            Some((m, v)) => (Some(m), Some(v)),
            None => (None, None),
        };
        let m2 = model.clone();
        let (vec_ids, kw_ids) = self
            .store
            .call(move |c| {
                let v = match (&m2, &qvec) {
                    (Some(m), Some(v)) => store::knn(c, m, v, CANDIDATES)?,
                    _ => vec![],
                };
                let k = match &fts {
                    Some(f) => store::keyword(c, f, CANDIDATES)?,
                    None => vec![],
                };
                Ok((v, k))
            })
            .await?;
        let mut all: Vec<i64> = vec_ids.iter().map(|x| x.0).chain(kw_ids.iter().map(|x| x.0)).collect();
        all.sort_unstable();
        all.dedup();
        let chunks = self.store.call(move |c| store::chunks_by_id(c, &all)).await?;
        let by_id: HashMap<i64, StoredChunk> = chunks.into_iter().filter(|c| in_scope(c, q)).map(|c| (c.id, c)).collect();
        let keep = |ids: Vec<(i64, f64)>| ids.into_iter().map(|x| x.0).filter(|id| by_id.contains_key(id)).collect::<Vec<_>>();
        let fused = rrf(&[("vector", keep(vec_ids)), ("keyword", keep(kw_ids))]);
        let mut hits: Vec<Hit> = fused
            .into_iter()
            .filter_map(|(id, score, via)| {
                let chunk = by_id.get(&id)?.clone();
                let boost = match (&q.stack, &chunk.stack) {
                    (Some(want), Some(have)) if want.eq_ignore_ascii_case(have) => STACK_BOOST,
                    _ => 0.0,
                };
                Some(Hit { chunk, score: score + boost, via })
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        // Diversity: skip near-duplicates and cap chunks per document.
        let mut seen = std::collections::HashSet::new();
        let mut per_doc: HashMap<String, usize> = HashMap::new();
        hits.retain(|h| {
            let n = per_doc.entry(h.chunk.path.clone()).or_default();
            if *n >= PER_DOCUMENT || !seen.insert(fingerprint(&h.chunk.content)) {
                return false;
            }
            *n += 1;
            true
        });
        hits.truncate(limit);
        Ok(Results { hits, model })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(id: i64, person: Option<&str>, purpose: Option<&str>, doc_type: &str) -> StoredChunk {
        StoredChunk {
            id,
            path: format!("p{id}"),
            person_id: person.map(str::to_string),
            purpose: purpose.map(str::to_string),
            doc_type: doc_type.into(),
            stack: None,
            section: String::new(),
            title: String::new(),
            content: String::new(),
        }
    }

    #[test]
    fn near_duplicates_share_a_fingerprint() {
        assert_eq!(fingerprint("Solution Architect, 12+ years."), fingerprint("solution architect 12 years"));
        assert_ne!(fingerprint("Python"), fingerprint("Rust"));
    }

    #[test]
    fn rrf_rewards_agreement() {
        let f = rrf(&[("vector", vec![1, 2, 3]), ("keyword", vec![3, 4])]);
        assert_eq!(f[0].0, 3, "found by both retrievers ranks first: {f:?}");
        assert_eq!(f[0].2, vec!["vector", "keyword"]);
    }

    #[test]
    fn scope_keeps_own_and_shared_only() {
        let q = Query { person: Some("asha".into()), purpose: Some("consulting".into()), ..Default::default() };
        assert!(in_scope(&chunk(1, Some("asha"), None, "skills"), &q));
        assert!(in_scope(&chunk(2, None, None, "shared"), &q), "shared knowledge is for everyone");
        assert!(in_scope(&chunk(3, Some("asha"), Some("consulting"), "about"), &q));
        assert!(!in_scope(&chunk(4, Some("asha"), Some("freelancing"), "about"), &q), "other purposes excluded");
        assert!(!in_scope(&chunk(5, Some("ravi"), None, "skills"), &q), "other people never");
        let typed = Query { doc_types: vec!["compensation".into()], ..Default::default() };
        assert!(!in_scope(&chunk(6, None, None, "skills"), &typed));
    }
}
