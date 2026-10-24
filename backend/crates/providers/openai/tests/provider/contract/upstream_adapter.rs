//! 验证插件上游适配复用原生账号、保留槽位与凭据更新

use super::*;
use gateway_core::account::AccountRuntimeSignals;
use gateway_core::account::FastMode;
use gateway_core::engine::upstream_adapter::{
    UpstreamAccountConnection, UpstreamAdapter, UpstreamAdapterInvocation, UpstreamAdapterPlan,
    UpstreamAdapterSelection,
};

#[derive(Debug)]
struct AdapterProbe {
    polls: Arc<std::sync::atomic::AtomicUsize>,
}

impl UpstreamAdapterPlan for AdapterProbe {
    fn select(
        &self,
        _: &AttemptContext,
        provider: &ProviderKind,
        _: &UpstreamModelId,
        _: Option<&ProviderAccountId>,
    ) -> Result<UpstreamAdapterSelection, gateway_core::error::ProviderError> {
        assert_eq!(provider.as_str(), "openai");
        Ok(UpstreamAdapterSelection::Adapter(Arc::new(Self {
            polls: self.polls.clone(),
        })))
    }
}

impl gateway_core::engine::upstream_adapter::UpstreamAdapter for AdapterProbe {
    fn transport(&self) -> &str {
        "http_sse"
    }

    fn execute(
        self: Arc<Self>,
        invocation: gateway_core::engine::upstream_adapter::UpstreamAdapterInvocation,
    ) -> gateway_core::engine::provider::EventStream {
        Box::pin(futures::stream::once(async move {
            self.polls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                invocation.account.account_id().as_str(),
                "acct_provider_contract"
            );
            assert_eq!(
                invocation.metadata.provider_account_id(),
                invocation.account.account_id()
            );
            assert_eq!(invocation.account.authentication_kind(), "oauth");
            let headers = invocation.account.authorization().unwrap();
            let header = |name: &str| {
                headers
                    .iter()
                    .find(|h| h.name() == name)
                    .map(|h| h.value().to_vec())
            };
            assert_eq!(
                header("authorization"),
                Some("Bearer at-acct_provider_contract".as_bytes().to_vec())
            );
            assert_eq!(
                header("chatgpt-account-id"),
                Some("chatgpt-acct_provider_contract".as_bytes().to_vec())
            );
            assert!(header("cookie").is_none());
            assert_eq!(invocation.context.fast_mode(), FastMode::Disabled);
            let Operation::Generate(generate) = &invocation.operation else {
                panic!("adapter must receive the original operation kind");
            };
            assert_eq!(
                generate.protocol_payload().body()["service_tier"],
                "priority"
            );
            assert!(
                invocation
                    .headers
                    .iter()
                    .any(|h| h.name() == "x-adapter-onion")
            );
            assert!(
                invocation
                    .account
                    .calculate_cost(
                        None,
                        &gateway_core::metering::Usage {
                            input_tokens: Some(7),
                            output_tokens: Some(2),
                            ..Default::default()
                        }
                    )
                    .is_some()
            );
            let usage = gateway_core::metering::Usage {
                input_tokens: Some(7),
                output_tokens: Some(2),
                ..Default::default()
            };
            assert_eq!(
                invocation.account.calculate_cost(Some("priority"), &usage),
                invocation.account.calculate_cost(Some("default"), &usage),
                "响应回显不得覆盖已冻结的禁用 Fast 策略"
            );
            Err(gateway_core::error::ProviderError::new(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))
        }))
    }
}

#[tokio::test]
async fn upstream_adapter_reuses_selected_native_account_inside_attempt_onion_and_stays_cold() {
    let store = Arc::new(MemoryAccountStore::default());
    create_account(&store, "acct_provider_contract").await;
    let server = MockServer::start().await;
    let provider = provider_with_base_url(&store, server.uri());
    let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let lease = ExtensionSetReference::new(
        ExtensionSetId::new("adapter-native-test".to_owned()).unwrap(),
        Arc::new(TestExtensionLease),
    );
    let middleware = Arc::new(RecordingMiddleware {
        observed: Default::default(),
        replacement: ("service_tier".into(), json!("priority")),
        request_headers: vec![MiddlewareHeader::new(
            "x-adapter-onion",
            Bytes::from_static(b"present"),
        )],
    });
    let context = AttemptContext::new(
        gateway_core::engine::RequestAttemptContext::new(
            ModelRequestId::new("req_adapter_native").unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_fast_mode(FastMode::Disabled)
        .with_upstream_adapters(Some(
            gateway_core::engine::upstream_adapter::FrozenUpstreamAdapterPlan::new(
                Arc::new(AdapterProbe {
                    polls: polls.clone(),
                }),
                lease.clone(),
            ),
        ))
        .with_middleware(
            Some(FrozenMiddlewarePlan::new(middleware, lease)),
            Arc::from([]),
            "/v1/responses".to_owned(),
            ClientTransport::HttpSse,
        ),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(5),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    );
    let mut stream = provider
        .execute(planned_request("openai", generate_operation()), context)
        .await
        .unwrap();
    assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 0);
    let error = stream.next().await.unwrap().unwrap_err();
    assert_eq!(error.kind(), ProviderErrorKind::Cancelled);
    assert_eq!(polls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(server.received_requests().await.unwrap().is_empty());
}

type SelectedConnection = Arc<dyn UpstreamAccountConnection>;

#[derive(Debug, Clone)]
struct AccountScopedProbe {
    available: bool,
    oauth: bool,
}

impl UpstreamAdapterPlan for AccountScopedProbe {
    fn select(
        &self,
        _: &AttemptContext,
        _: &ProviderKind,
        _: &UpstreamModelId,
        account: Option<&ProviderAccountId>,
    ) -> Result<UpstreamAdapterSelection, gateway_core::error::ProviderError> {
        let Some(account) = account else {
            return Ok(UpstreamAdapterSelection::PendingAccount);
        };
        if account.as_str() != "acct_subagent_a" {
            return Ok(UpstreamAdapterSelection::Native);
        }
        if !self.available {
            return Err(gateway_core::error::ProviderError::new(
                ProviderErrorKind::Unavailable,
                UpstreamSendState::NotSent,
            ));
        }
        Ok(UpstreamAdapterSelection::Adapter(Arc::new(self.clone())))
    }
}

impl UpstreamAdapter for AccountScopedProbe {
    fn transport(&self) -> &str {
        "http_json"
    }
    fn supports_authentication(&self, kind: &str) -> bool {
        self.oauth && kind == "oauth"
    }
    fn execute(
        self: Arc<Self>,
        invocation: UpstreamAdapterInvocation,
    ) -> gateway_core::engine::provider::EventStream {
        assert_eq!(invocation.account.account_id().as_str(), "acct_subagent_a");
        Box::pin(futures::stream::once(async {
            Err(gateway_core::error::ProviderError::new(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))
        }))
    }
}

#[tokio::test]
async fn account_scoped_adapter_selection_reuses_native_pool_and_filters_ineligible_accounts() {
    for (available, oauth) in [(true, true), (true, false), (false, true)] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_subagent_a").await;
        store.set_scheduling("acct_subagent_a", None, AccountWeight::new(100).unwrap());
        let server = MockServer::start().await;
        store
            .seed_api_key(
                "acct_subagent_b",
                server.uri(),
                provider_openai::credential::ResponsesTransport::Http,
            )
            .await;
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(CAPTURE_COMPLETED_SSE),
            )
            .expect(1)
            .mount(&server)
            .await;
        let provider = provider_with_base_url(&store, server.uri());
        let request_context = || {
            AttemptContext::new(
                RequestAttemptContext::new(
                    ModelRequestId::new("req_account_adapter").unwrap(),
                    ClientApiKeyId::new("key_openai_contract").unwrap(),
                )
                .with_upstream_adapters(Some(
                    gateway_core::engine::upstream_adapter::FrozenUpstreamAdapterPlan::new(
                        Arc::new(AccountScopedProbe { available, oauth }),
                        ExtensionSetReference::new(
                            ExtensionSetId::new("account-adapters".to_owned()).unwrap(),
                            Arc::new(TestExtensionLease),
                        ),
                    ),
                )),
                NonZeroU32::MIN,
                SystemTime::now() + Duration::from_secs(5),
                account_policy(),
                AccountAttemptContext::new(BTreeSet::new(), None, None)
                    .with_account_scope(contract_account_scope()),
                None,
                CancellationToken::new(),
            )
        };
        if available && oauth {
            let mut stream = provider
                .clone()
                .execute(
                    planned_request("openai", generate_operation()),
                    request_context(),
                )
                .await
                .unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_subagent_a"
            );
            assert_eq!(
                stream.next().await.unwrap().unwrap_err().kind(),
                ProviderErrorKind::Cancelled
            );
            drop(stream);
            assert!(server.received_requests().await.unwrap().is_empty());
            store
                .set_enabled(&ProviderAccountId::new("acct_subagent_a").unwrap(), false)
                .await
                .unwrap();
        }
        let mut stream = provider
            .execute(
                planned_request("openai", generate_operation()),
                request_context(),
            )
            .await
            .unwrap();
        assert_eq!(
            stream.metadata().provider_account_id().as_str(),
            "acct_subagent_b"
        );
        while let Some(event) = stream.next().await {
            event.unwrap();
        }
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn account_scoped_adapter_preselection_keeps_extension_continuations_out_of_native_checks() {
    use gateway_core::operation::ExtensionSessionOwner;

    for attempt in [
        ContinuationAttempt::Native,
        ContinuationAttempt::ReplayOwner,
        ContinuationAttempt::ReplayAny,
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_subagent_a").await;
        let server = MockServer::start().await;
        let provider = provider_with_base_url(&store, server.uri());
        let kind = ProviderKind::new("openai").unwrap();
        let account = ProviderAccountId::new("acct_subagent_a").unwrap();
        let key = ClientApiKeyId::new("key_openai_contract").unwrap();
        let pin = NativeContinuationPin::new(
            PreviousResponseId::new("public-response"),
            PreviousResponseId::new("adapter-response"),
            key.clone(),
            kind.clone(),
            account.clone(),
        );
        let context = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_scoped_continuation").unwrap(),
                key,
            )
            .with_upstream_adapters(Some(
                gateway_core::engine::upstream_adapter::FrozenUpstreamAdapterPlan::new(
                    Arc::new(AccountScopedProbe {
                        available: true,
                        oauth: true,
                    }),
                    ExtensionSetReference::new(
                        ExtensionSetId::new("account-adapters".to_owned()).unwrap(),
                        Arc::new(TestExtensionLease),
                    ),
                ),
            )),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(5),
            account_policy(),
            AccountAttemptContext::new(
                Default::default(),
                None,
                Some(ProviderAccountStateOwner::new(kind, account)),
            )
            .with_account_scope(contract_account_scope()),
            Some(ContinuationBinding::Pinned(pin)),
            CancellationToken::new(),
        )
        .with_continuation_attempt(attempt);
        let state = ProviderSessionState::new(
            "openai",
            Map::from_iter([("opaque".into(), json!("adapter-only"))]),
        )
        .unwrap()
        .with_extension_owner(ExtensionSessionOwner {
            instance_id: "instance".into(),
            contribution_id: "adapter".into(),
            adapter_id: "example".into(),
            generation: 1,
            incarnation: "process".into(),
            connection_local: true,
        });
        let operation = generate_operation().with_provider_session_state(state);
        let result = provider
            .execute(planned_request("openai", operation), context)
            .await;
        if attempt == ContinuationAttempt::ReplayAny {
            let error = result
                .err()
                .expect("unowned extension state cannot migrate");
            assert_eq!(error.send_state(), UpstreamSendState::NotSent);
        } else {
            let mut stream = result.unwrap();
            assert_eq!(
                stream.metadata().provider_account_id().as_str(),
                "acct_subagent_a"
            );
            assert_eq!(
                stream.next().await.unwrap().unwrap_err().kind(),
                ProviderErrorKind::Cancelled
            );
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

struct ConnectionProbe {
    selected: Arc<Mutex<Option<oneshot::Sender<SelectedConnection>>>>,
}

impl std::fmt::Debug for ConnectionProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConnectionProbe")
            .finish_non_exhaustive()
    }
}

impl UpstreamAdapterPlan for ConnectionProbe {
    fn select(
        &self,
        _: &AttemptContext,
        _: &ProviderKind,
        _: &UpstreamModelId,
        _: Option<&ProviderAccountId>,
    ) -> Result<UpstreamAdapterSelection, gateway_core::error::ProviderError> {
        Ok(UpstreamAdapterSelection::Adapter(Arc::new(Self {
            selected: Arc::clone(&self.selected),
        })))
    }
}

impl UpstreamAdapter for ConnectionProbe {
    fn transport(&self) -> &str {
        "http_sse"
    }

    fn execute(
        self: Arc<Self>,
        invocation: UpstreamAdapterInvocation,
    ) -> gateway_core::engine::provider::EventStream {
        Box::pin(futures::stream::once(async move {
            assert!(
                self.selected
                    .lock()
                    .unwrap()
                    .take()
                    .unwrap()
                    .send(invocation.account)
                    .is_ok()
            );
            Err(gateway_core::error::ProviderError::new(
                ProviderErrorKind::Cancelled,
                UpstreamSendState::NotSent,
            ))
        }))
    }
}

async fn selected_adapter_connection(
    provider: Arc<CodexProvider>,
    request_id: &str,
) -> SelectedConnection {
    let (selected, receiver) = oneshot::channel();
    let plan = gateway_core::engine::upstream_adapter::FrozenUpstreamAdapterPlan::new(
        Arc::new(ConnectionProbe {
            selected: Arc::new(Mutex::new(Some(selected))),
        }),
        ExtensionSetReference::new(
            ExtensionSetId::new("adapter-credential-test".to_owned()).unwrap(),
            Arc::new(TestExtensionLease),
        ),
    );
    let context = AttemptContext::new(
        RequestAttemptContext::new(
            ModelRequestId::new(request_id).unwrap(),
            ClientApiKeyId::new("key_openai_contract").unwrap(),
        )
        .with_upstream_adapters(Some(plan)),
        NonZeroU32::MIN,
        SystemTime::now() + Duration::from_secs(5),
        account_policy(),
        AccountAttemptContext::new(BTreeSet::new(), None, None)
            .with_account_scope(contract_account_scope()),
        None,
        CancellationToken::new(),
    );
    let mut stream = provider
        .execute(planned_request("openai", generate_operation()), context)
        .await
        .unwrap();
    assert_eq!(
        stream.next().await.unwrap().unwrap_err().kind(),
        ProviderErrorKind::Cancelled
    );
    receiver.await.unwrap()
}

#[tokio::test]
async fn guardian_reservation_survives_upstream_adapters_and_metadata_precedence() {
    // 普通池上限 2 且已满，审批独立池仍有空位；关闭独立池后审批也受普通容量约束
    for (subagent, turn_kind, reserved, allowed) in [
        (Some("guardian"), None, 1, true),
        (None, Some("guardian"), 1, true),
        (Some("collab_spawn"), Some("guardian"), 1, true),
        (Some("guardian"), Some("collab_spawn"), 1, false),
        (Some("collab_spawn"), None, 1, false),
        (None, None, 1, false),
        (None, None, 0, false),
        (Some("guardian"), None, 0, false),
    ] {
        let store = Arc::new(MemoryAccountStore::default());
        create_account(&store, "acct_provider_contract").await;
        let leases = Arc::new(TestLeaseCoordinator::default());
        leases.signals.lock().unwrap().insert(
            ProviderAccountId::new("acct_provider_contract").unwrap(),
            AccountRuntimeSignals {
                in_flight: 2,
                last_started_at: None,
                quota_reset_at: None,
                quota_remaining_rank: None,
                cooldown: None,
                failure_rate_basis_points: None,
                first_output_latency_ms: None,
            },
        );
        let server = MockServer::start().await;
        let provider = provider_with_affinity_and_base_url_and_leases(
            &store,
            Arc::new(MemorySessionAffinity::default()),
            server.uri(),
            leases.clone(),
        );
        let (selected, receiver) = oneshot::channel();
        let plan = gateway_core::engine::upstream_adapter::FrozenUpstreamAdapterPlan::new(
            Arc::new(ConnectionProbe {
                selected: Arc::new(Mutex::new(Some(selected))),
            }),
            ExtensionSetReference::new(
                ExtensionSetId::new("guardian-adapter-test".to_owned()).unwrap(),
                Arc::new(TestExtensionLease),
            ),
        );
        let context = AttemptContext::new(
            RequestAttemptContext::new(
                ModelRequestId::new("req_guardian_adapter").unwrap(),
                ClientApiKeyId::new("key_openai_contract").unwrap(),
            )
            .with_upstream_adapters(Some(plan)),
            NonZeroU32::MIN,
            SystemTime::now() + Duration::from_secs(5),
            account_policy().with_openai_guardian_reserved_concurrency(reserved),
            AccountAttemptContext::new(BTreeSet::new(), None, None)
                .with_account_scope(contract_account_scope()),
            None,
            CancellationToken::new(),
        );
        let mut body = Map::from_iter([
            ("model".into(), json!("gpt-5.4")),
            ("input".into(), json!("guardian scheduling contract")),
        ]);
        if let Some(kind) = subagent {
            body.insert("client_metadata".into(), json!({"x-openai-subagent": kind}));
        }
        let mut protocol_context = Map::new();
        if let Some(kind) = turn_kind {
            protocol_context.insert(
                "turn_metadata".into(),
                json!(json!({"subagent_kind": kind}).to_string()),
            );
        }
        let operation = Operation::Generate(GenerateRequest::from_protocol_payload(
            ProtocolPayload::json_object("openai", body)
                .unwrap()
                .with_context(protocol_context),
        ));
        let result = provider
            .execute(planned_request("openai", operation), context)
            .await;
        assert_eq!(
            result.is_ok(),
            allowed,
            "{subagent:?}, {turn_kind:?}, reserve={reserved}"
        );
        if allowed {
            let mut stream = result.unwrap();
            assert_eq!(
                stream.next().await.unwrap().unwrap_err().kind(),
                ProviderErrorKind::Cancelled
            );
            drop(receiver.await.unwrap());
            let requests = leases.requests.lock().unwrap();
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].max_concurrent().get(), 1);
            assert_eq!(
                requests[0].concurrency_pool(),
                gateway_core::provider_ports::ProviderConcurrencyPool::Reserved
            );
        } else {
            assert!(leases.requests.lock().unwrap().is_empty());
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn upstream_adapter_reloads_rotated_credentials_and_proxy_without_accepting_stale_failure() {
    use gateway_core::account::OutboundProxy;

    let store = Arc::new(MemoryAccountStore::default());
    let account_id = "acct_provider_contract";
    create_account(&store, account_id).await;
    let old_proxy = OutboundProxy::parse("http://127.0.0.1:18001").unwrap();
    let new_proxy = OutboundProxy::parse("http://127.0.0.1:18002").unwrap();
    store.set_egress(account_id, Some(old_proxy.clone()), None);
    let upstream = MockServer::start().await;
    let provider = provider_with_base_url(&store, upstream.uri());
    let old =
        selected_adapter_connection(Arc::clone(&provider), "req_adapter_before_refresh").await;
    let before = store.account(account_id).unwrap();
    assert_eq!(old.outbound_proxy(), Some(&old_proxy));

    // 使用原生刷新成功后的 CAS 入口；适配器不能复制令牌或缓存第二份账号代理
    let revision = store
        .repository()
        .rotate_refreshed_oauth_secret(
            &before,
            secret("rotated-adapter-access-token"),
            Some(SystemTime::now() + Duration::from_secs(3600)),
            None,
        )
        .await
        .unwrap();
    store.set_egress(account_id, Some(new_proxy.clone()), None);
    let current = selected_adapter_connection(provider, "req_adapter_after_refresh").await;
    assert_eq!(current.account_id(), old.account_id());
    assert_eq!(current.credential_revision(), revision);
    assert_ne!(current.credential_revision(), old.credential_revision());
    assert_eq!(current.outbound_proxy(), Some(&new_proxy));
    assert_eq!(old.outbound_proxy(), Some(&old_proxy));
    for (connection, expected) in [
        (&old, b"Bearer at-acct_provider_contract".as_slice()),
        (&current, b"Bearer rotated-adapter-access-token".as_slice()),
    ] {
        let headers = connection.authorization().unwrap();
        assert_eq!(
            headers
                .iter()
                .find(|header| header.name() == "authorization")
                .unwrap()
                .value(),
            expected
        );
        assert_eq!(
            headers
                .iter()
                .find(|header| header.name() == "chatgpt-account-id")
                .unwrap()
                .value(),
            b"chatgpt-acct_provider_contract".as_slice()
        );
    }

    // 旧请求的 401 不能使已轮换凭据失效，未发送失败也不能污染账号状态
    for (connection, send_state) in [
        (&old, UpstreamSendState::Sent),
        (&current, UpstreamSendState::NotSent),
    ] {
        let error = connection
            .record_failure(gateway_core::error::ProviderError::new(
                ProviderErrorKind::Unauthorized,
                send_state,
            ))
            .await;
        assert_eq!(error.kind(), ProviderErrorKind::Unauthorized);
        assert_eq!(error.send_state(), send_state);
        let after = store.account(account_id).unwrap();
        assert_eq!(after.revision(), revision);
        assert_eq!(after.credential_state(), CredentialState::Ready);
    }
    current
        .record_failure(gateway_core::error::ProviderError::new(
            ProviderErrorKind::Unauthorized,
            UpstreamSendState::Sent,
        ))
        .await;
    assert_eq!(
        store.account(account_id).unwrap().credential_state(),
        CredentialState::Expired
    );
    assert!(upstream.received_requests().await.unwrap().is_empty());
}
