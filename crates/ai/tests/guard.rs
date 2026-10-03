//! The egress guard through the providers and the direct transport: user
//! URLs, the operator's exact allowlist, a resolver that answers a private
//! address, the loopback switch.

mod support;

use std::collections::HashMap;
use std::io;
use std::net::IpAddr;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use shelfy_ai::direct::DirectTransport;
use shelfy_ai::guard::Resolve;
use shelfy_ai::{
    ChatRequest, Egress, EgressPolicy, ErrorKind, Message, Origin, Provider, ProviderConfig,
    ProviderKind, Source,
};
use support::*;
use url::Url;

/// Answers names from a fixed table.
struct Table(HashMap<&'static str, Vec<IpAddr>>);

impl Resolve for Table {
    fn resolve<'a>(&'a self, host: &'a str, _port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        let answer = self
            .0
            .get(host)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"));
        Box::pin(async move { answer })
    }
}

fn table() -> Arc<Table> {
    let ip = |text: &str| text.parse::<IpAddr>().unwrap();
    Arc::new(Table(HashMap::from([
        ("rebind.example.com", vec![ip("10.0.0.7")]),
        (
            "mixed.example.com",
            vec![ip("93.184.216.34"), ip("169.254.169.254")],
        ),
        ("public.example.com", vec![ip("93.184.216.34")]),
        ("loopback.test", vec![ip("127.0.0.1")]),
        ("elsewhere.test", vec![ip("10.9.9.9")]),
    ])))
}

fn user_provider(base: &str, transport: DirectTransport) -> Result<Provider, shelfy_ai::AiError> {
    let config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        Source::User,
        Url::parse(base).unwrap(),
    );
    Provider::new(config, &EgressPolicy::new(), Arc::new(transport))
}

fn hello() -> ChatRequest {
    ChatRequest::new("m", vec![Message::user_text("hello")])
}

#[tokio::test]
async fn a_name_that_resolves_to_a_private_address_is_refused() {
    for name in ["rebind.example.com", "mixed.example.com"] {
        let provider = user_provider(
            &format!("https://{name}/v1"),
            DirectTransport::with_resolver(table()),
        )
        .unwrap();
        assert_eq!(provider.egress(), Egress::Public);
        let error = provider.chat(&hello(), &options()).await.unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Refused, "{name}: {error}");
        assert!(
            error.message().contains("is not a public address"),
            "{error}"
        );
    }
}

#[tokio::test]
async fn public_providers_never_go_through_the_direct_transport() {
    let provider = user_provider(
        "https://public.example.com/v1",
        DirectTransport::with_resolver(table()),
    )
    .unwrap();
    let error = provider.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported, "{error}");
}

#[tokio::test]
async fn user_urls_are_refused_at_construction() {
    for base in [
        "http://api.openai.com/v1",
        "https://127.0.0.1/v1",
        "https://2130706433/v1",
        "https://0x7f.0.0.1/v1",
        "https://[::ffff:127.0.0.1]/v1",
        "https://169.254.169.254/latest",
        "https://100.94.10.20:8080/v1",
        "https://user:pass@api.example.com/v1",
    ] {
        let error = user_provider(base, DirectTransport::new()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Refused, "{base}");
    }
}

#[tokio::test]
async fn the_operators_host_is_off_limits_to_users_on_any_port_or_scheme() {
    let operator_base = Url::parse("http://100.94.10.20:8080/v1").unwrap();
    let policy = EgressPolicy::new().allow(Origin::of(&operator_base).unwrap());
    let transport = Arc::new(DirectTransport::new());
    let operator = Provider::new(
        ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            Source::Operator,
            operator_base.clone(),
        ),
        &policy,
        transport.clone(),
    )
    .unwrap();
    assert_eq!(operator.egress(), Egress::Allowlisted);
    for base in [
        "http://100.94.10.20:8080/v1",
        "https://100.94.10.20:8080/v1",
        "http://100.94.10.20:9000/v1",
    ] {
        let config = ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            Source::User,
            Url::parse(base).unwrap(),
        );
        let error = Provider::new(config, &policy, transport.clone()).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::Refused, "{base}");
        assert!(
            error.message().contains("reserved for the operator"),
            "{error}"
        );
    }
    // The operator's own STT endpoint on another port is not allowlisted.
    let stt = ProviderConfig::new(
        ProviderKind::WhisperCpp,
        Source::Operator,
        Url::parse("http://100.94.10.20:8178/inference").unwrap(),
    );
    assert_eq!(
        Provider::new(stt, &policy, transport).unwrap_err().kind(),
        ErrorKind::Refused
    );
}

#[tokio::test]
async fn the_loopback_switch_lets_a_user_provider_reach_the_stub() {
    let stub = stub().await;
    let config = ProviderConfig::new(
        ProviderKind::OpenAiCompatible,
        Source::User,
        stub.openai_base(),
    );
    let refused = Provider::new(
        config.clone(),
        &EgressPolicy::new(),
        Arc::new(DirectTransport::new()),
    );
    assert_eq!(refused.unwrap_err().kind(), ErrorKind::Refused);

    let policy = EgressPolicy::new().allow_loopback(true);
    let provider = Provider::new(config, &policy, Arc::new(DirectTransport::new())).unwrap();
    assert_eq!(provider.egress(), Egress::Loopback);
    provider.chat(&hello(), &options()).await.unwrap();
}

#[tokio::test]
async fn the_loopback_route_reaches_loopback_addresses_only() {
    let stub = stub().await;
    let port = stub.addr().port();
    let policy = EgressPolicy::new().allow_loopback(true);
    let transport = Arc::new(DirectTransport::with_resolver(table()));
    let reach = |host: &str| {
        let base = Url::parse(&format!("http://{host}:{port}/v1")).unwrap();
        Provider::new(
            ProviderConfig::new(ProviderKind::OpenAiCompatible, Source::User, base),
            &policy,
            transport.clone(),
        )
    };
    // `loopback.test` is not a loopback name: refused before any lookup.
    assert_eq!(
        reach("loopback.test").unwrap_err().kind(),
        ErrorKind::Refused
    );
    // `localhost` is, but must resolve to loopback.
    let localhost = Provider::new(
        ProviderConfig::new(
            ProviderKind::OpenAiCompatible,
            Source::User,
            Url::parse(&format!("http://localhost:{port}/v1")).unwrap(),
        ),
        &policy,
        Arc::new(DirectTransport::with_resolver(Arc::new(Table(
            HashMap::from([("localhost", vec!["10.1.1.1".parse().unwrap()])]),
        )))),
    )
    .unwrap();
    let error = localhost.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Refused, "{error}");
    assert!(
        error.message().contains("not a loopback address"),
        "{error}"
    );
}

#[tokio::test]
async fn a_name_that_does_not_resolve_is_offline() {
    let base = Url::parse("http://ornith.invalid:8080/v1").unwrap();
    let policy = EgressPolicy::new().allow(Origin::of(&base).unwrap());
    let provider = Provider::new(
        ProviderConfig::new(ProviderKind::OpenAiCompatible, Source::Operator, base),
        &policy,
        Arc::new(DirectTransport::with_resolver(table())),
    )
    .unwrap();
    let error = provider.chat(&hello(), &options()).await.unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Offline, "{error}");
    assert!(error.message().contains("did not resolve"), "{error}");
}
