//! 账号级 canonical 到上游 wire 模型映射

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::validation::validate_text;

/// 单个账号最多保存的精确模型映射数量
pub const MAX_ACCOUNT_MODEL_MAPPINGS: usize = 512;
/// PostgreSQL `jsonb::text` 允许的最大 UTF-8 字节数
pub const MAX_ACCOUNT_MODEL_MAPPINGS_BYTES: usize = 262_144;

/// 账号模型映射文档不满足精确单跳约束
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("model mappings must contain 0–512 exact model IDs and fit 262144 bytes")]
pub struct InvalidAccountModelMappings;

/// 已校验的账号级模型映射；空映射保持现有 alias 和全局映射结果
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct AccountModelMappings(BTreeMap<String, String>);

impl AccountModelMappings {
    #[must_use]
    pub const fn empty() -> Self {
        Self(BTreeMap::new())
    }

    pub fn new(mappings: BTreeMap<String, String>) -> Result<Self, InvalidAccountModelMappings> {
        if mappings.len() > MAX_ACCOUNT_MODEL_MAPPINGS
            || mappings
                .iter()
                .any(|(canonical, wire)| !valid_model_id(canonical) || !valid_model_id(wire))
            || jsonb_text_len(&mappings)
                .is_none_or(|bytes| bytes > MAX_ACCOUNT_MODEL_MAPPINGS_BYTES)
        {
            return Err(InvalidAccountModelMappings);
        }
        Ok(Self(mappings))
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(canonical, wire)| (canonical.as_str(), wire.as_str()))
    }

    /// 对 canonical 模型执行一次精确替换；不递归解析目标
    #[must_use]
    pub fn resolve<'a>(&'a self, canonical: &'a str) -> Cow<'a, str> {
        self.0
            .get(canonical)
            .map_or(Cow::Borrowed(canonical), |wire| Cow::Borrowed(wire))
    }

    #[must_use]
    pub fn wire_model(&self, canonical: &str) -> Option<&str> {
        self.0.get(canonical).map(String::as_str)
    }
}

fn jsonb_text_len(mappings: &BTreeMap<String, String>) -> Option<usize> {
    if mappings.is_empty() {
        return Some(2);
    }
    let mut bytes = 2_usize;
    for (index, (canonical, wire)) in mappings.iter().enumerate() {
        bytes = bytes
            .checked_add(serde_json::to_string(canonical).ok()?.len())?
            .checked_add(serde_json::to_string(wire).ok()?.len())?
            .checked_add(2)?;
        if index + 1 < mappings.len() {
            bytes = bytes.checked_add(2)?;
        }
    }
    Some(bytes)
}

impl Default for AccountModelMappings {
    fn default() -> Self {
        Self::empty()
    }
}

fn valid_model_id(value: &str) -> bool {
    value.trim() == value && validate_text(value, 256, true, None).is_ok()
}

impl<'de> Deserialize<'de> for AccountModelMappings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let mappings = BTreeMap::<String, String>::deserialize(deserializer)?;
        Self::new(mappings).map_err(serde::de::Error::custom)
    }
}
