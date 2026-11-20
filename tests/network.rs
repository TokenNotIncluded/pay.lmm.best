use axum::{Router, http::StatusCode, routing::post};
use pay_lmm::network::SafeClient;
#[tokio::test]
async fn response_size_is_bounded_and_redirects_are_not_followed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new()
        .route("/large", post(|| async { vec![b'x'; 4097] }))
        .route(
            "/redirect",
            post(|| async {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", "http://169.254.169.254/latest/meta-data/")],
                    "",
                )
            }),
        );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = SafeClient::new(true, 4096).unwrap();
    assert!(
        client
            .execute(client.post(&format!("{origin}/large")).unwrap())
            .await
            .is_err()
    );
    let (status, _) = client
        .execute(client.post(&format!("{origin}/redirect")).unwrap())
        .await
        .unwrap();
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert!(
        SafeClient::new(false, 4096)
            .unwrap()
            .post(&format!("{origin}/large"))
            .is_err()
    );
    server.abort();
}
