-- Parameters:
--   $1 uuid        library_id
--   $2 text        web-style lexical query
--   $3 uuid        active 768-dimensional model_id
--   $4 vector(768) query embedding
--   $5 integer     requested limit, constrained by the caller to 1..100
--
-- This query is intentionally dimension-specific so PostgreSQL can use the
-- correct vector typmod and HNSW index. Other approved dimensions receive a
-- generated, reviewed sibling query rather than a dynamic table name.

WITH lexical AS MATERIALIZED (
    SELECT
        document.asset_id,
        row_number() OVER (
            ORDER BY ts_rank_cd(
                document.search_vector,
                websearch_to_tsquery('simple'::regconfig, $2)
            ) DESC,
            document.asset_id
        ) AS lexical_rank,
        ts_rank_cd(
            document.search_vector,
            websearch_to_tsquery('simple'::regconfig, $2)
        ) AS lexical_score
    FROM memebank.asset_search_documents AS document
    JOIN memebank.assets AS asset
      ON asset.library_id = document.library_id
     AND asset.id = document.asset_id
    WHERE document.library_id = $1
      AND asset.state = 'ready'
      AND document.search_vector @@ websearch_to_tsquery('simple'::regconfig, $2)
    ORDER BY lexical_score DESC, document.asset_id
    LIMIT ($5 * 4)
),
semantic AS MATERIALIZED (
    SELECT
        embedding.asset_id,
        row_number() OVER (
            ORDER BY embedding.embedding <=> $4::vector(768), embedding.asset_id
        ) AS semantic_rank,
        1.0 - (embedding.embedding <=> $4::vector(768)) AS semantic_score
    FROM memebank.asset_embeddings_768 AS embedding
    JOIN memebank.assets AS asset
      ON asset.library_id = embedding.library_id
     AND asset.id = embedding.asset_id
    WHERE embedding.library_id = $1
      AND embedding.model_id = $3
      AND embedding.metric = 'cosine'
      AND asset.state = 'ready'
    ORDER BY embedding.embedding <=> $4::vector(768), embedding.asset_id
    LIMIT ($5 * 4)
),
candidate_ids AS (
    SELECT asset_id FROM lexical
    UNION
    SELECT asset_id FROM semantic
),
fused AS (
    SELECT
        candidate.asset_id,
        lexical.lexical_score,
        semantic.semantic_score,
        lexical.lexical_rank,
        semantic.semantic_rank,
        coalesce(1.0 / (60.0 + lexical.lexical_rank), 0.0)
        + coalesce(1.0 / (60.0 + semantic.semantic_rank), 0.0) AS reciprocal_rank_score
    FROM candidate_ids AS candidate
    LEFT JOIN lexical USING (asset_id)
    LEFT JOIN semantic USING (asset_id)
)
SELECT
    fused.asset_id,
    fused.reciprocal_rank_score,
    fused.lexical_score,
    fused.semantic_score,
    fused.lexical_rank,
    fused.semantic_rank
FROM fused
ORDER BY fused.reciprocal_rank_score DESC, fused.asset_id
LIMIT $5;
