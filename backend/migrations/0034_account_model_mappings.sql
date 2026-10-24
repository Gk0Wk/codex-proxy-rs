-- 账号级 canonical 到上游 wire 的精确单跳映射；旧账号默认保持恒等
ALTER TABLE provider_accounts
    ADD COLUMN model_mappings_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    ADD CONSTRAINT provider_accounts_model_mappings_shape CHECK (
        jsonb_typeof(model_mappings_json) = 'object'
        AND octet_length(model_mappings_json::text) <= 262144
    );
