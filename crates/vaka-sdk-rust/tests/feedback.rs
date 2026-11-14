//! capture_feedback against a tiny fake server (own process: the SDK client is global).

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

struct Req {
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn fake_server(feedback_status: Arc<Mutex<u16>>) -> (String, Arc<Mutex<Vec<Req>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = format!("http://{}", listener.local_addr().unwrap());
    let reqs = Arc::new(Mutex::new(Vec::new()));
    let out = reqs.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut r = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() || line.is_empty() {
                continue;
            }
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let mut headers = Vec::new();
            loop {
                let mut h = String::new();
                r.read_line(&mut h).unwrap();
                let h = h.trim_end();
                if h.is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    headers.push((k.trim().to_string(), v.trim().to_string()));
                }
            }
            let len = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            let (status, resp) = if path == "/ingest/v1/feedback" {
                match *feedback_status.lock().unwrap() {
                    202 => (202, r#"{"id":"fb-1","short_id":"API-F1"}"#),
                    s => (s, r#"{"error":"too much feedback"}"#),
                }
            } else {
                (202, r#"{"accepted":0,"rejected":[]}"#)
            };
            reqs.lock().unwrap().push(Req { path, headers, body });
            let _ = write!(stream, "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}", resp.len());
        }
    });
    (addr, out)
}

#[test]
fn capture_feedback_end_to_end() {
    assert!(matches!(vaka_sdk::capture_feedback(vaka_sdk::Feedback { message: "x".into(), ..Default::default() }), Err(vaka_sdk::FeedbackError::NotInitialized)));

    let status = Arc::new(Mutex::new(202));
    let (endpoint, reqs) = fake_server(status.clone());
    let _guard = vaka_sdk::init(vaka_sdk::Config {
        key: "vk_test".into(),
        endpoint,
        release: Some("1.2.3".into()),
        environment: "test".into(),
        sessions: false,
        capture_panics: false,
        ..Default::default()
    });
    vaka_sdk::identify(Some("user-7"));
    vaka_sdk::set_tag("tenant", "acme");
    vaka_sdk::capture_message("export failed", vaka_sdk::Level::Error);
    let event_id = vaka_sdk::last_event_id().expect("event id");

    let r = vaka_sdk::capture_feedback(vaka_sdk::Feedback {
        message: "  The export button does nothing  ".into(),
        email: Some("ann@example.com".into()),
        event_id: Some(event_id.clone()),
        issue: Some("API-12".into()),
        session_id: Some("s-1".into()),
        ip: Some("203.0.113.9".into()),
        tags: [("plan".to_string(), "pro".to_string())].into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(r, vaka_sdk::FeedbackResult { id: "fb-1".into(), short_id: "API-F1".into() });
    {
        let reqs = reqs.lock().unwrap();
        let req = reqs.iter().find(|r| r.path == "/ingest/v1/feedback").unwrap();
        assert_eq!(req.header("x-vaka-key"), Some("vk_test"));
        assert_eq!(req.header("x-forwarded-for"), Some("203.0.113.9"));
        let item: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(item["message"], "The export button does nothing");
        assert_eq!(item["email"], "ann@example.com");
        assert_eq!(item["event_id"], event_id.as_str());
        assert_eq!(item["issue"], "API-12");
        assert_eq!(item["session_id"], "s-1");
        assert_eq!(item["user_id"], "user-7");
        assert_eq!(item["source"], "api");
        assert_eq!(item["platform"], "rust");
        assert_eq!(item["release"], "1.2.3");
        assert_eq!(item["tags"], serde_json::json!({"tenant": "acme", "plan": "pro"}));
    }

    let png = b"\x89PNG\r\n\x1a\n0000".to_vec();
    vaka_sdk::capture_feedback(vaka_sdk::Feedback {
        message: "with a screenshot".into(),
        attachments: vec![vaka_sdk::Attachment { filename: "shot.png".into(), content_type: None, data: png }],
        ..Default::default()
    })
    .unwrap();
    {
        let reqs = reqs.lock().unwrap();
        let req = reqs.iter().filter(|r| r.path == "/ingest/v1/feedback").nth(1).unwrap();
        assert!(req.header("content-type").unwrap().starts_with("multipart/form-data; boundary="));
        let text = String::from_utf8_lossy(&req.body);
        assert!(text.contains("\"message\":\"with a screenshot\""), "{text}");
        assert!(text.contains("filename=\"shot.png\"\r\nContent-Type: image/png"), "{text}");
    }

    assert!(matches!(vaka_sdk::capture_feedback(vaka_sdk::Feedback { message: "  ".into(), ..Default::default() }), Err(vaka_sdk::FeedbackError::Invalid(_))));
    let four = vec![vaka_sdk::Attachment::default(); 4];
    assert!(matches!(
        vaka_sdk::capture_feedback(vaka_sdk::Feedback { message: "x".into(), attachments: four, ..Default::default() }),
        Err(vaka_sdk::FeedbackError::Invalid(_))
    ));
    *status.lock().unwrap() = 429;
    match vaka_sdk::capture_feedback(vaka_sdk::Feedback { message: "again".into(), ..Default::default() }) {
        Err(vaka_sdk::FeedbackError::Status(429, m)) => assert_eq!(m, "too much feedback"),
        other => panic!("{other:?}"),
    }
}
