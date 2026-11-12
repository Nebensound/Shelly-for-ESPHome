//! Unit tests of `crate::rpc_client` (not counted as production code for coverage).

use axum::{
    Router,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};

use super::*;

fn credentials(text: &str) -> Credentials {
    text.parse().expect("credentials")
}

#[test]
fn answers_the_sha256_example_of_rfc_7616() {
    let challenge = r#"Digest realm="http-auth@example.org", qop="auth, auth-int", algorithm=SHA-256, nonce="7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v", opaque="FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS""#;
    let header = authorization(
        challenge,
        &credentials("Mufasa:Circle of Life"),
        "GET",
        "/dir/index.html",
        "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
    )
    .expect("header");
    assert!(header.contains(
        r#"response="753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1""#
    ));
    assert!(header.starts_with(r#"Digest username="Mufasa", realm="http-auth@example.org""#));
    assert!(header.ends_with(r#", opaque="FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS""#));
}

#[test]
fn refuses_challenges_it_cannot_answer() {
    let login = credentials("admin:secret");
    let answer = |challenge: &str| authorization(challenge, &login, "POST", "/rpc", "c");
    let good = r#"Digest qop="auth", realm="shelly", nonce="5", algorithm=SHA-256"#;
    assert!(answer(good).expect("answered").contains(r#"realm="shelly", nonce="5""#));
    assert!(!answer(good).expect("answered").contains("opaque"));
    for challenge in [
        r#"Digest qop="auth", realm="shelly", nonce="5", algorithm=MD5"#,
        r#"Digest qop="auth", realm="shelly", nonce="5""#,
        r#"Digest qop="auth-int", realm="shelly", nonce="5", algorithm=SHA-256"#,
        r#"Digest realm="shelly", nonce="5", algorithm=SHA-256"#,
        r#"Digest qop="auth", nonce="5", algorithm=SHA-256"#,
        r#"Digest qop="auth", realm="shelly", algorithm=SHA-256"#,
        r#"Basic realm="shelly""#,
        "",
    ] {
        assert_eq!(answer(challenge), None, "{challenge}");
    }
    // An unterminated quote takes the rest as the value.
    assert_eq!(
        challenge_parameters(r#"Digest realm="shelly, nonce=5"#),
        [("realm".to_owned(), "shelly, nonce=5".to_owned())]
    );
}

const PASSWORD: &str = "secret";

/// A device that wants a login: it checks the digest like the stock
/// firmware and answers what the method name asks for.
async fn device(headers: HeaderMap, body: String) -> axum::response::Response {
    let request: serde_json::Value = serde_json::from_str(&body).expect("JSON request");
    let method = request["method"].as_str().unwrap_or_default().to_owned();
    let challenge = r#"Digest qop="auth", realm="shelly1g3-test", nonce="66", algorithm=SHA-256"#;
    let unauthorized =
        (StatusCode::UNAUTHORIZED, [(axum::http::header::WWW_AUTHENTICATE, challenge)]);
    if method.starts_with("Open.") {
        // No login needed.
    } else if method.starts_with("Basic.") {
        let basic = [(axum::http::header::WWW_AUTHENTICATE, r#"Basic realm="x""#)];
        return (StatusCode::UNAUTHORIZED, basic).into_response();
    } else {
        let Some(given) = headers.get("authorization").and_then(|value| value.to_str().ok()) else {
            return unauthorized.into_response();
        };
        let parameters = challenge_parameters(given);
        let get = |name: &str| {
            parameters
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap_or_default()
        };
        let secret = sha256_hex(&format!("admin:shelly1g3-test:{PASSWORD}"));
        let request = sha256_hex(&format!("POST:{}", get("uri")));
        let expected =
            sha256_hex(&format!("{secret}:66:{}:{}:auth:{request}", get("nc"), get("cnonce")));
        if get("username") != "admin" || get("response") != expected {
            return unauthorized.into_response();
        }
    }
    match method.rsplit('.').next().unwrap_or_default() {
        "Ok" => r#"{"id":1,"result":{"answer":42}}"#.into_response(),
        "Empty" => r#"{"id":1}"#.into_response(),
        "InReply" => r#"{"id":1,"error":{"code":-109,"message":"Not committed"}}"#.into_response(),
        "OnItsOwn" => {
            (StatusCode::INTERNAL_SERVER_ERROR, r#"{"code":-114,"message":"busy"}"#).into_response()
        }
        "Missing" => (StatusCode::NOT_FOUND, "no such method").into_response(),
        _ => "not json".into_response(),
    }
}

async fn start() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let app = Router::new().route("/rpc", post(device));
    tokio::spawn(async move { axum::serve(listener, app).await });
    address.to_string()
}

#[tokio::test]
async fn calls_methods_with_and_without_a_login() {
    let host = start().await;
    let open = RpcClient::new(&host, None);
    let params = serde_json::json!({});
    assert_eq!(open.call("Open.Ok", params.clone()).await.expect("ok")["answer"], 42);
    assert_eq!(open.call("Open.Empty", params.clone()).await.expect("ok"), serde_json::Value::Null);
    assert!(matches!(open.call("Sys.Ok", params.clone()).await, Err(RpcError::AuthRequired)));

    let admin = RpcClient::new(&host, Some(credentials(&format!("admin:{PASSWORD}"))));
    assert_eq!(admin.call("Sys.Ok", params.clone()).await.expect("ok")["answer"], 42);
    let wrong = RpcClient::new(&host, Some(credentials("admin:wrong")));
    assert!(matches!(wrong.call("Sys.Ok", params.clone()).await, Err(RpcError::AuthFailed)));
    let other_user = RpcClient::new(&host, Some(credentials(&format!("root:{PASSWORD}"))));
    let refused = other_user.call("Sys.Ok", params.clone()).await.expect_err("user");
    assert!(refused.to_string().contains("use the user admin"));
    assert!(matches!(admin.call("Basic.Ok", params).await, Err(RpcError::AuthFailed)));
}

#[tokio::test]
async fn reports_what_the_device_refuses() {
    let host = start().await;
    let client = RpcClient::new(&host, None);
    let call = |method: &'static str| {
        let client = client.clone();
        async move { client.call(method, serde_json::json!({})).await.expect_err("error") }
    };
    assert_eq!(
        call("Open.InReply").await.to_string(),
        "Open.InReply failed: Not committed (code -109)"
    );
    assert_eq!(call("Open.OnItsOwn").await.to_string(), "Open.OnItsOwn failed: busy (code -114)");
    assert_eq!(call("Open.Missing").await.to_string(), "Open.Missing answered with HTTP 404");
    assert_eq!(
        call("Open.Garbage").await.to_string(),
        "Open.Garbage gave an answer that is no RPC reply"
    );
    let gone = RpcClient::new("127.0.0.1:1", None);
    let error = gone.call("Open.Ok", serde_json::json!({})).await.expect_err("unreachable");
    assert!(error.to_string().starts_with("device unreachable"));
}

/// A device that answers its first connection with `response` and then
/// stops listening.
async fn one_answer(response: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut request = vec![0; 4096];
        let _ = stream.read(&mut request).await;
        let _ = stream.write_all(response.as_bytes()).await;
        let _ = stream.shutdown().await;
    });
    address.to_string()
}

#[tokio::test]
async fn reports_a_device_that_goes_away() {
    // The login is asked for, then the device is gone.
    let challenge = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Digest qop=\"auth\", \
                     realm=\"r\", nonce=\"n\", algorithm=SHA-256\r\nContent-Length: 0\r\n\
                     Connection: close\r\n\r\n";
    let host = one_answer(challenge).await;
    let client = RpcClient::new(&host, Some(credentials("admin:secret")));
    let error = client.call("Sys.Ok", serde_json::json!({})).await.expect_err("gone");
    assert!(matches!(error, RpcError::Http(_)), "{error}");

    // The answer breaks off.
    let partial = "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"id\"";
    let host = one_answer(partial).await;
    let error = RpcClient::new(&host, None)
        .call("Open.Ok", serde_json::json!({}))
        .await
        .expect_err("broken off");
    assert!(matches!(error, RpcError::Http(_)), "{error}");
}
