-- MissedCall Respondr Initial Schema

CREATE EXTENSION IF NOT EXISTS "uuid-ossp";
CREATE EXTENSION IF NOT EXISTS "pgcrypto";

-- Tenants
CREATE TABLE IF NOT EXISTS tenants (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

-- Users
CREATE TABLE IF NOT EXISTS users (
    id UUID PRIMARY KEY,
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    name TEXT NOT NULL,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    role TEXT NOT NULL DEFAULT 'admin',
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_users_email ON users(email);
CREATE INDEX IF NOT EXISTS idx_users_tenant_id ON users(tenant_id);

-- Inbound Calls
CREATE TABLE IF NOT EXISTS inbound_calls (
    id UUID PRIMARY KEY,
    caller_number TEXT NOT NULL,
    caller_name TEXT,
    called_number TEXT NOT NULL,
    call_time TIMESTAMP NOT NULL DEFAULT NOW(),
    duration INTEGER,
    recording_url TEXT,
    voicemail_url TEXT,
    disposition TEXT NOT NULL DEFAULT 'missed',
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_inbound_calls_tenant ON inbound_calls(tenant_id);
CREATE INDEX IF NOT EXISTS idx_inbound_calls_caller ON inbound_calls(caller_number);
CREATE INDEX IF NOT EXISTS idx_inbound_calls_disposition ON inbound_calls(disposition);

-- Response Rules
CREATE TABLE IF NOT EXISTS response_rules (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    trigger_condition TEXT NOT NULL,
    response_type TEXT NOT NULL,
    response_content JSONB NOT NULL DEFAULT '{}',
    schedule JSONB,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_response_rules_tenant ON response_rules(tenant_id);

-- Follow Ups
CREATE TABLE IF NOT EXISTS follow_ups (
    id UUID PRIMARY KEY,
    call_id UUID NOT NULL REFERENCES inbound_calls(id) ON DELETE CASCADE,
    follow_type TEXT NOT NULL,
    scheduled_at TIMESTAMP NOT NULL,
    completed_at TIMESTAMP,
    status TEXT NOT NULL DEFAULT 'pending',
    notes TEXT,
    assigned_to UUID REFERENCES users(id),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_follow_ups_tenant ON follow_ups(tenant_id);
CREATE INDEX IF NOT EXISTS idx_follow_ups_status ON follow_ups(status);

-- Messages
CREATE TABLE IF NOT EXISTS messages (
    id UUID PRIMARY KEY,
    call_id UUID REFERENCES inbound_calls(id) ON DELETE SET NULL,
    direction TEXT NOT NULL,
    from_number TEXT NOT NULL,
    to_number TEXT NOT NULL,
    body TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'sent',
    sent_at TIMESTAMP,
    delivered_at TIMESTAMP,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_messages_tenant ON messages(tenant_id);

-- Message Templates
CREATE TABLE IF NOT EXISTS message_templates (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    body TEXT NOT NULL,
    variables TEXT[],
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_message_templates_tenant ON message_templates(tenant_id);

-- Contacts
CREATE TABLE IF NOT EXISTS contacts (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    phone TEXT NOT NULL,
    email TEXT,
    company TEXT,
    notes TEXT,
    tags TEXT[],
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_contacts_tenant ON contacts(tenant_id);
CREATE INDEX IF NOT EXISTS idx_contacts_phone ON contacts(phone);

-- Voicemails
CREATE TABLE IF NOT EXISTS voicemails (
    id UUID PRIMARY KEY,
    call_id UUID NOT NULL REFERENCES inbound_calls(id) ON DELETE CASCADE,
    audio_url TEXT,
    transcription TEXT,
    duration INTEGER,
    listened BOOLEAN NOT NULL DEFAULT false,
    notes TEXT,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_voicemails_tenant ON voicemails(tenant_id);

-- Call Logs
CREATE TABLE IF NOT EXISTS call_logs (
    id UUID PRIMARY KEY,
    caller_number TEXT NOT NULL,
    called_number TEXT NOT NULL,
    duration INTEGER,
    disposition TEXT NOT NULL,
    cost DOUBLE PRECISION,
    recorded BOOLEAN NOT NULL DEFAULT false,
    notes TEXT,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_call_logs_tenant ON call_logs(tenant_id);

-- Integrations
CREATE TABLE IF NOT EXISTS integrations (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    integration_type TEXT NOT NULL,
    config JSONB NOT NULL DEFAULT '{}',
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMP NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_integrations_tenant ON integrations(tenant_id);

-- Activity Log
CREATE TABLE IF NOT EXISTS activity_log (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id),
    action TEXT NOT NULL,
    entity_type TEXT NOT NULL,
    entity_id UUID,
    metadata JSONB,
    created_at TIMESTAMP NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_activity_log_tenant ON activity_log(tenant_id);

-- Tenant Settings
CREATE TABLE IF NOT EXISTS tenant_settings (
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value JSONB NOT NULL DEFAULT '{}',
    PRIMARY KEY (tenant_id, key)
);

-- Dashboard Stats (materialized for performance)
CREATE TABLE IF NOT EXISTS dashboard_stats (
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    period TEXT NOT NULL,
    total_calls BIGINT NOT NULL DEFAULT 0,
    missed_calls BIGINT NOT NULL DEFAULT 0,
    answered_calls BIGINT NOT NULL DEFAULT 0,
    response_rate DOUBLE PRECISION,
    avg_response_time DOUBLE PRECISION,
    PRIMARY KEY (tenant_id, period)
);

-- Seed default response rules for tenant
-- These will be inserted when a tenant registers via application logic
