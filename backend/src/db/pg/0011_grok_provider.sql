-- Allow Grok Build accounts alongside Claude and Codex. Gemini and OpenCode stay in the check so
-- an import of an older database can still hold those rows until startup removes them.
ALTER TABLE ai_accounts DROP CONSTRAINT ai_accounts_provider_check;
ALTER TABLE ai_accounts
  ADD CONSTRAINT ai_accounts_provider_check
  CHECK (provider IN ('claude', 'codex', 'gemini', 'opencode', 'grok'));
