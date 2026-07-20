//! LanceDB-backed vector store — disk-backed ANN vector store.
//! Stores vectors in a LanceDB table with cosine distance indexing.

use std::path::PathBuf;
use std::sync::Arc;

use lancedb::arrow::arrow_array::{
    FixedSizeListArray, Float32Array, RecordBatch, StringArray, UInt64Array,
};
use lancedb::arrow::arrow_schema::{DataType, Field, Schema};
use lancedb::query::{ExecutableQuery, QueryBase};
use uuid::Uuid;

use super::store_trait::VectorStore;
use super::types::{ChunkType, MemoryChunk, ScoredChunk};
use everevo_core::memory::{ProjectionMetadata, SourcePointer};
use everevo_core::EverEvoError;

/// Disk-backed vector store powered by LanceDB with ANN indexing.
pub struct LanceDBStore {
    rt: tokio::runtime::Runtime,
    uri: String,
    table_name: String,
    dim: usize,
}

impl LanceDBStore {
    /// Open or create a LanceDB-backed vector store.
    pub fn open(path: impl Into<PathBuf>, dim: usize) -> Result<Self, EverEvoError> {
        let uri = path.into().to_string_lossy().to_string();
        let rt = tokio::runtime::Runtime::new()
            .map_err(|e| EverEvoError::Internal(format!("tokio runtime: {e}")))?;
        let store = Self {
            rt,
            uri,
            table_name: "chunks".into(),
            dim,
        };
        store.ensure_table()?;
        tracing::info!(dim = dim, table = %store.table_name, "LanceDBStore opened");
        Ok(store)
    }

    pub fn dimension(&self) -> usize {
        self.dim
    }

    fn ensure_table(&self) -> Result<(), EverEvoError> {
        self.rt.block_on(async {
            let db = lancedb::connect(&self.uri).execute().await.map_err(|e| {
                EverEvoError::Vector(format!("connect: {e}"))
            })?;
            match db.open_table(&self.table_name).execute().await {
                Ok(_) => {
                    tracing::debug!("Opened existing LanceDB table '{}'", self.table_name);
                    Ok(())
                }
                Err(_) => {
                    let schema = self.make_arrow_schema();
                    let empty = RecordBatch::new_empty(Arc::new(schema));
                    let tbl = db
                        .create_table(&self.table_name, vec![empty])
                        .execute()
                        .await
                        .map_err(|e| EverEvoError::Vector(format!("create_table: {e}")))?;
                    tbl.create_index(&["vector"], lancedb::index::Index::Auto)
                        .execute()
                        .await
                        .map_err(|e| EverEvoError::Vector(format!("create_index: {e}")))?;
                    tracing::info!(
                        "Created LanceDB table '{}' with cosine index (dim={})",
                        self.table_name,
                        self.dim
                    );
                    Ok(())
                }
            }
        })
    }

    fn make_arrow_schema(&self) -> Schema {
        Schema::new(vec![
            Field::new("id", DataType::Utf8, false),
            Field::new(
                "vector",
                DataType::FixedSizeList(
                    Arc::new(Field::new("item", DataType::Float32, true)),
                    self.dim as i32,
                ),
                false,
            ),
            Field::new("content", DataType::Utf8, false),
            Field::new("source_pointers", DataType::Utf8, false),
            Field::new("projection", DataType::Utf8, false),
            Field::new("chunk_type", DataType::Utf8, false),
            Field::new("created_at", DataType::Utf8, false),
            Field::new("retrieval_count", DataType::UInt64, false),
        ])
    }

    fn chunks_to_record_batch(&self, chunks: &[MemoryChunk]) -> Result<RecordBatch, EverEvoError> {
        let n = chunks.len();
        let schema = Arc::new(self.make_arrow_schema());

        let ids: Vec<String> = chunks.iter().map(|c| c.id.to_string()).collect();
        let id_array = StringArray::from(ids);

        let mut flat_values = Vec::with_capacity(n * self.dim);
        for chunk in chunks {
            if chunk.vector.len() != self.dim {
                return Err(EverEvoError::InvalidInput(format!(
                    "expected vector dim {}, got {}",
                    self.dim,
                    chunk.vector.len()
                )));
            }
            flat_values.extend_from_slice(&chunk.vector);
        }
        let float_array = Float32Array::from(flat_values);
        let vector_array = FixedSizeListArray::new(
            Arc::new(Field::new("item", DataType::Float32, true)),
            self.dim as i32,
            Arc::new(float_array),
            None,
        );

        let contents: Vec<&str> = chunks.iter().map(|c| c.content.as_str()).collect();
        let content_array = StringArray::from(contents);

        let source_ptrs: Vec<String> = chunks
            .iter()
            .map(|c| serde_json::to_string(&c.source_pointers).unwrap_or_default())
            .collect();
        let source_ptrs_refs: Vec<&str> = source_ptrs.iter().map(String::as_str).collect();
        let source_ptrs_array = StringArray::from(source_ptrs_refs);

        let projections: Vec<String> = chunks
            .iter()
            .map(|c| serde_json::to_string(&c.projection).unwrap_or_default())
            .collect();
        let projection_refs: Vec<&str> = projections.iter().map(String::as_str).collect();
        let projection_array = StringArray::from(projection_refs);

        let chunk_types: Vec<&str> = chunks.iter().map(|c| c.chunk_type.as_str()).collect();
        let chunk_type_array = StringArray::from(chunk_types);

        let created_ats: Vec<String> =
            chunks.iter().map(|c| c.created_at.to_rfc3339()).collect();
        let created_at_refs: Vec<&str> = created_ats.iter().map(String::as_str).collect();
        let created_at_array = StringArray::from(created_at_refs);

        let retrieval_counts: Vec<u64> = chunks.iter().map(|c| c.retrieval_count).collect();
        let retrieval_count_array = UInt64Array::from(retrieval_counts);

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(id_array),
                Arc::new(vector_array),
                Arc::new(content_array),
                Arc::new(source_ptrs_array),
                Arc::new(projection_array),
                Arc::new(chunk_type_array),
                Arc::new(created_at_array),
                Arc::new(retrieval_count_array),
            ],
        )
        .map_err(|e| EverEvoError::Internal(format!("RecordBatch: {e}")))
    }

    fn record_batch_to_chunks(batch: &RecordBatch) -> Result<Vec<MemoryChunk>, EverEvoError> {
        let field_err =
            |name: &str| EverEvoError::Internal(format!("schema missing field: {name}"));
        let schema = batch.schema();
        let num_rows = batch.num_rows();

        let id_array = batch
            .column(schema.index_of("id").map_err(|_| field_err("id"))?)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| EverEvoError::Internal("id column is not StringArray".into()))?;

        let vector_array = batch
            .column(schema.index_of("vector").map_err(|_| field_err("vector"))?)
            .as_any()
            .downcast_ref::<FixedSizeListArray>()
            .ok_or_else(|| {
                EverEvoError::Internal("vector column is not FixedSizeListArray".into())
            })?;
        let dim = vector_array.value_length() as usize;
        let float_values = vector_array
            .values()
            .as_any()
            .downcast_ref::<Float32Array>()
            .ok_or_else(|| {
                EverEvoError::Internal("vector inner is not Float32Array".into())
            })?;

        let content_array = batch
            .column(schema.index_of("content").map_err(|_| field_err("content"))?)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| EverEvoError::Internal("content column is not StringArray".into()))?;

        let source_ptrs_array = batch
            .column(schema.index_of("source_pointers").map_err(|_| field_err("source_pointers"))?)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| {
                EverEvoError::Internal("source_pointers column is not StringArray".into())
            })?;

        let projection_array = batch
            .column(schema.index_of("projection").map_err(|_| field_err("projection"))?)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| {
                EverEvoError::Internal("projection column is not StringArray".into())
            })?;

        let chunk_type_array = batch
            .column(schema.index_of("chunk_type").map_err(|_| field_err("chunk_type"))?)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| {
                EverEvoError::Internal("chunk_type column is not StringArray".into())
            })?;

        let created_at_array = batch
            .column(schema.index_of("created_at").map_err(|_| field_err("created_at"))?)
            .as_any()
            .downcast_ref::<StringArray>()
            .ok_or_else(|| {
                EverEvoError::Internal("created_at column is not StringArray".into())
            })?;

        let retrieval_count_array = batch
            .column(
                schema
                    .index_of("retrieval_count")
                    .map_err(|_| field_err("retrieval_count"))?,
            )
            .as_any()
            .downcast_ref::<UInt64Array>()
            .ok_or_else(|| {
                EverEvoError::Internal("retrieval_count column is not UInt64Array".into())
            })?;

        let mut chunks = Vec::with_capacity(num_rows);
        for i in 0..num_rows {
            let id_str = id_array.value(i);
            let id = Uuid::parse_str(id_str)
                .map_err(|e| EverEvoError::Internal(format!("parse uuid '{id_str}': {e}")))?;
            let vector: Vec<f32> = float_values
                .values()
                .iter()
                .skip(i * dim)
                .take(dim)
                .copied()
                .collect();
            let content = content_array.value(i).to_string();
            let source_pointers: Vec<SourcePointer> =
                serde_json::from_str(source_ptrs_array.value(i)).unwrap_or_default();
            let projection: ProjectionMetadata =
                serde_json::from_str(projection_array.value(i))
                    .unwrap_or_else(|_| ProjectionMetadata::new("0.0.0", "unknown", vec![], 0.0));
            let chunk_type = ChunkType::from_str(chunk_type_array.value(i));
            let created_at =
                chrono::DateTime::parse_from_rfc3339(created_at_array.value(i))
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .unwrap_or_else(|_| chrono::Utc::now());
            let retrieval_count = retrieval_count_array.value(i);

            chunks.push(MemoryChunk {
                id,
                content,
                vector,
                source_pointers,
                projection,
                chunk_type,
                created_at,
                retrieval_count,
            });
        }
        Ok(chunks)
    }
}

impl VectorStore for LanceDBStore {
    fn insert(&self, chunks: Vec<MemoryChunk>) -> Result<(), EverEvoError> {
        if chunks.is_empty() {
            return Ok(());
        }
        let batch = self.chunks_to_record_batch(&chunks)?;
        self.rt.block_on(async {
            let db = lancedb::connect(&self.uri).execute().await.map_err(|e| {
                EverEvoError::Vector(format!("connect: {e}"))
            })?;
            let table = db.open_table(&self.table_name).execute().await.map_err(|e| {
                EverEvoError::Vector(format!("open_table: {e}"))
            })?;
            table
                .add(vec![batch])
                .execute()
                .await
                .map_err(|e| EverEvoError::Vector(format!("add: {e}")))?;
            Ok(())
        })
    }

    fn search(
        &self,
        query_vector: &[f32],
        top_k: usize,
    ) -> Result<Vec<ScoredChunk>, EverEvoError> {
        if query_vector.len() != self.dim {
            return Err(EverEvoError::InvalidInput(format!(
                "query vector dim {} != store dim {}",
                query_vector.len(),
                self.dim
            )));
        }
        let qv = query_vector.to_vec();
        let k = top_k;

        self.rt.block_on(async {
            let db = lancedb::connect(&self.uri).execute().await.map_err(|e| {
                EverEvoError::Vector(format!("connect: {e}"))
            })?;
            let table = db.open_table(&self.table_name).execute().await.map_err(|e| {
                EverEvoError::Vector(format!("open_table: {e}"))
            })?;
            let results = table
                .query()
                .nearest_to(qv)
                .map_err(|e| EverEvoError::Vector(format!("nearest_to: {e}")))?
                .distance_type(lancedb::DistanceType::Cosine)
                .limit(k)
                .execute()
                .await
                .map_err(|e| EverEvoError::Vector(format!("search execute: {e}")))?;

            let mut batches: Vec<RecordBatch> = Vec::new();
            {
                futures::pin_mut!(results);
                loop {
                    match futures::StreamExt::next(&mut results).await {
                        Some(Ok(batch)) => batches.push(batch),
                        Some(Err(e)) => {
                            return Err(EverEvoError::Vector(format!("stream error: {e}")));
                        }
                        None => break,
                    }
                }
            }

            let mut scored = Vec::new();
            for batch in &batches {
                let distance_opt: Option<&Float32Array> =
                    batch.schema().column_with_name("_distance").and_then(|_| {
                        let idx = batch.schema().index_of("_distance").ok()?;
                        batch.column(idx).as_any().downcast_ref::<Float32Array>()
                    });
                let chunks = Self::record_batch_to_chunks(batch)?;
                for (i, chunk) in chunks.into_iter().enumerate() {
                    let raw_distance = distance_opt.map(|d| d.value(i)).unwrap_or(1.0);
                    let score = 1.0 - raw_distance;
                    scored.push(ScoredChunk { chunk, score });
                }
            }
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            scored.truncate(k);
            Ok(scored)
        })
    }

    fn delete(&self, ids: &[Uuid]) -> Result<(), EverEvoError> {
        if ids.is_empty() {
            return Ok(());
        }
        let quoted: Vec<String> = ids.iter().map(|id| format!("'{}'", id)).collect();
        let predicate = format!("id IN ({})", quoted.join(","));

        self.rt.block_on(async {
            let db = lancedb::connect(&self.uri).execute().await.map_err(|e| {
                EverEvoError::Vector(format!("connect: {e}"))
            })?;
            let table = db.open_table(&self.table_name).execute().await.map_err(|e| {
                EverEvoError::Vector(format!("open_table: {e}"))
            })?;
            table
                .delete(&predicate)
                .await
                .map_err(|e| EverEvoError::Vector(format!("delete: {e}")))?;
            Ok(())
        })
    }

    fn count(&self) -> usize {
        self.rt
            .block_on(async {
                let db = lancedb::connect(&self.uri).execute().await.map_err(|e| {
                    EverEvoError::Vector(format!("connect: {e}"))
                })?;
                let table = db.open_table(&self.table_name).execute().await.map_err(|e| {
                    EverEvoError::Vector(format!("open_table: {e}"))
                })?;
                table
                    .count_rows(None)
                    .await
                    .map_err(|e| EverEvoError::Vector(format!("count_rows: {e}")))
            })
            .unwrap_or(0) as usize
    }

    fn get(&self, id: &Uuid) -> Option<MemoryChunk> {
        let id_str = id.to_string();
        let dim = self.dim;
        self.rt.block_on(async {
            let db = lancedb::connect(&self.uri).execute().await.ok()?;
            let table = db.open_table("chunks").execute().await.ok()?;
            let dummy = vec![0.0_f32; dim];
            let results = table
                .query()
                .nearest_to(dummy)
                .ok()?
                .only_if(format!("id = '{}'", id_str))
                .limit(1)
                .execute()
                .await
                .ok()?;
            let mut batches: Vec<RecordBatch> = Vec::new();
            {
                futures::pin_mut!(results);
                loop {
                    match futures::StreamExt::next(&mut results).await {
                        Some(Ok(batch)) => {
                            batches.push(batch);
                            break;
                        }
                        Some(Err(_)) => break,
                        None => break,
                    }
                }
            }
            let batch = batches.first()?;
            let chunks = Self::record_batch_to_chunks(batch).ok()?;
            chunks.into_iter().next()
        })
    }
}

#[cfg(test)]
#[cfg(feature = "lancedb")]
mod tests {
    use super::*;
    use everevo_core::memory::ProjectionMetadata;
    use tempfile::TempDir;

    fn open_temp_store(dim: usize) -> (LanceDBStore, TempDir) {
        let dir = TempDir::new().expect("tempdir");
        let store =
            LanceDBStore::open(dir.path().join("chunks.lance"), dim).expect("open LanceDBStore");
        (store, dir)
    }

    fn make_chunk_with_dim(content: &str, vector: &[f32]) -> MemoryChunk {
        MemoryChunk {
            id: Uuid::new_v4(),
            content: content.into(),
            vector: vector.to_vec(),
            source_pointers: vec![],
            projection: ProjectionMetadata::new("1.0.0", "none", vec![], 1.0),
            chunk_type: ChunkType::Fact,
            created_at: chrono::Utc::now(),
            retrieval_count: 0,
        }
    }

    #[test]
    fn test_lancedb_insert_and_search() {
        let (store, _dir) = open_temp_store(3);
        let c1 = make_chunk_with_dim("hello world", &[1.0, 0.0, 0.0]);
        let c2 = make_chunk_with_dim("goodbye world", &[0.0, 1.0, 0.0]);
        let c3 = make_chunk_with_dim("foo bar", &[0.0, 0.0, 1.0]);
        store.insert(vec![c1.clone(), c2.clone(), c3.clone()]).unwrap();
        assert_eq!(store.count(), 3);
        let results = store.search(&[1.0, 0.1, 0.0], 2).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].chunk.id, c1.id);
    }

    #[test]
    fn test_lancedb_delete() {
        let (store, _dir) = open_temp_store(2);
        let c1 = make_chunk_with_dim("keep", &[1.0, 0.0]);
        let c2 = make_chunk_with_dim("remove", &[0.0, 1.0]);
        store.insert(vec![c1.clone(), c2.clone()]).unwrap();
        assert_eq!(store.count(), 2);
        store.delete(&[c2.id]).unwrap();
        assert_eq!(store.count(), 1);
        let results = store.search(&[0.0, 1.0], 5).unwrap();
        assert!(results.iter().all(|s| s.chunk.id != c2.id));
    }

    #[test]
    fn test_lancedb_count() {
        let (store, _dir) = open_temp_store(2);
        assert_eq!(store.count(), 0);
        store.insert(vec![make_chunk_with_dim("first", &[1.0, 0.0])]).unwrap();
        assert_eq!(store.count(), 1);
        store.insert(vec![
            make_chunk_with_dim("second", &[0.0, 1.0]),
            make_chunk_with_dim("third", &[0.5, 0.5]),
        ]).unwrap();
        assert_eq!(store.count(), 3);
    }

    #[test]
    fn test_lancedb_get() {
        let (store, _dir) = open_temp_store(2);
        let chunk = make_chunk_with_dim("find me", &[0.7, 0.3]);
        store.insert(vec![chunk.clone()]).unwrap();
        let found = store.get(&chunk.id);
        assert!(found.is_some());
        assert_eq!(found.unwrap().content, "find me");
        let missing = store.get(&Uuid::new_v4());
        assert!(missing.is_none());
    }
}
