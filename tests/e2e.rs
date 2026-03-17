use httpmock::prelude::*;
use serde_json::json;
use std::future::Future;
use std::time::Duration;
use tempfile::tempdir;
use tokio::net::TcpListener;

async fn wait_until<F, Fut>(max_retries: usize, interval: Duration, mut f: F) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    for _ in 0..max_retries {
        if f().await {
            return true;
        }
        tokio::time::sleep(interval).await;
    }
    false
}

#[tokio::test]
async fn test_e2e() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    // 1. Setup Mock TMDB
    let tmdb_mock = MockServer::start();

    // Mock for Big Buck Bunny search
    tmdb_mock.mock(|when, then| {
        when.method(GET)
            .path("/search/movie")
            .query_param("query", "Big Buck Bunny");
        then.status(200).json_body(json!({
            "results": [{ "id": 1234 }]
        }));
    });

    // Mock for Big Buck Bunny details
    tmdb_mock.mock(|when, then| {
        when.method(GET).path("/movie/1234");
        then.status(200).json_body(json!({
            "imdb_id": "tt1254201",
            "poster_path": "/poster.jpg"
        }));
    });

    // Mock for Big Buck Bunny Renamed search
    tmdb_mock.mock(|when, then| {
        when.method(GET)
            .path("/search/movie")
            .query_param("query", "Big Buck Bunny Renamed");
        then.status(200).json_body(json!({
            "results": [{ "id": 1234 }]
        }));
    });

    // Mock for Sintel search
    tmdb_mock.mock(|when, then| {
        when.method(GET)
            .path("/search/movie")
            .query_param("query", "Sintel");
        then.status(200).json_body(json!({
            "results": [{ "id": 5678 }]
        }));
    });

    // Mock for Sintel details
    tmdb_mock.mock(|when, then| {
        when.method(GET).path("/movie/5678");
        then.status(200).json_body(json!({
            "imdb_id": "tt1727596",
            "poster_path": "/sintel.jpg"
        }));
    });

    // 2. Prepare Media Directory
    let temp_media = tempdir().unwrap();
    let initial_video = temp_media.path().join("Big.Buck.Bunny.2008.mp4");
    let initial_content = "fake video data for Big Buck Bunny";
    std::fs::write(&initial_video, initial_content).unwrap();

    // 3. Start the Application
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);

    // 4. Setup Environment Variables
    std::env::set_var("MEDIA_PATH", temp_media.path());
    std::env::set_var("TMDB_BASE_URL", tmdb_mock.base_url());
    std::env::set_var("TMDB_API_KEY", "fake_key");
    std::env::set_var("PORT", addr.port().to_string());
    std::env::set_var("BASE_URL", &base_url);

    tokio::spawn(async move {
        lanio::run(Some(listener)).await.expect("App failed to run");
    });

    let client = reqwest::Client::new();

    // Wait for App to be Ready
    let ready = wait_until(10, Duration::from_millis(100), || async {
        let resp = client.get(format!("{}/health", base_url)).send().await;
        matches!(resp, Ok(r) if r.status().is_success())
    })
    .await;
    assert!(ready, "App never became ready");

    // 5. Initial Verification (Big Buck Bunny should be there)
    let movie_found = wait_until(20, Duration::from_millis(500), || async {
        let resp = client
            .get(format!("{}/catalog/movie/lanio-movies", base_url))
            .send()
            .await;
        if let Ok(r) = resp {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                return json["metas"]
                    .as_array()
                    .map(|metas| metas.iter().any(|m| m["name"] == "Big Buck Bunny"))
                    .unwrap_or(false);
            }
        }
        false
    })
    .await;
    assert!(movie_found, "Initial movie not found in catalog");

    // 6. Test VIDEO STREAMING path
    // Get the stream URL for Big Buck Bunny (tt1254201)
    let resp = client
        .get(format!("{}/stream/movie/tt1254201", base_url))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let stream_resp: serde_json::Value = resp.json().await.unwrap();
    let streams = stream_resp["streams"]
        .as_array()
        .expect("Streams should be an array");
    assert!(!streams.is_empty(), "No streams found for movie");

    let stream_url = streams[0]["url"]
        .as_str()
        .expect("Stream URL should be a string");

    // Request the actual video content
    let video_resp = client.get(stream_url).send().await.unwrap();
    assert!(
        video_resp.status().is_success(),
        "Video request failed with status: {} for URL: {}",
        video_resp.status(),
        stream_url
    );
    let video_content = video_resp.text().await.unwrap();
    assert_eq!(video_content, initial_content, "Video content mismatch");

    // 7. Test ADDING new media
    let second_video = temp_media.path().join("Sintel.2010.mp4");
    std::fs::write(&second_video, "more fake video").unwrap();

    let sintel_found = wait_until(20, Duration::from_millis(500), || async {
        let resp = client
            .get(format!("{}/catalog/movie/lanio-movies", base_url))
            .send()
            .await;
        if let Ok(r) = resp {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                return json["metas"]
                    .as_array()
                    .map(|metas| metas.iter().any(|m| m["name"] == "Sintel"))
                    .unwrap_or(false);
            }
        }
        false
    })
    .await;
    assert!(
        sintel_found,
        "Added movie (Sintel) never appeared in catalog"
    );

    // 8. Test MOVING/RENAMING media
    let renamed_video = temp_media.path().join("Big.Buck.Bunny.Renamed.mp4");
    std::fs::rename(&initial_video, &renamed_video).unwrap();

    // Verify stream still works after rename (with retries for re-indexing)
    let stream_accessible = wait_until(20, Duration::from_millis(500), || async {
        let resp = client
            .get(format!("{}/stream/movie/tt1254201", base_url))
            .send()
            .await;
        if let Ok(r) = resp {
            if let Ok(stream_resp) = r.json::<serde_json::Value>().await {
                if let Some(streams) = stream_resp["streams"].as_array() {
                    if !streams.is_empty() {
                        if let Some(stream_url) = streams[0]["url"].as_str() {
                            let video_resp = client.get(stream_url).send().await;
                            return matches!(video_resp, Ok(vr) if vr.status().is_success());
                        }
                    }
                }
            }
        }
        false
    })
    .await;
    assert!(
        stream_accessible,
        "Video stream not accessible after rename"
    );

    // 9. Test REMOVING media
    std::fs::remove_file(&second_video).unwrap();

    let sintel_gone = wait_until(20, Duration::from_millis(500), || async {
        let resp = client
            .get(format!("{}/catalog/movie/lanio-movies", base_url))
            .send()
            .await;
        if let Ok(r) = resp {
            if let Ok(json) = r.json::<serde_json::Value>().await {
                return json["metas"]
                    .as_array()
                    .map(|metas| !metas.iter().any(|m| m["name"] == "Sintel"))
                    .unwrap_or(true);
            }
        }
        true
    })
    .await;
    assert!(
        sintel_gone,
        "Removed movie (Sintel) still present in catalog"
    );
}
