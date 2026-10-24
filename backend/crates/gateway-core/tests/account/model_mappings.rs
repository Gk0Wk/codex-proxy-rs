//! 验证账号级逻辑模型映射的默认值、单跳解析与账号授权边界

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use gateway_core::account::{
    AccountModelAccess, AccountModelAccessMode, AccountModelMappings, ProviderAccountId,
};
use gateway_core::routing::{
    ClientRoutingScope, FrozenAccountScope, ProviderKind, RuntimeAccount, RuntimeAccountDirectory,
};
use serde_json::json;

#[test]
fn account_model_mappings_should_default_empty_and_resolve_exactly_one_hop() {
    let empty = AccountModelMappings::default();
    assert!(empty.is_empty());
    assert_eq!(empty.resolve("canonical"), "canonical");

    let mappings: AccountModelMappings = serde_json::from_value(json!({
        "canonical": "wire-model"
    }))
    .expect("valid mapping document");
    assert_eq!(mappings.resolve("canonical"), "wire-model");
    assert_eq!(mappings.resolve("wire-model"), "wire-model");
    assert_eq!(mappings.resolve("other"), "other");
}

#[test]
fn account_model_mappings_should_reject_invalid_documents() {
    for invalid in [
        json!("not-an-object"),
        json!({"": "wire"}),
        json!({"canonical": ""}),
        json!({" canonical": "wire"}),
        json!({"canonical": "wire "}),
        json!({"canonical": "line\nbreak"}),
        json!({"canonical": "x".repeat(257)}),
        json!({"canonical": 1}),
    ] {
        assert!(serde_json::from_value::<AccountModelMappings>(invalid).is_err());
    }
}

#[test]
fn account_model_mappings_should_apply_jsonb_text_budget_with_escaped_bytes() {
    let escaped = |suffix: usize, quote: bool| {
        let fill = if quote { '"' } else { '\\' }.to_string().repeat(253);
        format!("{fill}{suffix:03}")
    };
    let accepted = (0..128)
        .map(|index| (escaped(index, false), escaped(index, true)))
        .collect::<BTreeMap<_, _>>();
    assert!(AccountModelMappings::new(accepted).is_ok());

    let rejected = (0..256)
        .map(|index| (escaped(index, false), escaped(index, true)))
        .collect::<BTreeMap<_, _>>();
    assert!(AccountModelMappings::new(rejected).is_err());

    let at_limit = (0..512)
        .map(|index| (format!("model-{index}"), "wire".to_owned()))
        .collect();
    assert!(AccountModelMappings::new(at_limit).is_ok());
    let oversized = (0..512)
        .map(|index| (format!("{}-{index:03}", "x".repeat(252)), "y".repeat(256)))
        .collect();
    assert!(AccountModelMappings::new(oversized).is_err());
    let too_many = (0..513)
        .map(|index| (format!("model-{index}"), "wire".to_owned()))
        .collect();
    assert!(AccountModelMappings::new(too_many).is_err());
}

#[test]
fn account_scope_should_apply_model_access_to_each_account_wire_target() {
    let provider = ProviderKind::new("openai").expect("provider");
    let wire_a = ProviderAccountId::new("acct_wire_a").expect("account");
    let wire_b = ProviderAccountId::new("acct_wire_b").expect("account");
    let mappings_a = AccountModelMappings::new(BTreeMap::from([(
        "canonical".to_owned(),
        "wire-a".to_owned(),
    )]))
    .expect("mappings");
    let mappings_b = AccountModelMappings::new(BTreeMap::from([(
        "canonical".to_owned(),
        "wire-b".to_owned(),
    )]))
    .expect("mappings");
    let access_a =
        AccountModelAccess::new(AccountModelAccessMode::Allowlist, vec!["wire-a".to_owned()])
            .expect("access");
    let access_b =
        AccountModelAccess::new(AccountModelAccessMode::Allowlist, vec!["wire-b".to_owned()])
            .expect("access");
    let directory = RuntimeAccountDirectory::new(BTreeMap::from([
        (
            wire_a.clone(),
            RuntimeAccount::new(provider.clone(), BTreeSet::new())
                .with_model_access(access_a)
                .with_model_mappings(mappings_a),
        ),
        (
            wire_b.clone(),
            RuntimeAccount::new(provider, BTreeSet::new())
                .with_model_access(access_b)
                .with_model_mappings(mappings_b),
        ),
    ]));
    let scope = FrozenAccountScope::new(Arc::new(directory), ClientRoutingScope::all_accounts());

    assert!(scope.allows_model(&wire_a, "canonical"));
    assert!(scope.allows_model(&wire_b, "canonical"));
    assert!(!scope.allows_model(&wire_a, "wire-b"));
    assert!(!scope.allows_model(&wire_b, "wire-a"));
}
