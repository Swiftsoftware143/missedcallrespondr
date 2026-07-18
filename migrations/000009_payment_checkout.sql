-- Payment Providers table
CREATE TABLE IF NOT EXISTS payment_providers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_type VARCHAR(32) NOT NULL,
    label VARCHAR(255) NOT NULL DEFAULT '',
    is_active BOOLEAN NOT NULL DEFAULT false,
    api_key_encrypted TEXT NOT NULL DEFAULT '',
    publishable_key VARCHAR(255) NOT NULL DEFAULT '',
    webhook_secret_encrypted TEXT NOT NULL DEFAULT '',
    config JSONB NOT NULL DEFAULT '{}',
    is_test_mode BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    UNIQUE(provider_type)
);

-- Checkout Sessions table
CREATE TABLE IF NOT EXISTS checkout_sessions (
    id UUID PRIMARY KEY,
    account_id UUID NOT NULL,
    user_id UUID NOT NULL,
    provider_type VARCHAR(32) NOT NULL,
    provider_session_id VARCHAR(255) NOT NULL DEFAULT '',
    purchasable_type VARCHAR(64) NOT NULL,
    purchasable_id UUID,
    amount NUMERIC(12,2) NOT NULL,
    currency VARCHAR(3) NOT NULL DEFAULT 'USD',
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    metadata JSONB NOT NULL DEFAULT '{}',
    webhook_event_id VARCHAR(255),
    webhook_received_at TIMESTAMP WITH TIME ZONE,
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_checkout_sessions_account_id ON checkout_sessions(account_id);
CREATE INDEX IF NOT EXISTS idx_checkout_sessions_provider_session ON checkout_sessions(provider_type, provider_session_id);
CREATE INDEX IF NOT EXISTS idx_checkout_sessions_status ON checkout_sessions(status);

-- Payment Webhook Events log table
CREATE TABLE IF NOT EXISTS payment_webhook_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_type VARCHAR(32) NOT NULL,
    event_type VARCHAR(128) NOT NULL,
    event_id VARCHAR(255) NOT NULL,
    raw_body JSONB NOT NULL DEFAULT '{}',
    headers JSONB NOT NULL DEFAULT '{}',
    status VARCHAR(32) NOT NULL DEFAULT 'received',
    created_at TIMESTAMP WITH TIME ZONE NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_payment_webhook_events_event_id ON payment_webhook_events(event_id);
CREATE INDEX IF NOT EXISTS idx_payment_webhook_events_status ON payment_webhook_events(status);
