\set ON_ERROR_STOP on

DO $verify$
DECLARE
    missing_extensions text[];
    missing_tables text[];
    unprotected_tables text[];
    incorrect_vectors text[];
BEGIN
    SELECT array_agg(required.name ORDER BY required.name)
    INTO missing_extensions
    FROM (VALUES ('pgcrypto'), ('pg_trgm'), ('vector')) AS required(name)
    WHERE NOT EXISTS (
        SELECT 1 FROM pg_extension WHERE extname = required.name
    );
    IF missing_extensions IS NOT NULL THEN
        RAISE EXCEPTION 'missing required extensions: %', missing_extensions;
    END IF;

    SELECT array_agg(required.name ORDER BY required.name)
    INTO missing_tables
    FROM (
        VALUES
            ('memebank.libraries'),
            ('memebank.library_memberships'),
            ('memebank.assets'),
            ('memebank.asset_variants'),
            ('memebank.storage_connections'),
            ('memebank.storage_locations'),
            ('memebank.enrichment_observations'),
            ('memebank.asset_search_documents'),
            ('memebank.asset_embeddings_384'),
            ('memebank.asset_embeddings_768'),
            ('memebank.asset_embeddings_1024'),
            ('memebank.jobs'),
            ('memebank.export_requests'),
            ('memebank.deletion_requests'),
            ('memebank.reconciliation_runs'),
            ('memebank_private.blobs')
    ) AS required(name)
    WHERE to_regclass(required.name) IS NULL;
    IF missing_tables IS NOT NULL THEN
        RAISE EXCEPTION 'missing required tables: %', missing_tables;
    END IF;

    SELECT array_agg(format('%I.%I', namespace.nspname, class.relname) ORDER BY namespace.nspname, class.relname)
    INTO unprotected_tables
    FROM pg_class AS class
    JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace
    WHERE namespace.nspname IN ('memebank', 'memebank_private')
      AND class.relkind IN ('r', 'p')
      AND (NOT class.relrowsecurity OR NOT class.relforcerowsecurity);
    IF unprotected_tables IS NOT NULL THEN
        RAISE EXCEPTION 'tables missing enabled and forced RLS: %', unprotected_tables;
    END IF;

    SELECT array_agg(format('%I.%I=%s', namespace.nspname, class.relname, format_type(attribute.atttypid, attribute.atttypmod)))
    INTO incorrect_vectors
    FROM pg_class AS class
    JOIN pg_namespace AS namespace ON namespace.oid = class.relnamespace
    JOIN pg_attribute AS attribute ON attribute.attrelid = class.oid AND attribute.attname = 'embedding'
    WHERE (namespace.nspname, class.relname) IN (
        ('memebank', 'asset_embeddings_384'),
        ('memebank', 'asset_embeddings_768'),
        ('memebank', 'asset_embeddings_1024')
    )
      AND format_type(attribute.atttypid, attribute.atttypmod) <> CASE class.relname
          WHEN 'asset_embeddings_384' THEN 'vector(384)'
          WHEN 'asset_embeddings_768' THEN 'vector(768)'
          WHEN 'asset_embeddings_1024' THEN 'vector(1024)'
      END;
    IF incorrect_vectors IS NOT NULL THEN
        RAISE EXCEPTION 'incorrect vector typmods: %', incorrect_vectors;
    END IF;

    IF NOT EXISTS (
        SELECT 1
        FROM pg_attribute
        WHERE attrelid = 'memebank.asset_search_documents'::regclass
          AND attname = 'search_vector'
          AND attgenerated = 's'
    ) THEN
        RAISE EXCEPTION 'asset_search_documents.search_vector must be a stored generated column';
    END IF;

    IF EXISTS (
        SELECT 1
        FROM pg_roles
        WHERE rolname IN ('mb_app', 'mb_worker')
          AND (rolsuper OR rolbypassrls OR rolcreaterole OR rolcreatedb OR rolcanlogin)
    ) THEN
        RAISE EXCEPTION 'application or worker role has forbidden privileges';
    END IF;

    IF NOT EXISTS (
        SELECT 1 FROM pg_roles
        WHERE rolname = 'mb_policy_owner'
          AND rolbypassrls
          AND NOT rolcanlogin
    ) THEN
        RAISE EXCEPTION 'mb_policy_owner must be no-login and BYPASSRLS';
    END IF;
END
$verify$;
