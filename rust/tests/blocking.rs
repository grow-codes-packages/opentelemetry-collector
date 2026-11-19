// Taken over from sdk-kit 0.1.0 (header removed): the mock serves the collector routes
// (health_check `/`, OTLP `/v1/traces`, zpages `/debug/servicez`) instead of the demo routes.
//! Blocking client (feature `blocking`).
#![cfg(feature = "blocking")]

use std::io::{Read, Write};
use std::net::TcpListener;

use grow_opentelemetry_collector as sdk;
use sdk::core::ErrorCode;

use sdk::blocking::OtelCollectorClient as Client;

use sdk::OtelCollectorConfig as Config;

/// Minimal HTTP server on a thread (no async runtime involved).
fn serve(responses: Vec<&'static str>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for body in responses {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let (status, rest) = body.split_once(' ').unwrap();
            let (ctype, text) = rest.split_once(' ').unwrap();
            let resp = format!(
                "HTTP/1.1 {status} X\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                text.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
        }
    });
    format!("http://{addr}")
}

#[test]
fn blocking_client() {
    let url = serve(vec![
        r#"200 application/json {"status":"Server available"}"#,
        r#"200 application/json {"status":"Server available"}"#,
        "200 application/x-protobuf  ",
        r#"200 text/html <td><b>Version</b></td><td>-</td><td>v2.0.0</td>"#,
    ]);
    let cfg = Config::builder()
        .url(url.clone())
        .health_url(format!("{url}/"))
        .zpages_url(url)
        .header("Authorization", "Bearer t-1234")
        .build()
        .unwrap();
    let c = Client::new(cfg).unwrap();
    assert!(c.health().unwrap().healthy);
    assert!(
        c.wait_until_ready(std::time::Duration::from_secs(5))
            .unwrap()
            .healthy
    );
    c.ping().unwrap();
    assert_eq!(c.server_version().unwrap(), "2.0.0");
    assert!(!c.async_client().is_closed());
    assert_eq!(c.block_on(async { 7 }), 7);
    c.close();
    c.close();
    assert_eq!(c.ping().unwrap_err().code(), ErrorCode::Cancelled);
    let _ = Client::from_env();
    let bad = Config::builder()
        .ca_cert_path("/nonexistent")
        .build()
        .unwrap();
    assert_eq!(Client::new(bad).unwrap_err().code(), ErrorCode::Config);
}
