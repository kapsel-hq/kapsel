-- Remove DEFERRABLE constraint on merkle_leaves.tree_index that causes deadlocks
-- during batch insertion of attestation leaves

-- Drop the existing deferrable unique constraint
-- The constraint name is auto-generated, so we need to find it first
DO $$
DECLARE
    constraint_name text;
BEGIN
    SELECT conname INTO constraint_name
    FROM pg_constraint
    WHERE conrelid = 'merkle_leaves'::regclass
    AND contype = 'u'
    AND array_length(conkey, 1) = 1
    AND conkey[1] = (SELECT attnum FROM pg_attribute WHERE attrelid = 'merkle_leaves'::regclass AND attname = 'tree_index');

    IF constraint_name IS NOT NULL THEN
        EXECUTE 'ALTER TABLE merkle_leaves DROP CONSTRAINT ' || quote_ident(constraint_name);
    END IF;
END
$$;

-- Create new non-deferrable unique constraint on tree_index
-- This prevents duplicate tree indexes without the deadlock-prone deferred checking
ALTER TABLE merkle_leaves
ADD CONSTRAINT merkle_leaves_tree_index_unique UNIQUE (tree_index);

-- Add comment explaining the change
COMMENT ON CONSTRAINT merkle_leaves_tree_index_unique ON merkle_leaves IS
'Non-deferrable unique constraint on tree_index to prevent deadlocks during batch attestation commits';
