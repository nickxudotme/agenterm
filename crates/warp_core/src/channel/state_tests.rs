use super::derive_http_origin_from_ws_url;
use super::state::ChannelState;

#[test]
fn wss_becomes_https_and_strips_path() {
    let got = derive_http_origin_from_ws_url("wss://rtc.app.warp.dev/graphql/v2");
    assert_eq!(got.as_deref(), Some("https://rtc.app.warp.dev"));
}

#[test]
fn ws_becomes_http_and_preserves_port() {
    let got = derive_http_origin_from_ws_url("ws://localhost:8080/graphql/v2");
    assert_eq!(got.as_deref(), Some("http://localhost:8080"));
}

#[test]
fn unparseable_input_returns_none() {
    assert!(derive_http_origin_from_ws_url("not a url").is_none());
    assert!(derive_http_origin_from_ws_url("https://app.warp.dev").is_none());
}

/// `ChannelState::init()` — the static default for Agenterm builds — must satisfy the local
/// Warp Drive predicate, because the panel's no-account, no-cloud behavior depends on it.
#[test]
fn default_oss_state_uses_local_warp_drive() {
    assert!(ChannelState::is_local_warp_drive());
}
