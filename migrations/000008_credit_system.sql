-- Add credit columns to tenant_plans
ALTER TABLE tenant_plans ADD COLUMN IF NOT EXISTS credit_balance INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tenant_plans ADD COLUMN IF NOT EXISTS lifetime_credits INTEGER NOT NULL DEFAULT 0;
ALTER TABLE tenant_plans ADD COLUMN IF NOT EXISTS expires_at TIMESTAMP WITH TIME ZONE;

-- Create the Free tier plan (lowest tier, price = $0)
INSERT INTO plans (id, name, slug, description, price_monthly, price_yearly, features, is_active, sort_order)
VALUES (
  gen_random_uuid(),
  'Free',
  'free',
  'Free look-around access with 50 starter credits. Pay-as-you-go after that.',
  0.00,
  0.00,
  '{"included_credits": 50, "starter_credits": 50, "max_users": 1, "max_phone_numbers": 1}'::jsonb,
  true,
  1
)
ON CONFLICT (slug) DO UPDATE SET
  name = 'Free',
  price_monthly = 0,
  price_yearly = 0,
  features = '{"included_credits": 50, "starter_credits": 50, "max_users": 1, "max_phone_numbers": 1}'::jsonb,
  is_active = true,
  sort_order = 1;

-- Create a Pro monthly plan ($49/mo)
INSERT INTO plans (id, name, slug, description, price_monthly, price_yearly, features, is_active, sort_order)
VALUES (
  gen_random_uuid(),
  'Pro Monthly',
  'pro-monthly',
  'Monthly subscription with 5,000 credits + 1,500 bonus credits every month.',
  49.00,
  490.00,
  '{"included_credits": 5000, "bonus_credits": 1500, "max_users": 5, "max_phone_numbers": 5}'::jsonb,
  true,
  2
)
ON CONFLICT (slug) DO NOTHING;
