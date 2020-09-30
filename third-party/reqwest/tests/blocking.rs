mod support;
use support::*;

#[test]
fn test_response_text() {
    let server = server::http(move |_req| async { http::Response::new("Hello".into()) });

    let url = format!("http://{}/text", server.addr());
    let res = reqwest::blocking::get(&url).unwrap();
    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    assert_eq!(res.content_length(), Some(5));

    let body = res.text().unwrap();
    assert_eq!(b"Hello", body.as_bytes());
}

#[test]
fn test_response_non_utf_8_text() {
    let server = server::http(move |_req| async {
        http::Response::builder()
            .header("content-type", "text/plain; charset=gbk")
            .body(b"\xc4\xe3\xba\xc3"[..].into())
            .unwrap()
    });

    let url = format!("http://{}/text", server.addr());
    let res = reqwest::blocking::get(&url).unwrap();
    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    assert_eq!(res.content_length(), Some(4));

    let body = res.text().unwrap();
    assert_eq!("你好", &body);
    assert_eq!(b"\xe4\xbd\xa0\xe5\xa5\xbd", body.as_bytes()); // Now it's utf-8
}

#[test]
#[cfg(feature = "json")]
fn test_response_json() {
    let server = server::http(move |_req| async { http::Response::new("\"Hello\"".into()) });

    let url = format!("http://{}/json", server.addr());
    let res = reqwest::blocking::get(&url).unwrap();
    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    assert_eq!(res.content_length(), Some(7));

    let body = res.json::<String>().unwrap();
    assert_eq!("Hello", body);
}

#[test]
fn test_response_copy_to() {
    let server = server::http(move |_req| async { http::Response::new("Hello".into()) });

    let url = format!("http://{}/1", server.addr());
    let mut res = reqwest::blocking::get(&url).unwrap();
    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);

    let mut dst = Vec::new();
    res.copy_to(&mut dst).unwrap();
    assert_eq!(dst, b"Hello");
}

#[test]
fn test_get() {
    let server = server::http(move |_req| async { http::Response::default() });

    let url = format!("http://{}/1", server.addr());
    let res = reqwest::blocking::get(&url).unwrap();

    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
    assert_eq!(res.remote_addr(), Some(server.addr()));

    assert_eq!(res.text().unwrap().len(), 0)
}

#[test]
fn test_post() {
    let server = server::http(move |req| async move {
        assert_eq!(req.method(), "POST");
        assert_eq!(req.headers()["content-length"], "5");

        let data = hyper::body::to_bytes(req.into_body()).await.unwrap();
        assert_eq!(&*data, b"Hello");

        http::Response::default()
    });

    let url = format!("http://{}/2", server.addr());
    let res = reqwest::blocking::Client::new()
        .post(&url)
        .body("Hello")
        .send()
        .unwrap();

    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

#[test]
fn test_post_form() {
    let server = server::http(move |req| async move {
        assert_eq!(req.method(), "POST");
        assert_eq!(req.headers()["content-length"], "24");
        assert_eq!(
            req.headers()["content-type"],
            "application/x-www-form-urlencoded"
        );

        let data = hyper::body::to_bytes(req.into_body()).await.unwrap();
        assert_eq!(&*data, b"hello=world&sean=monstar");

        http::Response::default()
    });

    let form = &[("hello", "world"), ("sean", "monstar")];

    let url = format!("http://{}/form", server.addr());
    let res = reqwest::blocking::Client::new()
        .post(&url)
        .form(form)
        .send()
        .expect("request send");

    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

/// Calling `Response::error_for_status`` on a response with status in 4xx
/// returns a error.
#[test]
fn test_error_for_status_4xx() {
    let server = server::http(move |_req| async {
        http::Response::builder()
            .status(400)
            .body(Default::default())
            .unwrap()
    });

    let url = format!("http://{}/1", server.addr());
    let res = reqwest::blocking::get(&url).unwrap();

    let err = res.error_for_status().unwrap_err();
    assert!(err.is_status());
    assert_eq!(err.status(), Some(reqwest::StatusCode::BAD_REQUEST));
}

/// Calling `Response::error_for_status`` on a response with status in 5xx
/// returns a error.
#[test]
fn test_error_for_status_5xx() {
    let server = server::http(move |_req| async {
        http::Response::builder()
            .status(500)
            .body(Default::default())
            .unwrap()
    });

    let url = format!("http://{}/1", server.addr());
    let res = reqwest::blocking::get(&url).unwrap();

    let err = res.error_for_status().unwrap_err();
    assert!(err.is_status());
    assert_eq!(
        err.status(),
        Some(reqwest::StatusCode::INTERNAL_SERVER_ERROR)
    );
}

#[test]
fn test_default_headers() {
    let server = server::http(move |req| async move {
        assert_eq!(req.headers()["reqwest-test"], "orly");
        http::Response::default()
    });

    let mut headers = http::HeaderMap::with_capacity(1);
    headers.insert("reqwest-test", "orly".parse().unwrap());
    let client = reqwest::blocking::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

    let url = format!("http://{}/1", server.addr());
    let res = client.get(&url).send().unwrap();

    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

#[test]
fn test_override_default_headers() {
    let server = server::http(move |req| {
        async move {
            // not 'iamatoken'
            assert_eq!(req.headers()[&http::header::AUTHORIZATION], "secret");
            http::Response::default()
        }
    });

    let mut headers = http::HeaderMap::with_capacity(1);
    headers.insert(
        http::header::AUTHORIZATION,
        http::header::HeaderValue::from_static("iamatoken"),
    );
    let client = reqwest::blocking::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

    let url = format!("http://{}/3", server.addr());
    let res = client
        .get(&url)
        .header(
            http::header::AUTHORIZATION,
            http::header::HeaderValue::from_static("secret"),
        )
        .send()
        .unwrap();

    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

#[test]
fn test_appended_headers_not_overwritten() {
    let server = server::http(move |req| async move {
        let mut accepts = req.headers().get_all("accept").into_iter();
        assert_eq!(accepts.next().unwrap(), "application/json");
        assert_eq!(accepts.next().unwrap(), "application/json+hal");
        assert_eq!(accepts.next(), None);

        http::Response::default()
    });

    let client = reqwest::blocking::Client::new();

    let url = format!("http://{}/4", server.addr());
    let res = client
        .get(&url)
        .header(header::ACCEPT, "application/json")
        .header(header::ACCEPT, "application/json+hal")
        .send()
        .unwrap();

    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);

    // make sure this also works with default headers
    use reqwest::header;
    let mut headers = header::HeaderMap::with_capacity(1);
    headers.insert(
        header::ACCEPT,
        header::HeaderValue::from_static("text/html"),
    );
    let client = reqwest::blocking::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap();

    let url = format!("http://{}/4", server.addr());
    let res = client
        .get(&url)
        .header(header::ACCEPT, "application/json")
        .header(header::ACCEPT, "application/json+hal")
        .send()
        .unwrap();

    assert_eq!(res.url().as_str(), &url);
    assert_eq!(res.status(), reqwest::StatusCode::OK);
}

#[cfg_attr(not(debug_assertions), ignore)]
#[test]
#[should_panic]
fn test_blocking_inside_a_runtime() {
    let server = server::http(move |_req| async { http::Response::new("Hello".into()) });

    let url = format!("http://{}/text", server.addr());

    let mut rt = tokio::runtime::Builder::new().build().expect("new rt");

    rt.block_on(async move {
        let _should_panic = reqwest::blocking::get(&url);
    });
}