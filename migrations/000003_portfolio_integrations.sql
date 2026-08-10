-- Portfolio Companies & Integration Targets
-- These tables support the portfolio management and cross-platform integration features.

CREATE TABLE IF NOT EXISTS portfolio_companies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    slug VARCHAR(255) NOT NULL,
    settings JSONB DEFAULT '{}',
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_pc_tenant ON portfolio_companies(tenant_id);

CREATE TABLE IF NOT EXISTS integration_targets (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    portfolio_company_id UUID REFERENCES portfolio_companies(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    provider VARCHAR(100) NOT NULL DEFAULT 'webhook',
    webhook_url TEXT,
    api_key TEXT,
    events TEXT[] DEFAULT '{}',
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE INDEX IF NOT EXISTS idx_it_tenant ON integration_targets(tenant_id);
CREATE INDEX IF NOT EXISTS idx_it_company ON integration_targets(portfolio_company_id);
