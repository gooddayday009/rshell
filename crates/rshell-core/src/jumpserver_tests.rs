use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

const USER: &str = "00000000-0000-0000-0000-000000000010";
const ORG: &str = "00000000-0000-0000-0000-000000000002";
const NODE: &str = "00000000-0000-0000-0000-000000000020";
const ASSET: &str = "00000000-0000-0000-0000-000000000030";
const TOKEN: &str = "00000000-0000-0000-0000-000000000040";

struct Step {
    method: &'static str,
    path: &'static str,
    response: Value,
    body: Option<Value>,
    cookie: bool,
    bearer: bool,
    status: u16,
    headers: &'static str,
}
impl Step {
    fn get(path: &'static str, response: Value) -> Self {
        Self {
            method: "GET",
            path,
            response,
            body: None,
            cookie: false,
            bearer: true,
            status: 200,
            headers: "",
        }
    }
    fn post(path: &'static str, body: Value, response: Value) -> Self {
        Self {
            method: "POST",
            path,
            response,
            body: Some(body),
            cookie: false,
            bearer: false,
            status: 200,
            headers: "",
        }
    }
}

/// Only binds loopback. Fixtures mirror the v4.10.2 serializers, not a real deployment.
fn server(steps: Vec<Step>) -> (Settings, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        for step in steps {
            let started = std::time::Instant::now();
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            started.elapsed() < Duration::from_secs(10),
                            "missing expected API request"
                        );
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("mock accept: {e}"),
                }
            };
            // Windows may inherit the listener's nonblocking mode on accepted sockets.
            stream.set_nonblocking(false).unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            let (headers, content_start, length) = loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0 && data.len() < 64 * 1024);
                data.extend_from_slice(&chunk[..n]);
                if let Some(index) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                    let headers = String::from_utf8(data[..index].to_vec()).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|v| v.parse::<usize>().ok())
                        })
                        .unwrap_or(0);
                    break (headers, index + 4, length);
                }
            };
            while data.len() < content_start + length {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).unwrap();
                assert!(n > 0);
                data.extend_from_slice(&chunk[..n]);
            }
            assert_eq!(
                headers.lines().next().unwrap(),
                format!("{} /{} HTTP/1.1", step.method, step.path)
            );
            let lower = headers.to_ascii_lowercase();
            assert_eq!(
                lower.contains("authorization: bearer test-bearer"),
                step.bearer
            );
            if step.cookie {
                assert!(lower.contains("cookie: sessionid=test-mfa-session"));
            }
            if let Some(body) = step.body {
                assert_eq!(
                    serde_json::from_slice::<Value>(&data[content_start..content_start + length])
                        .unwrap(),
                    body
                );
            }
            let response = step.response.to_string();
            write!(stream, "HTTP/1.1 {} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}", step.status, response.len(), step.headers, response).unwrap();
        }
    });
    (
        Settings {
            url: format!("http://{address}"),
            allow_http: true,
            username: "alice".into(),
            ssh_host: "koko.test.invalid".into(),
            ..Default::default()
        },
        handle,
    )
}

fn permissions() -> Value {
    json!({"id":USER,"username":"alice","workbench_orgs":[{"id":ORG,"name":"Default"}]})
}
fn login_response() -> Value {
    json!({"token":"test-bearer","date_expired":"2099/01/01 08:00:00 +0800","user":{"id":USER}})
}
fn asset() -> Asset {
    Asset {
        id: Uuid::parse_str(ASSET).unwrap(),
        name: "web-01".into(),
        address: "10.0.0.8".into(),
        nodes: vec![Uuid::parse_str(NODE).unwrap()],
        is_active: true,
    }
}
fn wire_asset() -> Value {
    json!({
        "id": ASSET, "name": "web-01", "address": "10.0.0.8",
        "nodes": [{"id": NODE, "name": "Production"}], "is_active": true
    })
}

#[test]
fn asset_node_objects_preserve_identity_and_cache_compatibility() {
    let parsed: Asset = serde_json::from_value(wire_asset()).unwrap();
    assert_eq!(parsed.nodes, asset().nodes);
    let cached = serde_json::to_value(&parsed).unwrap();
    assert_eq!(cached["nodes"], json!([NODE]));
    let restored: Asset = serde_json::from_value(cached).unwrap();
    assert_eq!(restored.nodes, parsed.nodes);
    for nodes in [
        json!([{"name":"missing-id"}]),
        json!([{"id":"invalid"}]),
        json!([42]),
        json!(null),
    ] {
        let mut value = wire_asset();
        value["nodes"] = nodes;
        assert!(serde_json::from_value::<Asset>(value).is_err());
    }
    let mut value = wire_asset();
    value.as_object_mut().unwrap().remove("nodes");
    assert!(
        serde_json::from_value::<Asset>(value)
            .unwrap()
            .nodes
            .is_empty()
    );
}

fn account(name: &str) -> Account {
    Account {
        alias: name.into(),
        name: name.into(),
        username: name.into(),
        has_secret: true,
        has_username: true,
        actions: json!([{"value":"connect","label":"Connect (All protocols)"}]),
    }
}
fn details() -> Value {
    json!({"id":ASSET,"permed_protocols":[{"name":"ssh","port":22,"public":false}],
        "permed_accounts":[{"alias":"deploy","name":"Deploy account","username":"deploy","has_username":true,"has_secret":true,"actions":[{"value":"connect","label":"Connect"}]}]})
}
fn logged_client(settings: Settings) -> Client {
    let mut client = Client::new(settings).unwrap();
    client.token = Some(Zeroizing::new("test-bearer".into()));
    client.user_id = USER.into();
    client.username = "alice".into();
    client.settings.org_id = ORG.into();
    client.organizations = vec![Organization {
        id: ORG.into(),
        name: "Default".into(),
    }];
    client.authenticated_at = now();
    client.expires_at = now() + 3600;
    client
}

#[tokio::test]
async fn v4102_password_mfa_cookie_pagination_and_koko_token_flow() {
    let mut login = Step::post(
        "api/v1/authentication/auth/",
        json!({"username":"alice","password":"synthetic-password"}),
        json!({"error":"mfa_required","data":{"choices":["otp"]}}),
    );
    login.headers = "Set-Cookie: sessionid=test-mfa-session; Path=/; HttpOnly\r\n";
    let mut verify = Step::post(
        "api/v1/authentication/mfa/challenge/",
        json!({"type":"otp","code":"123456"}),
        json!({"msg":"ok"}),
    );
    verify.cookie = true;
    let mut finish = Step::post("api/v1/authentication/auth/", json!({}), login_response());
    finish.cookie = true;
    let mut connect = Step::post(
        "api/v1/authentication/connection-token/",
        json!({"asset":ASSET,"account":"deploy","protocol":"ssh","connect_method":"ssh_client","input_username":"","input_secret":"","is_reusable":false}),
        json!({"id":TOKEN,"value":"test-one-time-secret","is_active":true}),
    );
    connect.bearer = true;
    let (settings, worker) = server(vec![
        login,
        verify,
        finish,
        Step::get("api/v1/users/profile/permissions/", permissions()),
        Step::get(
            "api/v1/perms/users/self/nodes/?limit=200&offset=0",
            json!([
                {"id":"favorite","name":"Favorite","key":"favorite","full_value":"Favorite"},
                {"id":"ungrouped","name":"Ungrouped","key":"ungrouped","full_value":"Ungrouped"},
                {"id":NODE,"name":"Production","key":"1:2","full_value":"Default / Production"}
            ]),
        ),
        Step::get(
            "api/v1/perms/users/self/assets/?limit=200&offset=0",
            json!({"count":2,"next":"https://untrusted.invalid/must-not-follow","results":[wire_asset()]}),
        ),
        Step::get(
            "api/v1/perms/users/self/assets/?limit=200&offset=1",
            json!({"count":2,"next":null,"results":[{"id":"00000000-0000-0000-0000-000000000031","name":"same-ip","address":"10.0.0.8","nodes":[],"is_active":true}]}),
        ),
        Step::get(
            "api/v1/perms/users/self/assets/00000000-0000-0000-0000-000000000030/",
            details(),
        ),
        Step::get(
            "api/v1/perms/users/self/assets/00000000-0000-0000-0000-000000000030/",
            details(),
        ),
        connect,
    ]);
    let mut client = Client::new(settings).unwrap();
    assert!(
        matches!(client.login("synthetic-password").await.unwrap(), AuthStep::Mfa(v) if v == ["otp"])
    );
    assert!(!client.logged_in());
    assert!(matches!(
        client.verify_mfa("otp", "123456").await.unwrap(),
        AuthStep::LoggedIn
    ));
    assert_eq!(client.settings.org_id, ORG);
    let snapshot = client.sync().await.unwrap();
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.nodes[0].id.to_string(), NODE);
    assert_eq!(snapshot.assets.len(), 2);
    assert_eq!(snapshot.assets[0].nodes, vec![snapshot.nodes[0].id]);
    assert_ne!(snapshot.assets[0].id, snapshot.assets[1].id);
    let accounts = client.accounts(asset().id).await.unwrap();
    assert_eq!(accounts[0].alias, "deploy");
    let connection = client
        .resolve(asset(), accounts[0].clone(), "", "")
        .await
        .unwrap();
    assert_eq!(connection.username, format!("JMS-{TOKEN}"));
    assert_eq!(connection.secret.as_str(), "test-one-time-secret");
    assert_eq!(client.settings.ssh_host, "koko.test.invalid");
    assert_ne!(client.settings.ssh_host, connection.asset.address);
    worker.join().unwrap();
}

#[test]
fn priority_accounts_require_exact_unique_authorized_match() {
    let accounts = vec![account("root"), account("deploy"), account("ops")];
    assert_eq!(preferred_account(&accounts, &Default::default()), None);
    assert_eq!(
        preferred_account(&accounts, &["deploy".into(), "root".into(), "ops".into()]),
        Some(1)
    );
    assert_eq!(
        preferred_account(&accounts, &["missing".into(), "ops".into(), "root".into()]),
        Some(2)
    );
    assert_eq!(
        preferred_account(
            &accounts,
            &["missing".into(), "missing2".into(), "deploy".into()]
        ),
        Some(1)
    );
    assert_eq!(
        preferred_account(&accounts, &["roo".into(), "".into(), "".into()]),
        None
    );
    assert_eq!(
        preferred_account(&[account("ops")], &Default::default()),
        Some(0)
    );
    let mut duplicate = account("deploy");
    duplicate.alias = "other-deploy".into();
    assert_eq!(
        preferred_account(
            &[account("deploy"), duplicate, account("root")],
            &["deploy".into(), "root".into(), "".into()]
        ),
        None
    );
}

#[tokio::test]
async fn restore_preserves_absolute_expiry_and_revalidates_user() {
    let (settings, worker) = server(vec![Step::get(
        "api/v1/users/profile/permissions/",
        permissions(),
    )]);
    let original = logged_client(settings.clone());
    let saved = original.saved_token().unwrap();
    let mut restored = Client::new(settings).unwrap();
    restored.restore(saved).await.unwrap();
    assert_eq!(restored.expires_at, original.expires_at);
    assert_eq!(restored.authenticated_at, original.authenticated_at);
    worker.join().unwrap();
}

#[tokio::test]
async fn expired_or_different_endpoint_saved_login_is_never_sent() {
    let settings = Settings {
        url: "http://127.0.0.1:1".into(),
        allow_http: true,
        username: "alice".into(),
        ssh_host: "koko".into(),
        ..Default::default()
    };
    let mut original = logged_client(settings.clone());
    let mut saved: Value = serde_json::from_str(&original.saved_token().unwrap()).unwrap();
    saved["expires_at"] = json!(now() - 1);
    let mut restored = Client::new(settings.clone()).unwrap();
    assert!(
        restored
            .restore(Zeroizing::new(saved.to_string()))
            .await
            .unwrap_err()
            .to_string()
            .contains("过期")
    );
    let mut changed = settings;
    changed.url = "http://127.0.0.1:2".into();
    let mut restored = Client::new(changed).unwrap();
    assert!(
        restored
            .restore(original.saved_token().unwrap())
            .await
            .unwrap_err()
            .to_string()
            .contains("不一致")
    );
    original.settings.keep_minutes = 1;
    original.shorten_session();
    assert!(original.expires_at <= original.authenticated_at + 60);
    let before = original.expires_at;
    original.settings.keep_minutes = 43200;
    original.shorten_session();
    assert_eq!(original.expires_at, before);
}

#[tokio::test]
async fn revoked_account_prevents_connection_token_request() {
    let mut detail = details();
    detail["permed_accounts"][0]["actions"] = json!([]);
    let (settings, worker) = server(vec![Step::get(
        "api/v1/perms/users/self/assets/00000000-0000-0000-0000-000000000030/",
        detail,
    )]);
    let client = logged_client(settings);
    assert!(
        client
            .resolve(asset(), account("deploy"), "", "")
            .await
            .is_err()
    );
    worker.join().unwrap();
}

#[tokio::test]
async fn duplicate_pages_fail_instead_of_committing_partial_snapshot() {
    let (settings, worker) = server(vec![
        Step::get(
            "api/v1/perms/users/self/nodes/?limit=200&offset=0",
            json!([]),
        ),
        Step::get(
            "api/v1/perms/users/self/assets/?limit=200&offset=0",
            json!({"next":"anything","results":[asset()]}),
        ),
        Step::get(
            "api/v1/perms/users/self/assets/?limit=200&offset=1",
            json!({"next":null,"results":[asset()]}),
        ),
    ]);
    assert!(
        logged_client(settings)
            .sync()
            .await
            .unwrap_err()
            .to_string()
            .contains("重复")
    );
    worker.join().unwrap();
}

#[tokio::test]
async fn errors_do_not_echo_secrets_and_redirects_are_not_followed() {
    let mut fail = Step::get(
        "api/v1/test/",
        json!({"error":"sensitive-echo","password":"synthetic-secret"}),
    );
    fail.status = 403;
    let mut redirect = Step::get("api/v1/test/", json!({}));
    redirect.status = 302;
    redirect.headers = "Location: http://127.0.0.1:1/other\r\n";
    let (settings, worker) = server(vec![fail, redirect]);
    let client = logged_client(settings);
    let error = client
        .request(Method::GET, "api/v1/test/", None)
        .await
        .unwrap_err()
        .to_string();
    assert!(!error.contains("synthetic-secret") && !error.contains("sensitive-echo"));
    assert!(
        client
            .request(Method::GET, "api/v1/test/", None)
            .await
            .unwrap_err()
            .to_string()
            .contains("重定向")
    );
    worker.join().unwrap();
}

#[test]
fn sync_interval_defaults_migrates_and_roundtrips() {
    let mut settings: Settings = serde_json::from_str("{}").unwrap();
    assert_eq!(settings.sync_interval_minutes, 60);
    assert_eq!(settings.sync_interval(), Duration::from_secs(3600));
    settings.sync_interval_minutes = 15;
    let restored: Settings =
        serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
    assert_eq!(restored.sync_interval(), Duration::from_secs(900));
    for invalid in [0, 43201, u32::MAX] {
        settings.sync_interval_minutes = invalid;
        assert!(
            settings
                .validate()
                .unwrap_err()
                .to_string()
                .contains("刷新间隔")
        );
    }
}

#[test]
fn settings_require_explicit_http_and_have_no_secret_fields() {
    let mut settings = Settings {
        url: "http://example.test:8888".into(),
        ssh_host: "example.test".into(),
        username: "alice".into(),
        ..Default::default()
    };
    assert!(settings.validate().is_err());
    settings.allow_http = true;
    assert!(settings.validate().is_ok());
    settings.remember_password = true;
    let json = serde_json::to_value(&settings).unwrap();
    assert!(json.get("password").is_none() && json.get("token").is_none());
    assert_eq!(json["remember_password"], true);
    settings.url = "http://alice:secret@example.test".into();
    assert!(settings.validate().is_err());
}

#[test]
fn v4102_expiry_format_respects_timezone_and_rejects_missing_offset() {
    let utc = parse_expiry("2026-10-28T08:00:00Z").unwrap();
    assert_eq!(parse_expiry("2026/10/28 16:00:00 +0800").unwrap(), utc);
    assert_eq!(parse_expiry("2026/10/28 03:00:00 -0500").unwrap(), utc);
    assert_eq!(parse_expiry("2026-10-28T16:00:00+08:00").unwrap(), utc);
    assert!(parse_expiry("2026/10/28 16:00:00").is_err());
    assert!(parse_expiry("2026/99/28 16:00:00 +0800").is_err());
}

#[tokio::test]
async fn default_candidates_aggregate_pages_and_filter_permissions() {
    let mut second = wire_asset();
    second["id"] = json!("00000000-0000-0000-0000-000000000031");
    let mut third = wire_asset();
    third["id"] = json!("00000000-0000-0000-0000-000000000032");
    let mut detail = details();
    let mut denied = detail["permed_accounts"][0].clone();
    denied["username"] = json!("denied");
    denied["alias"] = json!("denied");
    denied["actions"] = json!([]);
    detail["permed_accounts"]
        .as_array_mut()
        .unwrap()
        .push(denied);
    let mut second_detail = details();
    second_detail["permed_accounts"][0]["alias"] = json!("ops");
    let mut non_ssh = details();
    non_ssh["permed_protocols"] = json!([{"name":"rdp"}]);
    non_ssh["permed_accounts"][0]["username"] = json!("rdp-only");
    let (settings, server) = server(vec![
        Step::get(
            "api/v1/perms/users/self/assets/?limit=200&offset=0",
            json!({"results":[wire_asset()],"next":"ignored"}),
        ),
        Step::get(
            "api/v1/perms/users/self/assets/?limit=200&offset=1",
            json!({"results":[second, third],"next":null}),
        ),
        Step::get(
            "api/v1/perms/users/self/assets/00000000-0000-0000-0000-000000000030/",
            detail,
        ),
        Step::get(
            "api/v1/perms/users/self/assets/00000000-0000-0000-0000-000000000031/",
            second_detail,
        ),
        Step::get(
            "api/v1/perms/users/self/assets/00000000-0000-0000-0000-000000000032/",
            non_ssh,
        ),
    ]);
    assert_eq!(
        logged_client(settings)
            .default_account_candidates()
            .await
            .unwrap(),
        vec!["deploy", "ops"]
    );
    server.join().unwrap();
}

#[tokio::test]
async fn default_candidates_fail_instead_of_returning_partial_results() {
    let mut failed = Step::get(
        "api/v1/perms/users/self/assets/00000000-0000-0000-0000-000000000030/",
        json!({"detail":"denied"}),
    );
    failed.status = 403;
    let (settings, server) = server(vec![
        Step::get(
            "api/v1/perms/users/self/assets/?limit=200&offset=0",
            json!([wire_asset()]),
        ),
        failed,
    ]);
    assert!(
        logged_client(settings)
            .default_account_candidates()
            .await
            .is_err()
    );
    server.join().unwrap();
}

#[test]
fn password_opt_in_is_disabled_for_legacy_settings() {
    let mut value = serde_json::to_value(Settings::default()).unwrap();
    value.as_object_mut().unwrap().remove("remember_password");
    let settings: Settings = serde_json::from_value(value).unwrap();
    assert!(!settings.remember_password);
}
