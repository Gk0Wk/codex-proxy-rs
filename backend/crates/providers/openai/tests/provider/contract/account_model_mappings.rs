//! 验证 OpenAI API Key 账号级模型映射使用最终 wire 事实

use super::*;
use gateway_core::account::{AccountModelMappings, ProviderAccountId};
use gateway_core::metering::PricingOverrides;

const ACCOUNT_ID: &str = "acct_provider_contract";
const CANONICAL_MODEL: &str = "gpt-5.4";
const WIRE_MODEL: &str = "wire-model";
const MAPPING_COMPLETED_SSE: &str = concat!(
    "event: response.created\n",
    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_mapping\",\"model\":\"gpt-5.4\"}}\n\n",
    "event: response.completed\n",
    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_mapping\",\"model\":\"gpt-5.4\",\"status\":\"completed\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2,\"input_tokens_details\":{\"cached_tokens\":0}}}}\n\n"
);

fn mapping_scope(mappings: AccountModelMappings) -> Arc<FrozenAccountScope> {
    let provider = ProviderKind::new("openai").unwrap();
    Arc::new(FrozenAccountScope::new(
        Arc::new(RuntimeAccountDirectory::new(BTreeMap::from([(
            ProviderAccountId::new(ACCOUNT_ID).unwrap(),
            RuntimeAccount::new(provider, BTreeSet::new()).with_model_mappings(mappings),
        )]))),
        ClientRoutingScope::all_accounts(),
    ))
}

fn mapped_request(operation: Operation, scope: Arc<FrozenAccountScope>) -> ProviderRequest {
    let provider = ProviderKind::new("openai").unwrap();
    let snapshot = RuntimeSnapshot::new(
        ConfigRevision::new(1).unwrap(),
        gateway_core::settings::SettingsValues::new(2, "smart", BTreeMap::new(), None, None),
        vec![provider.clone()],
        [CANONICAL_MODEL, WIRE_MODEL]
            .into_iter()
            .map(|model| {
                ProviderModel::new(
                    provider.clone(),
                    UpstreamModelId::new(model).unwrap(),
                    ModelCapabilities::new(BTreeSet::from([OperationKind::Generate]), Some(32_000))
                        .with_upstream_feature_validation(),
                )
            })
            .collect(),
        vec![],
    )
    .unwrap();
    let plan = snapshot
        .plan(
            &PublicModelId::new(CANONICAL_MODEL).unwrap(),
            &operation,
            scope,
            &RoutingContext::default(),
        )
        .unwrap();
    ProviderRequest::new(operation, plan.candidates()[0].clone())
}

fn mapped_context(
    request_id: &str,
    scope: Arc<FrozenAccountScope>,
    pricing: PricingOverrides,
) -> AttemptContext {
    AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_pricing(Arc::new(pricing))
        .with_request_location(Some(global_request_location())),
        NonZeroU32::new(1).unwrap(),
        SystemTime::now() + Duration::from_secs(30),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None).with_account_scope(scope),
        None,
        CancellationToken::new(),
    )
}

fn text_pricing(model: &str) -> PricingOverrides {
    let mut document = json!({"openai": {}});
    document["openai"][model] = json!({
        "multiplierBps": 10000,
        "bands": {"standard": {
            "input": "1", "output": "2", "cacheRead": "0", "cacheWrite": "0"
        }}
    });
    serde_json::from_value(document).unwrap()
}

#[tokio::test]
async fn api_key_account_mapping_controls_body_metadata_and_pricing() {
    let store = Arc::new(MemoryAccountStore::default());
    let server = MockServer::start().await;
    store
        .seed_api_key(
            ACCOUNT_ID,
            server.uri(),
            provider_openai::credential::ResponsesTransport::Http,
        )
        .await;
    let mapping = AccountModelMappings::new(BTreeMap::from([(
        CANONICAL_MODEL.to_owned(),
        WIRE_MODEL.to_owned(),
    )]))
    .unwrap();
    store.set_model_mappings(ACCOUNT_ID, mapping.clone());
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(MAPPING_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let scope = mapping_scope(mapping);
    let operation = generate_operation();
    let request = mapped_request(operation, Arc::clone(&scope));
    let pricing = text_pricing(WIRE_MODEL);
    let mut stream = Arc::clone(&provider)
        .execute(
            request,
            mapped_context("req_account_model_mapping", scope, pricing),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().upstream_model().unwrap().as_str(),
        WIRE_MODEL
    );
    let mut cost = None;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        cost = cost.or_else(|| {
            event.canonical_facts().iter().find_map(|fact| match fact {
                GatewayEvent::CalculatedCost(cost) => Some(cost.clone()),
                _ => None,
            })
        });
    }
    let cost = cost.unwrap().into_estimate();
    assert_eq!(cost.total().unwrap().amount().canonical(), "0.000003");
    let requests = server.received_requests().await.unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["model"], WIRE_MODEL);
}

#[tokio::test]
async fn empty_account_mapping_keeps_canonical_body_metadata_and_pricing() {
    let store = Arc::new(MemoryAccountStore::default());
    let server = MockServer::start().await;
    store
        .seed_api_key(
            ACCOUNT_ID,
            server.uri(),
            provider_openai::credential::ResponsesTransport::Http,
        )
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(MAPPING_COMPLETED_SSE, "text/event-stream"),
        )
        .expect(1)
        .mount(&server)
        .await;
    let provider = provider_with_base_url(&store, server.uri());
    let scope = mapping_scope(AccountModelMappings::empty());
    let operation = generate_operation();
    let request = mapped_request(operation, Arc::clone(&scope));
    let pricing = text_pricing(CANONICAL_MODEL);
    let mut stream = Arc::clone(&provider)
        .execute(
            request,
            mapped_context("req_empty_account_mapping", scope, pricing),
        )
        .await
        .unwrap();
    assert_eq!(
        stream.metadata().upstream_model().unwrap().as_str(),
        CANONICAL_MODEL
    );
    let mut cost = None;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        cost = cost.or_else(|| {
            event.canonical_facts().iter().find_map(|fact| match fact {
                GatewayEvent::CalculatedCost(cost) => Some(cost.clone()),
                _ => None,
            })
        });
    }
    assert_eq!(
        cost.unwrap()
            .into_estimate()
            .total()
            .unwrap()
            .amount()
            .canonical(),
        "0.000003"
    );
    let requests = server.received_requests().await.unwrap();
    let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(sent["model"], CANONICAL_MODEL);
}
