-- provider_keys table
DROP TABLE IF EXISTS provider_keys CASCADE;
CREATE TABLE provider_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id),
    provider VARCHAR(64) NOT NULL,
    api_key TEXT NOT NULL,
    base_url VARCHAR(512),
    metadata JSONB DEFAULT '{}',
    is_active BOOLEAN DEFAULT true,
    scope VARCHAR(16) NOT NULL DEFAULT 'tenant',
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE(tenant_id, provider)
);

-- available_providers
CREATE TABLE IF NOT EXISTS available_providers (
    key VARCHAR(64) PRIMARY KEY,
    name VARCHAR(128) NOT NULL,
    description TEXT,
    requires_base_url BOOLEAN DEFAULT false,
    requires_metadata JSONB DEFAULT '[]',
    icon VARCHAR(32)
);

INSERT INTO available_providers (key, name, description, icon) VALUES
    ('mailgun', 'Mailgun', 'Transactional email sending', 'mail'),
    ('sendgrid', 'SendGrid', 'Email delivery service', 'mail'),
    ('sendiio', 'Sendiio', 'Email/SMS campaign delivery', 'mail'),
    ('nexweave', 'Nexweave', 'Personalized video/image generation', 'video'),
    ('sam_gov', 'SAM.gov', 'Federal contracting opportunities', 'shield'),
    ('openai', 'OpenAI', 'GPT models for AI features', 'brain'),
    ('deepseek', 'DeepSeek', 'AI model for content generation', 'brain')
ON CONFLICT (key) DO NOTHING;
