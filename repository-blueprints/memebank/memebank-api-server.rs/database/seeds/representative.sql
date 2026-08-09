\set ON_ERROR_STOP on

BEGIN;

INSERT INTO memebank.libraries (id, owner_user_id, name, visibility)
VALUES
    ('10000000-0000-4000-8000-000000000001', '20000000-0000-4000-8000-000000000001', 'Alpha Library', 'private'),
    ('10000000-0000-4000-8000-000000000002', '20000000-0000-4000-8000-000000000002', 'Beta Library', 'private');

INSERT INTO memebank.library_memberships (library_id, user_id, role, status, joined_at)
VALUES
    ('10000000-0000-4000-8000-000000000001', '20000000-0000-4000-8000-000000000001', 'owner', 'active', clock_timestamp()),
    ('10000000-0000-4000-8000-000000000002', '20000000-0000-4000-8000-000000000002', 'owner', 'active', clock_timestamp());

INSERT INTO memebank_private.blobs (
    id, digest_algorithm, digest_hex, byte_length, media_type, pixel_width, pixel_height
)
VALUES
    ('30000000-0000-4000-8000-000000000001', 'sha256', repeat('a', 64), 120000, 'image/png', 1200, 900),
    ('30000000-0000-4000-8000-000000000002', 'sha256', repeat('b', 64), 12000, 'image/webp', 320, 240),
    ('30000000-0000-4000-8000-000000000003', 'sha256', repeat('c', 64), 110000, 'image/png', 1200, 900);

INSERT INTO memebank.assets (
    id, library_id, original_blob_id, created_by_user_id, state, title, note, ready_at
)
VALUES
    (
        '40000000-0000-4000-8000-000000000001',
        '10000000-0000-4000-8000-000000000001',
        '30000000-0000-4000-8000-000000000001',
        '20000000-0000-4000-8000-000000000001',
        'ready',
        'Locks need leases',
        'Distributed coordination reaction image',
        clock_timestamp()
    ),
    (
        '40000000-0000-4000-8000-000000000002',
        '10000000-0000-4000-8000-000000000002',
        '30000000-0000-4000-8000-000000000003',
        '20000000-0000-4000-8000-000000000002',
        'ready',
        'Private beta asset',
        'Must never be visible from Alpha Library',
        clock_timestamp()
    );

INSERT INTO memebank.asset_variants (
    id, library_id, asset_id, blob_id, kind, recipe
)
VALUES (
    '50000000-0000-4000-8000-000000000001',
    '10000000-0000-4000-8000-000000000001',
    '40000000-0000-4000-8000-000000000001',
    '30000000-0000-4000-8000-000000000002',
    'thumbnail',
    'thumbnail-webp-v1'
);

INSERT INTO memebank.perceptual_hashes (
    library_id, asset_id, variant_id, algorithm, hash_bits, hash_bucket
)
VALUES (
    '10000000-0000-4000-8000-000000000001',
    '40000000-0000-4000-8000-000000000001',
    '50000000-0000-4000-8000-000000000001',
    'phash64',
    B'0011001100110011001100110011001100110011001100110011001100110011',
    13107
);

INSERT INTO memebank.tags (id, library_id, normalized_tag, display_name)
VALUES
    ('60000000-0000-4000-8000-000000000001', '10000000-0000-4000-8000-000000000001', 'distributed-systems', 'distributed systems'),
    ('60000000-0000-4000-8000-000000000002', '10000000-0000-4000-8000-000000000001', 'leases', 'leases');

INSERT INTO memebank.asset_tag_decisions (
    library_id, asset_id, tag_id, source, decision, decided_by_user_id
)
VALUES
    (
        '10000000-0000-4000-8000-000000000001',
        '40000000-0000-4000-8000-000000000001',
        '60000000-0000-4000-8000-000000000001',
        'user',
        'confirmed',
        '20000000-0000-4000-8000-000000000001'
    ),
    (
        '10000000-0000-4000-8000-000000000001',
        '40000000-0000-4000-8000-000000000001',
        '60000000-0000-4000-8000-000000000002',
        'user',
        'confirmed',
        '20000000-0000-4000-8000-000000000001'
    );

INSERT INTO memebank.asset_search_documents (
    asset_id, library_id, source_revision, title_text, note_text, confirmed_tags_text, ocr_text, selected_caption_text
)
VALUES
    (
        '40000000-0000-4000-8000-000000000001',
        '10000000-0000-4000-8000-000000000001',
        0,
        'Locks need leases',
        'Distributed coordination reaction image',
        'distributed systems leases',
        'LOCKS NEED LEASES NOT WISHFUL THINKING',
        'A two-line coordination meme on a dark background'
    ),
    (
        '40000000-0000-4000-8000-000000000002',
        '10000000-0000-4000-8000-000000000002',
        0,
        'Private beta asset',
        'Must never be visible from Alpha Library',
        '',
        'BETA PRIVATE',
        ''
    );

INSERT INTO memebank.embedding_models (
    id,
    model_key,
    space,
    model_name,
    model_revision,
    processor_version,
    dimension,
    metric,
    provenance_kind,
    source_uri,
    artifact_sha256,
    license,
    redistribution_terms,
    status,
    activated_at
)
VALUES (
    '70000000-0000-4000-8000-000000000001',
    'fixture/siglip-768/v1',
    'native_visual',
    'fixture-siglip',
    'sha256:fixture-revision',
    'fixture-image-processor-v1',
    768,
    'cosine',
    'local_artifact',
    'fixture://memebank/siglip-768',
    repeat('d', 64),
    'CC0-1.0',
    'Synthetic fixture only',
    'active',
    clock_timestamp()
);

INSERT INTO memebank.embedding_search_routes (space, primary_model_id, cutover_state)
VALUES ('native_visual', '70000000-0000-4000-8000-000000000001', 'stable');

INSERT INTO memebank.asset_embeddings_768 (
    library_id,
    asset_id,
    variant_id,
    model_id,
    space,
    metric,
    model_revision,
    embedding
)
VALUES (
    '10000000-0000-4000-8000-000000000001',
    '40000000-0000-4000-8000-000000000001',
    '50000000-0000-4000-8000-000000000001',
    '70000000-0000-4000-8000-000000000001',
    'native_visual',
    'cosine',
    'sha256:fixture-revision',
    array_fill(0.001::real, ARRAY[768])::vector(768)
);

INSERT INTO memebank.storage_connections (
    id, library_id, provider_kind, ownership, display_name, capability_snapshot
)
VALUES (
    '80000000-0000-4000-8000-000000000001',
    '10000000-0000-4000-8000-000000000001',
    'filesystem',
    'device_local',
    'Development fixture storage',
    '{"put":true,"read":true,"stat":true,"delete":true,"list":true}'::jsonb
);

INSERT INTO memebank.storage_locations (
    library_id, connection_id, blob_id, object_key, state, integrity_algorithm, integrity_hex, last_verified_at
)
VALUES (
    '10000000-0000-4000-8000-000000000001',
    '80000000-0000-4000-8000-000000000001',
    '30000000-0000-4000-8000-000000000001',
    'sha256/aa/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    'present',
    'sha256',
    repeat('a', 64),
    clock_timestamp()
);

INSERT INTO memebank.jobs (
    id, library_id, asset_id, kind, idempotency_key, payload
)
VALUES (
    '90000000-0000-4000-8000-000000000001',
    '10000000-0000-4000-8000-000000000001',
    '40000000-0000-4000-8000-000000000001',
    'refresh_search_document',
    'fixture:refresh:asset:400000000001',
    '{"asset_id":"40000000-0000-4000-8000-000000000001"}'::jsonb
);

COMMIT;
