-- 000034_system_sender_display_name.sql
-- One-shot correction of the panel-managed SYSTEM sender DISPLAY NAME.
--
-- What was wrong: `admin_settings` key `email` stored
--     from_name = "MissedCallRespondr Help Desk"
-- i.e. the DOMAIN spelling (mail.missedcallrespondr.com, squashed, no space), while this app's own
-- brand is "MissedCall Respondr" everywhere a customer sees it — `branding::APP_NAME`, every email
-- template's `app_name`, the marketing site, the app dashboard. The result was user-visible and
-- self-contradictory: the onboarding mail's own SUBJECT read "Welcome to MissedCall Respondr!" while
-- its From line read "MissedCallRespondr Help Desk <no-reply@mail.missedcallrespondr.com>".
--
-- The fleet contract (programme item b, card t_68d95177) is that the sender reads
--     "<App> Help Desk <no-reply@mail.<domain>>"
-- with <App> the app's real name, so this install owed one correction.
--
-- Scoped tightly: it rewrites the ONE jsonb key, and only when the value is still the exact wrong
-- literal — an operator who has already set their own sender in Admin > Settings > Email is never
-- clobbered, and a re-run after the first fix matches nothing (idempotent; the boot runner executes
-- every migration on each start).
UPDATE admin_settings
   SET value = jsonb_set(value, '{from_name}', '"MissedCall Respondr Help Desk"'::jsonb, true),
       updated_at = NOW()
 WHERE key = 'email'
   AND value->>'from_name' = 'MissedCallRespondr Help Desk';
