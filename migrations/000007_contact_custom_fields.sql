-- Contact Custom Fields
CREATE TABLE IF NOT EXISTS contact_custom_fields (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    field_name TEXT NOT NULL,
    field_type TEXT NOT NULL DEFAULT 'text',
    is_required BOOLEAN NOT NULL DEFAULT false,
    field_order INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE(tenant_id, field_name)
);

-- Contact Custom Field Values (the actual data per contact)
CREATE TABLE IF NOT EXISTS contact_field_values (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    contact_id UUID NOT NULL REFERENCES contacts(id) ON DELETE CASCADE,
    field_id UUID NOT NULL REFERENCES contact_custom_fields(id) ON DELETE CASCADE,
    value TEXT,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW(),
    UNIQUE(contact_id, field_id)
);

CREATE INDEX IF NOT EXISTS idx_contact_custom_fields_tenant ON contact_custom_fields(tenant_id);
CREATE INDEX IF NOT EXISTS idx_contact_field_values_contact ON contact_field_values(contact_id);
