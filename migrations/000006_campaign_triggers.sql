-- Campaign triggers system for MissedCall Respondr
-- Supports PerkZilla-style: on_win, on_enter, on_quiz_result, on_raffle

CREATE TABLE IF NOT EXISTS campaign_email_triggers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    portfolio_company_id UUID REFERENCES portfolio_companies(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    trigger_event TEXT NOT NULL DEFAULT 'on_win',
    -- Available events: on_win, on_enter, on_quiz_result, on_loss, on_raffle_entry
    subject_template TEXT NOT NULL,
    body_template TEXT NOT NULL,
    from_name TEXT DEFAULT 'MissedCall Respondr',
    from_email TEXT,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_email_triggers_tenant ON campaign_email_triggers(tenant_id);
CREATE INDEX IF NOT EXISTS idx_email_triggers_portfolio ON campaign_email_triggers(portfolio_company_id);
CREATE INDEX IF NOT EXISTS idx_email_triggers_event ON campaign_email_triggers(trigger_event);

CREATE TABLE IF NOT EXISTS campaign_redirect_triggers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    portfolio_company_id UUID REFERENCES portfolio_companies(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    trigger_event TEXT NOT NULL DEFAULT 'on_win',
    -- Available events: on_win, on_enter, on_quiz_result, on_loss, on_raffle_entry
    redirect_url TEXT NOT NULL,
    is_active BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_redirect_triggers_tenant ON campaign_redirect_triggers(tenant_id);
CREATE INDEX IF NOT EXISTS idx_redirect_triggers_portfolio ON campaign_redirect_triggers(portfolio_company_id);
CREATE INDEX IF NOT EXISTS idx_redirect_triggers_event ON campaign_redirect_triggers(trigger_event);

-- SMTP/email provider config per portfolio company
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS smtp_provider TEXT;
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS smtp_api_key TEXT;
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS smtp_from_email TEXT;
ALTER TABLE portfolio_companies ADD COLUMN IF NOT EXISTS smtp_from_name TEXT;
