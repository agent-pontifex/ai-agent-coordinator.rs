-- Parameters:
--   $1 embedding_space
--   $2 uuid expected current primary model
--   $3 uuid fully backfilled and validated replacement model
--   $4 bigint expected optimistic route revision
--
-- The caller must verify corpus/search-quality evidence before executing this
-- transaction. A zero-row result means the route changed concurrently or the
-- model lifecycle preconditions are not satisfied.

WITH candidate AS (
    SELECT model.id
    FROM memebank.embedding_models AS model
    WHERE model.id = $3
      AND model.space = $1::memebank.embedding_space
      AND model.status = 'active'
),
updated AS (
    UPDATE memebank.embedding_search_routes AS route
    SET primary_model_id = candidate.id,
        shadow_model_id = route.primary_model_id,
        cutover_state = 'cutting_over',
        revision = route.revision + 1,
        updated_at = clock_timestamp()
    FROM candidate
    WHERE route.space = $1::memebank.embedding_space
      AND route.primary_model_id = $2
      AND route.revision = $4
    RETURNING route.*
)
SELECT * FROM updated;
