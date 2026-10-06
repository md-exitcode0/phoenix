//! Pure-Rust in-memory brute-force `VectorDB` implementation.
//!
//! Linear-scan O(n) similarity search over all stored vectors. Used as:
//! - the Android default (LanceDB + Arrow do not cross-compile cleanly there);
//! - the `vector_db_url = ":memory:"` escape hatch on every target (handy
//!   for tests and ephemeral cognify runs);
//! - a test fixture in lieu of the testing-feature-gated `MockVectorDB`.
//!
//! **No persistence.** Data is lost on process restart. For durable storage
//! prefer the default `LanceDbAdapter` (on non-Android targets) or
//! `vector_db_provider="pgvector"`.
//!
//! **Memory:** O(n × dim). At ~6 GB for 1M × 1536-dim, this is a
//! soft cap — beyond that, pgvector (or the closed `cognee-vector-qdrant`)
//! is the correct choice.
//!
//! **Distance metric:** every collection uses cosine similarity
//! (higher = more similar). The `VectorDB` trait's
//! `create_collection(data_type, field_name, dimension)` does not carry
//! a `DistanceMetric`; per-collection metric plumbing is beyond T5's
//! scope.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::error::{VectorDBError, VectorDBResult};
use crate::models::{SearchResult, VectorPoint};
use crate::vector_db_trait::VectorDB;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Collection {
    dimension: usize,
    points: Vec<VectorPoint>,
}

/// In-memory brute-force vector database.
///
/// All collections are held in a single `tokio::sync::RwLock`. Cloning
/// the struct shares the same underlying storage (`Arc`-backed), so it
/// is safe to hand out to multiple async tasks.
///
/// **Optional persistence** ([`Self::with_persistence`]): collections are
/// snapshotted to one JSON file after every mutation (atomic tmp+rename)
/// and reloaded at construction. Without it, a process restart silently
/// discards every embedding while the graph/relational stores survive —
/// recall then reports a cold store forever (live 2026-07-06: a completed
/// 92-doc cognify run left recall empty because the vectors died with the
/// CLI process).
#[derive(Debug, Clone, Default)]
pub struct BruteForceVectorDB {
    collections: Arc<RwLock<HashMap<String, Collection>>>,
    persist_path: Option<Arc<std::path::PathBuf>>,
}

impl BruteForceVectorDB {
    /// Construct an empty in-memory vector database.
    pub fn new() -> Self {
        Self {
            collections: Arc::new(RwLock::new(HashMap::new())),
            persist_path: None,
        }
    }

    /// Construct a disk-backed vector database: load the snapshot at `path`
    /// if present, and rewrite it after every mutation. A corrupt/unreadable
    /// snapshot logs and starts empty (never fails construction) — the store
    /// can always be rebuilt by re-indexing.
    pub fn with_persistence(path: std::path::PathBuf) -> Self {
        let collections = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<HashMap<String, Collection>>(&bytes) {
                Ok(map) => map,
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        "vector snapshot unreadable — starting empty (re-index to rebuild): {e}"
                    );
                    HashMap::new()
                }
            },
            Err(_) => HashMap::new(), // absent file = first run
        };
        Self {
            collections: Arc::new(RwLock::new(collections)),
            persist_path: Some(Arc::new(path)),
        }
    }

    /// Mirror `MockVectorDB::collection_key`: `"{data_type}_{field_name}"`.
    fn key(data_type: &str, field_name: &str) -> String {
        format!("{data_type}_{field_name}")
    }

    /// Snapshot the store to `persist_path` (no-op when memory-only).
    /// Called under the write guard by every mutating op, so snapshots are
    /// serialized and always consistent. Atomic tmp+rename; a failed write
    /// warns and continues — persistence is best-effort, search still works.
    fn save(&self, collections: &HashMap<String, Collection>) {
        let Some(path) = self.persist_path.as_deref() else {
            return;
        };
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec(collections)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, path)
        };
        if let Err(e) = write() {
            tracing::warn!(path = %path.display(), "vector snapshot write failed: {e}");
        }
    }
}

/// Cosine similarity in `[-1.0, 1.0]`. Higher = more similar.
///
/// `EPSILON` guards the denominator against zero-magnitude inputs; we
/// deliberately do not special-case NaN (matches `MockVectorDB`).
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "cosine_similarity inputs must match");
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    let denom = (na.sqrt() * nb.sqrt()).max(f32::EPSILON);
    dot / denom
}

#[async_trait]
impl VectorDB for BruteForceVectorDB {
    async fn create_collection(
        &self,
        data_type: &str,
        field_name: &str,
        dimension: usize,
    ) -> VectorDBResult<()> {
        let key = Self::key(data_type, field_name);
        let mut g = self.collections.write().await;
        if g.contains_key(&key) {
            return Err(VectorDBError::CollectionExists(key));
        }
        g.insert(
            key,
            Collection {
                dimension,
                points: Vec::new(),
            },
        );
        self.save(&g);
        Ok(())
    }

    async fn has_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<bool> {
        let g = self.collections.read().await;
        Ok(g.contains_key(&Self::key(data_type, field_name)))
    }

    async fn index_points(
        &self,
        data_type: &str,
        field_name: &str,
        points: &[VectorPoint],
    ) -> VectorDBResult<()> {
        if points.is_empty() {
            return Ok(());
        }
        let key = Self::key(data_type, field_name);
        let mut g = self.collections.write().await;
        let coll = g
            .get_mut(&key)
            .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;

        // Validate dimensions before mutating storage.
        for p in points {
            if p.vector.len() != coll.dimension {
                return Err(VectorDBError::DimensionMismatch {
                    collection: key.clone(),
                    expected: coll.dimension,
                    actual: p.vector.len(),
                });
            }
        }

        // Upsert by id: replace existing, otherwise append. On replace, union
        // dataset membership so a content-addressed point indexed under several
        // datasets stays retrievable for all of them (cross-dataset dedup).
        for p in points {
            if let Some(existing) = coll.points.iter_mut().find(|x| x.id == p.id) {
                let mut merged = p.clone();
                merged.merge_dataset_membership(existing);
                *existing = merged;
            } else {
                coll.points.push(p.clone());
            }
        }
        self.save(&g);
        Ok(())
    }

    async fn search_similar(
        &self,
        data_type: &str,
        field_name: &str,
        query_vector: &[f32],
        top_k: usize,
    ) -> VectorDBResult<Vec<SearchResult>> {
        let key = Self::key(data_type, field_name);
        // Score under the read guard, then drop it before sorting + result
        // construction so we never hold the lock across the (synchronous,
        // but still post-await) sort/truncate step.
        let mut scored: Vec<(Uuid, f32, HashMap<String, serde_json::Value>)> = {
            let g = self.collections.read().await;
            let coll = g
                .get(&key)
                .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;
            if query_vector.len() != coll.dimension {
                return Err(VectorDBError::DimensionMismatch {
                    collection: key.clone(),
                    expected: coll.dimension,
                    actual: query_vector.len(),
                });
            }
            coll.points
                .iter()
                .map(|p| {
                    (
                        p.id,
                        cosine_similarity(&p.vector, query_vector),
                        p.metadata.clone(),
                    )
                })
                .collect()
        };

        // Higher score first (descending). `total_cmp` orders NaN deterministically.
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(top_k);
        Ok(scored
            .into_iter()
            .map(|(id, score, metadata)| SearchResult {
                id,
                score,
                metadata,
            })
            .collect())
    }

    async fn delete_collection(&self, data_type: &str, field_name: &str) -> VectorDBResult<()> {
        let mut g = self.collections.write().await;
        g.remove(&Self::key(data_type, field_name));
        self.save(&g);
        Ok(())
    }

    async fn delete_points(
        &self,
        data_type: &str,
        field_name: &str,
        point_ids: &[Uuid],
    ) -> VectorDBResult<()> {
        let key = Self::key(data_type, field_name);
        let mut g = self.collections.write().await;
        let coll = g
            .get_mut(&key)
            .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;
        coll.points.retain(|p| !point_ids.contains(&p.id));
        self.save(&g);
        Ok(())
    }

    async fn collection_size(&self, data_type: &str, field_name: &str) -> VectorDBResult<usize> {
        let key = Self::key(data_type, field_name);
        let g = self.collections.read().await;
        let coll = g
            .get(&key)
            .ok_or_else(|| VectorDBError::CollectionNotFound(key.clone()))?;
        Ok(coll.points.len())
    }

    async fn list_collections(&self) -> VectorDBResult<Vec<(String, String)>> {
        let g = self.collections.read().await;
        let mut out: Vec<(String, String)> = g
            .keys()
            .filter_map(|k| {
                k.split_once('_')
                    .map(|(a, b)| (a.to_string(), b.to_string()))
            })
            .collect();
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test code — panics are acceptable"
)]
mod tests {
    use super::*;
    use std::collections::HashMap as Hm;
    use uuid::Uuid;

    fn point(id_seed: u128, v: Vec<f32>) -> VectorPoint {
        VectorPoint {
            id: Uuid::from_u128(id_seed),
            vector: v,
            metadata: Hm::new(),
        }
    }

    #[tokio::test]
    async fn create_then_has_collection() {
        let db = BruteForceVectorDB::new();
        assert!(!db.has_collection("T", "f").await.unwrap());
        db.create_collection("T", "f", 4).await.unwrap();
        assert!(db.has_collection("T", "f").await.unwrap());
    }

    #[tokio::test]
    async fn create_duplicate_returns_exists() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 4).await.unwrap();
        let err = db.create_collection("T", "f", 4).await.unwrap_err();
        assert!(
            matches!(err, VectorDBError::CollectionExists(ref k) if k == "T_f"),
            "expected CollectionExists, got {err:?}",
        );
    }

    #[tokio::test]
    async fn index_dim_mismatch_returns_error() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 3).await.unwrap();
        let p = point(1, vec![1.0, 2.0]); // dim 2, expected 3
        let err = db.index_points("T", "f", &[p]).await.unwrap_err();
        assert!(
            matches!(
                err,
                VectorDBError::DimensionMismatch {
                    expected: 3,
                    actual: 2,
                    ..
                }
            ),
            "expected DimensionMismatch 3 vs 2, got {err:?}",
        );
    }

    #[tokio::test]
    async fn index_replaces_by_id() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 2).await.unwrap();
        let p_v1 = point(1, vec![1.0, 0.0]);
        let p_v2 = point(1, vec![0.0, 1.0]); // same id, new vector
        db.index_points("T", "f", &[p_v1]).await.unwrap();
        db.index_points("T", "f", &[p_v2]).await.unwrap();
        assert_eq!(db.collection_size("T", "f").await.unwrap(), 1);

        // Query for [0,1] → the upserted vector should score 1.0.
        let results = db.search_similar("T", "f", &[0.0, 1.0], 1).await.unwrap();
        assert_eq!(results.len(), 1);
        assert!(
            (results[0].score - 1.0).abs() < 1e-5,
            "upserted vector should score 1.0, got {}",
            results[0].score
        );
    }

    #[tokio::test]
    async fn search_ranks_descending() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 3).await.unwrap();
        let a = point(1, vec![1.0, 0.0, 0.0]);
        let b = point(2, vec![0.0, 1.0, 0.0]);
        let c = point(3, vec![0.0, 0.0, 1.0]);
        db.index_points("T", "f", &[a, b, c]).await.unwrap();

        let results = db
            .search_similar("T", "f", &[1.0, 0.0, 0.0], 3)
            .await
            .unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, Uuid::from_u128(1), "A should rank first");
        // Tail order is implementation-defined for ties (both 0.0) under
        // total_cmp; just assert descending and that A wins.
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
        assert!(
            (results[0].score - 1.0).abs() < 1e-5,
            "self-similarity should be ~1.0, got {}",
            results[0].score
        );
    }

    #[tokio::test]
    async fn search_empty_collection_returns_empty() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 3).await.unwrap();
        let results = db
            .search_similar("T", "f", &[1.0, 0.0, 0.0], 5)
            .await
            .unwrap();
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn search_query_dim_mismatch_returns_error() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 3).await.unwrap();
        let err = db
            .search_similar("T", "f", &[1.0, 0.0], 5)
            .await
            .unwrap_err();
        assert!(
            matches!(
                err,
                VectorDBError::DimensionMismatch {
                    expected: 3,
                    actual: 2,
                    ..
                }
            ),
            "expected DimensionMismatch, got {err:?}",
        );
    }

    #[tokio::test]
    async fn delete_points_removes_matching_ids() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 2).await.unwrap();
        let a = point(1, vec![1.0, 0.0]);
        let b = point(2, vec![0.0, 1.0]);
        let c = point(3, vec![1.0, 1.0]);
        db.index_points("T", "f", &[a, b, c]).await.unwrap();
        db.delete_points("T", "f", &[Uuid::from_u128(1), Uuid::from_u128(3)])
            .await
            .unwrap();
        assert_eq!(db.collection_size("T", "f").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn delete_collection_is_idempotent() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 2).await.unwrap();
        db.delete_collection("T", "f").await.unwrap();
        // Deleting again should not error.
        db.delete_collection("T", "f").await.unwrap();
        assert!(!db.has_collection("T", "f").await.unwrap());
    }

    #[tokio::test]
    async fn list_collections_returns_pairs() {
        let db = BruteForceVectorDB::new();
        let empty = db.list_collections().await.unwrap();
        assert!(empty.is_empty());

        db.create_collection("DocumentChunk", "text", 3)
            .await
            .unwrap();
        db.create_collection("Entity", "name", 3).await.unwrap();

        let pairs = db.list_collections().await.unwrap();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.contains(&("DocumentChunk".to_string(), "text".to_string())));
        assert!(pairs.contains(&("Entity".to_string(), "name".to_string())));
    }

    #[tokio::test]
    async fn collection_size_after_upsert() {
        let db = BruteForceVectorDB::new();
        db.create_collection("T", "f", 2).await.unwrap();
        assert_eq!(db.collection_size("T", "f").await.unwrap(), 0);
        db.index_points(
            "T",
            "f",
            &[point(1, vec![1.0, 0.0]), point(2, vec![0.0, 1.0])],
        )
        .await
        .unwrap();
        assert_eq!(db.collection_size("T", "f").await.unwrap(), 2);
        // Re-upsert same id 1; size stays 2.
        db.index_points("T", "f", &[point(1, vec![0.5, 0.5])])
            .await
            .unwrap();
        assert_eq!(db.collection_size("T", "f").await.unwrap(), 2);
    }

    #[tokio::test]
    async fn collection_size_unknown_collection_errors() {
        let db = BruteForceVectorDB::new();
        let err = db.collection_size("T", "f").await.unwrap_err();
        assert!(
            matches!(err, VectorDBError::CollectionNotFound(_)),
            "expected CollectionNotFound, got {err:?}",
        );
    }

    #[tokio::test]
    async fn persistence_survives_reopen() {
        // The live 2026-07-06 failure: a clean cognify run, then every vector
        // gone with the process — recall cold forever. A persistent store must
        // hand back indexed points to a brand-new instance on the same path.
        let dir = std::env::temp_dir().join(format!("bf-vec-test-{}", Uuid::new_v4()));
        let path = dir.join("vectors.json");

        let db = BruteForceVectorDB::with_persistence(path.clone());
        db.create_collection("DocumentChunk", "text", 2)
            .await
            .unwrap();
        db.index_points(
            "DocumentChunk",
            "text",
            &[point(1, vec![1.0, 0.0]), point(2, vec![0.0, 1.0])],
        )
        .await
        .unwrap();
        drop(db);

        // Fresh instance = fresh process, same snapshot.
        let reopened = BruteForceVectorDB::with_persistence(path.clone());
        assert_eq!(
            reopened
                .collection_size("DocumentChunk", "text")
                .await
                .unwrap(),
            2
        );
        let hits = reopened
            .search_similar("DocumentChunk", "text", &[1.0, 0.0], 1)
            .await
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].score > 0.99);

        // Deletes persist too.
        reopened
            .delete_collection("DocumentChunk", "text")
            .await
            .unwrap();
        let after_delete = BruteForceVectorDB::with_persistence(path);
        assert!(
            !after_delete
                .has_collection("DocumentChunk", "text")
                .await
                .unwrap(),
            "deleted collection came back from the snapshot"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn persistence_corrupt_snapshot_starts_empty() {
        let dir = std::env::temp_dir().join(format!("bf-vec-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vectors.json");
        std::fs::write(&path, b"not json{{{").unwrap();
        let db = BruteForceVectorDB::with_persistence(path);
        assert!(!db.has_collection("T", "f").await.unwrap());
        let _ = std::fs::remove_dir_all(dir);
    }
}
