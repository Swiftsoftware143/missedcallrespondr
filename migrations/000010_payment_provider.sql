-- Add payment_provider column to plans table
-- Allows each plan to specify which payment processor to use
ALTER TABLE plans ADD COLUMN IF NOT EXISTS payment_provider VARCHAR(64);
