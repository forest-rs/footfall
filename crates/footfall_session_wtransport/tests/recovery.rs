// Copyright 2026 the Footfall Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Each way a consumer's request can fail reaches it as a distinct, typed
//! error.

use footfall_session::SessionErrorCode;
use footfall_session_wtransport::{
    CertificateHash, ClientError, SessionClient, SessionIdentity, SessionServer,
    SessionServerConfig,
};
use std::{net::SocketAddr, time::Duration};
use wtransport::{ClientConfig, Endpoint, Identity, ServerConfig};

fn bind() -> (SessionServer, String, CertificateHash) {
    let identity = SessionIdentity::self_signed_localhost().expect("identity should build");
    let hash = identity.certificate_hash();
    let server = SessionServer::bind(&identity, SessionServerConfig::default())
        .expect("server should bind an ephemeral localhost port");
    let url = server.url().expect("bound server should report its URL");
    (server, url, hash)
}

#[tokio::test]
async fn a_wrong_certificate_pin_is_rejected() {
    let (server, url, _) = bind();
    let accept = tokio::spawn(async move { server.accept_request().await });

    let wrong = CertificateHash::from_bytes([7; 32]);
    let error = SessionClient::new(url, wrong)
        .discover()
        .await
        .expect_err("the pin does not match");
    assert!(
        matches!(error, ClientError::CertificateRejected { .. }),
        "{error:?}"
    );
    accept.abort();
}

#[tokio::test]
async fn a_wrong_path_is_rejected_by_the_server() {
    let (server, url, hash) = bind();
    let accept = tokio::spawn(async move { server.accept_request().await });

    let wrong_path = url.replace("/footfall/session", "/elsewhere");
    let error = SessionClient::new(wrong_path, hash)
        .discover()
        .await
        .expect_err("the path is wrong");
    assert!(
        matches!(error, ClientError::SessionRejected { .. }),
        "{error:?}"
    );
    assert!(
        accept.await.expect("accept task should not panic").is_err(),
        "the server reports the bad path too"
    );
}

#[tokio::test]
async fn no_server_times_out() {
    // Bind and drop a socket to find a port nothing listens on.
    let port = std::net::UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .expect("an ephemeral port should be available")
        .port();
    let url = format!("https://127.0.0.1:{port}/footfall/session");
    let error = SessionClient::new(url, CertificateHash::from_bytes([0; 32]))
        .with_timeout(Duration::from_millis(300))
        .discover()
        .await
        .expect_err("nothing is listening");
    assert!(
        matches!(
            error,
            ClientError::TimedOut { .. } | ClientError::Unreachable { .. }
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn an_unknown_request_gets_an_error_frame() {
    let (server, url, hash) = bind();
    let accept = tokio::spawn(async move { server.accept_request().await });

    let config = ClientConfig::builder()
        .with_bind_default()
        .with_server_certificate_hashes([wtransport::tls::Sha256Digest::new(*hash.as_bytes())])
        .build();
    let connection = Endpoint::client(config)
        .expect("client endpoint should build")
        .connect(&url)
        .await
        .expect("connection should open");
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .expect("stream should open")
        .await
        .expect("stream should be accepted");
    send.write_all(b"FFALLX99")
        .await
        .expect("request should send");
    send.finish().await.expect("request should finish");

    let mut header = [0_u8; 2];
    recv.read_exact(&mut header)
        .await
        .expect("the server should answer");
    assert_eq!(
        header,
        [2, 2],
        "an error response with the unsupported code"
    );
    assert!(accept.await.expect("accept task should not panic").is_err());
}

#[tokio::test]
async fn a_server_on_an_incompatible_version_is_named() {
    // A minimal server that answers discovery as protocol 9.0.
    let identity = Identity::self_signed(["localhost", "127.0.0.1"]).expect("identity");
    let hash =
        CertificateHash::from_bytes(*identity.certificate_chain().as_slice()[0].hash().as_ref());
    let endpoint = Endpoint::server(
        ServerConfig::builder()
            .with_bind_address(SocketAddr::from(([127, 0, 0, 1], 0)))
            .with_identity(identity)
            .build(),
    )
    .expect("server should bind");
    let port = endpoint.local_addr().expect("bound address").port();
    let server = tokio::spawn(async move {
        let request = endpoint.accept().await.await.expect("session request");
        let connection = request.accept().await.expect("session");
        let (mut send, mut recv) = connection.accept_bi().await.expect("stream");
        let mut magic = [0_u8; 8];
        recv.read_exact(&mut magic).await.expect("request");
        // Discovery response tag, then version 9.0.
        send.write_all(&[0, 9, 0, 0, 0]).await.expect("response");
        send.finish().await.expect("finish");
        // Keep the connection open until the client has read the reply.
        let _ = connection.closed().await;
    });

    let url = format!("https://127.0.0.1:{port}/footfall/session");
    let error = SessionClient::new(url, hash)
        .discover()
        .await
        .expect_err("the versions are incompatible");
    match error {
        ClientError::Incompatible { server, client } => {
            assert_eq!((server.major(), server.minor()), (9, 0));
            assert_eq!((client.major(), client.minor()), (1, 0));
        }
        other => panic!("expected Incompatible, got {other:?}"),
    }
    server.abort();
}

#[test]
fn certificate_hashes_parse_from_their_display_form() {
    let hash = CertificateHash::from_bytes([0xab; 32]);
    let text = hash.to_string();
    assert_eq!(text.parse::<CertificateHash>(), Ok(hash));
    assert_eq!(
        text.to_uppercase().parse::<CertificateHash>(),
        Ok(hash),
        "hex digits parse in either case"
    );
    assert!("abc".parse::<CertificateHash>().is_err());
    assert!("zz".repeat(32).parse::<CertificateHash>().is_err());
}

#[test]
fn server_error_codes_name_what_went_wrong() {
    let error = ClientError::Server {
        code: SessionErrorCode::NotFound,
        message: String::from("unknown session `x`"),
    };
    assert_eq!(error.server_code(), Some(SessionErrorCode::NotFound));
    assert_eq!(
        error.to_string(),
        "server error (not_found): unknown session `x`"
    );
}
