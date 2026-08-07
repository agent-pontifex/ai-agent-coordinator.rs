BEGIN;

CREATE TABLE memebank.libraries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_user_id uuid NOT NULL,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 160),
    visibility memebank.library_visibility NOT NULL DEFAULT 'private',
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    tombstoned_at timestamptz,
    CHECK (tombstoned_at IS NULL OR tombstoned_at >= created_at),
    UNIQUE (id, owner_user_id)
);

CREATE TABLE memebank.library_memberships (
    library_id uuid NOT NULL REFERENCES memebank.libraries(id) ON DELETE CASCADE,
    user_id uuid NOT NULL,
    role memebank.membership_role NOT NULL,
    status memebank.membership_status NOT NULL DEFAULT 'invited',
    invited_by_user_id uuid,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    joined_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    PRIMARY KEY (library_id, user_id),
    CHECK (
        (status = 'active' AND joined_at IS NOT NULL)
        OR (status <> 'active')
    )
);

CREATE TABLE memebank_private.blobs (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    digest_algorithm memebank.digest_algorithm NOT NULL,
    digest_hex text NOT NULL CHECK (digest_hex ~ '^[0-9a-f]{64}$'),
    byte_length bigint NOT NULL CHECK (byte_length >= 0),
    media_type text NOT NULL CHECK (media_type ~ '^[a-z0-9.+-]+/[a-z0-9.+-]+$'),
    pixel_width integer CHECK (pixel_width > 0),
    pixel_height integer CHECK (pixel_height > 0),
    frame_count integer CHECK (frame_count > 0),
    sanitized_metadata jsonb NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(sanitized_metadata) = 'object'),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (digest_algorithm, digest_hex, byte_length),
    CHECK ((pixel_width IS NULL) = (pixel_height IS NULL))
);

CREATE TABLE memebank.assets (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    library_id uuid NOT NULL REFERENCES memebank.libraries(id) ON DELETE RESTRICT,
    original_blob_id uuid REFERENCES memebank_private.blobs(id) ON DELETE RESTRICT,
    created_by_user_id uuid NOT NULL,
    state memebank.asset_state NOT NULL DEFAULT 'importing',
    title text CHECK (title IS NULL OR char_length(title) <= 240),
    note text CHECK (note IS NULL OR char_length(note) <= 4000),
    source_client_reference text CHECK (source_client_reference IS NULL OR char_length(source_client_reference) <= 512),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    ready_at timestamptz,
    tombstoned_at timestamptz,
    UNIQUE (library_id, id),
    CHECK (state <> 'ready' OR original_blob_id IS NOT NULL),
    CHECK (state <> 'deleted' OR tombstoned_at IS NOT NULL),
    CHECK (ready_at IS NULL OR ready_at >= created_at),
    CHECK (tombstoned_at IS NULL OR tombstoned_at >= created_at)
);

CREATE TABLE memebank.asset_variants (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    library_id uuid NOT NULL,
    asset_id uuid NOT NULL,
    blob_id uuid NOT NULL REFERENCES memebank_private.blobs(id) ON DELETE RESTRICT,
    source_variant_id uuid REFERENCES memebank.asset_variants(id) ON DELETE SET NULL,
    kind memebank.variant_kind NOT NULL,
    recipe text NOT NULL CHECK (char_length(recipe) BETWEEN 1 AND 160),
    frame_index integer CHECK (frame_index >= 0),
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (library_id, id),
    UNIQUE (asset_id, kind, recipe, frame_index),
    FOREIGN KEY (library_id, asset_id)
        REFERENCES memebank.assets(library_id, id)
        ON DELETE CASCADE,
    CHECK ((kind = 'animated_frame') = (frame_index IS NOT NULL))
);

CREATE TABLE memebank.perceptual_hashes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    library_id uuid NOT NULL,
    asset_id uuid NOT NULL,
    variant_id uuid NOT NULL,
    algorithm text NOT NULL CHECK (algorithm IN ('phash64', 'dhash64')),
    hash_bits bit(64) NOT NULL,
    hash_bucket integer NOT NULL CHECK (hash_bucket BETWEEN 0 AND 65535),
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    UNIQUE (variant_id, algorithm),
    FOREIGN KEY (library_id, asset_id)
        REFERENCES memebank.assets(library_id, id)
        ON DELETE CASCADE,
    FOREIGN KEY (library_id, variant_id)
        REFERENCES memebank.asset_variants(library_id, id)
        ON DELETE CASCADE
);

CREATE UNIQUE INDEX library_single_active_owner_idx
    ON memebank.library_memberships (library_id)
    WHERE role = 'owner' AND status = 'active';

CREATE INDEX library_memberships_user_idx
    ON memebank.library_memberships (user_id, status, library_id);

CREATE INDEX assets_library_state_updated_idx
    ON memebank.assets (library_id, state, updated_at DESC, id);

CREATE INDEX variants_asset_kind_idx
    ON memebank.asset_variants (library_id, asset_id, kind);

CREATE INDEX perceptual_hash_bucket_idx
    ON memebank.perceptual_hashes (library_id, algorithm, hash_bucket);

COMMIT;
