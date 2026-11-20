//! Tests for Google Drive folder expansion and parsing.

use dman_daemon::gdrive::{
    GdriveClient, GdriveError, parse_confirm_page, parse_folder_html, parse_folder_id,
    sanitize_name,
};
use std::path::Path;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gdrive");

fn read_fixture(name: &str) -> String {
    let p = Path::new(FIXTURES_DIR).join(name);
    std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", p.display()))
}

#[test]
fn url_parsing_accepts_folders_and_rejects_files() {
    let valid = [
        (
            "https://drive.google.com/drive/folders/1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
            "1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
        ),
        (
            "https://drive.google.com/drive/folders/1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2/",
            "1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
        ),
        (
            "https://drive.google.com/drive/folders/1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2?usp=sharing",
            "1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
        ),
        (
            "https://drive.google.com/drive/u/0/folders/1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
            "1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
        ),
        (
            "https://drive.google.com/drive/u/2/folders/1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2?usp=drive_link",
            "1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
        ),
        (
            "http://docs.google.com/drive/folders/ABC123xyz_-folderId",
            "ABC123xyz_-folderId",
        ),
    ];
    for (url, expected_id) in valid {
        assert_eq!(
            parse_folder_id(url),
            Some(expected_id.to_string()),
            "failed for {url}"
        );
    }

    let invalid = [
        "https://drive.google.com/file/d/1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2/view",
        "https://drive.google.com/file/u/0/d/1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
        "https://example.com/drive/folders/1ZXEhzbLRLU1giKKRJkjm8N04cO_JoYE2",
        "magnet:?xt=urn:btih:abc",
        "https://drive.google.com/",
        "https://drive.google.com/drive/my-drive",
    ];
    for url in invalid {
        assert_eq!(parse_folder_id(url), None, "expected None for {url}");
    }
}

#[test]
fn fixture_flat_folder_parser() {
    let html = read_fixture("flat.html");
    let listing = parse_folder_html(&html).expect("failed to parse flat fixture");

    assert_eq!(listing.folder_name, Some("Sample Flat Folder".to_string()));
    assert_eq!(listing.subfolders.len(), 0);
    assert_eq!(listing.files.len(), 3);

    assert_eq!(listing.files[0].id, "file_id_1");
    assert_eq!(listing.files[0].name, "document.pdf");
    assert_eq!(
        listing.files[0].mime_type.as_deref(),
        Some("application/pdf")
    );

    assert_eq!(listing.files[1].id, "file_id_2");
    assert_eq!(listing.files[1].name, "photo.jpg");

    assert_eq!(listing.files[2].id, "file_id_3");
    assert_eq!(listing.files[2].name, "archive.tar.gz");
}

#[test]
fn fixture_nested_folder_parser() {
    let root_html = read_fixture("nested.html");
    let root_listing = parse_folder_html(&root_html).expect("failed to parse root nested fixture");

    assert_eq!(root_listing.folder_name, Some("Nested Project".to_string()));
    assert_eq!(root_listing.files.len(), 1);
    assert_eq!(root_listing.files[0].id, "file_root");
    assert_eq!(root_listing.files[0].name, "README.md");

    assert_eq!(root_listing.subfolders.len(), 1);
    assert_eq!(root_listing.subfolders[0].id, "sub_id_1");
    assert_eq!(root_listing.subfolders[0].name, "Documents");

    let sub_html = read_fixture("nested_sub.html");
    let sub_listing = parse_folder_html(&sub_html).expect("failed to parse subfolder fixture");

    assert_eq!(sub_listing.folder_name, Some("Documents".to_string()));
    assert_eq!(sub_listing.files.len(), 1);
    assert_eq!(sub_listing.files[0].id, "doc_id_1");
    assert_eq!(sub_listing.files[0].name, "report.docx");
}

#[test]
fn fixture_quota_page_returns_typed_error() {
    let html = read_fixture("quota.html");
    let err = parse_folder_html(&html).expect_err("quota page must return error");
    match err {
        GdriveError::QuotaExceeded { message } => {
            assert!(
                message.contains("Too many users have viewed or downloaded this file recently"),
                "unexpected quota message: {message}"
            );
        }
        other => panic!("expected QuotaExceeded, got {other:?}"),
    }
}

#[test]
fn fixture_missing_blob_returns_typed_error() {
    let html = read_fixture("missing_blob.html");
    let err = parse_folder_html(&html).expect_err("missing blob page must return error");
    assert_eq!(err, GdriveError::MissingBlob);
}

#[test]
fn fixture_name_needing_sanitizing() {
    let html = read_fixture("sanitize.html");
    let listing = parse_folder_html(&html).expect("failed to parse sanitize fixture");

    assert_eq!(listing.files.len(), 7);

    let sanitized: Vec<String> = listing
        .files
        .iter()
        .map(|f| sanitize_name(&f.name))
        .collect();

    assert_eq!(sanitized[0], "_________etc_passwd");
    assert_eq!(sanitized[1], "path_traversal_file.txt");
    assert_eq!(sanitized[2], "_nul.txt");
    assert_eq!(sanitized[3], "_con");
    assert_eq!(sanitized[4], "badchar.bin");
    assert_eq!(sanitized[5], "trailing");
    assert_eq!(sanitized[6], "_______test.txt");

    for name in &sanitized {
        assert!(
            !name.contains('/'),
            "must not contain forward slash: {name}"
        );
        assert!(!name.contains('\\'), "must not contain backslash: {name}");
        assert!(!name.contains('\0'), "must not contain null byte: {name}");
        assert!(!name.starts_with(".."), "must not start with ..: {name}");
        assert!(!name.ends_with('.'), "must not end with dot: {name}");
    }
}

#[test]
fn fixture_confirm_page_parser() {
    let html = read_fixture("confirm.html");
    let url = parse_confirm_page(&html).expect("failed to parse confirmation page");
    assert!(
        url.starts_with("https://drive.usercontent.google.com/download"),
        "unexpected url {url}"
    );
    assert!(url.contains("confirm=t"), "missing confirm=t in {url}");
    assert!(url.contains("id=large_file_id"), "missing id in {url}");
    assert!(
        url.contains("uuid=3174745f-f80a-4d70-8ffa-cc957ada2737"),
        "missing uuid in {url}"
    );
}

#[test]
fn fixture_private_folder_returns_auth_unsupported() {
    let html = read_fixture("private.html");
    let err = parse_folder_html(&html).expect_err("private folder must return error");
    assert_eq!(err, GdriveError::AuthUnsupported);
}

#[tokio::test]
async fn recursive_expansion_with_wiremock() {
    let server = wiremock::MockServer::start().await;

    // Root folder page
    Mock::given(method("GET"))
        .and(path("/drive/folders/root_123"))
        .respond_with(ResponseTemplate::new(200).set_body_string(read_fixture("nested.html")))
        .mount(&server)
        .await;

    // Subfolder page
    Mock::given(method("GET"))
        .and(path("/drive/folders/sub_id_1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(read_fixture("nested_sub.html")))
        .mount(&server)
        .await;

    // Root file: direct download redirects with 303 to usercontent
    Mock::given(method("GET"))
        .and(path("/uc"))
        .and(query_param("id", "file_root"))
        .respond_with(ResponseTemplate::new(303).insert_header(
            "Location",
            "https://drive.usercontent.google.com/download?id=file_root&export=download",
        ))
        .mount(&server)
        .await;

    // Subfolder file: requires confirmation, returns confirm.html
    Mock::given(method("GET"))
        .and(path("/uc"))
        .and(query_param("id", "doc_id_1"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            read_fixture("confirm.html").into_bytes(),
            "text/html; charset=utf-8",
        ))
        .mount(&server)
        .await;

    let client = GdriveClient::new().with_base_url(server.uri());
    let files = client
        .expand_folder("root_123")
        .await
        .expect("expand_folder failed");

    assert_eq!(files.len(), 2);

    // First file: README.md in "Nested Project" with resolved redirect Location
    assert_eq!(files[0].file_name, "README.md");
    assert_eq!(files[0].rel_dir, Path::new("Nested Project"));
    assert_eq!(
        files[0].download_url,
        "https://drive.usercontent.google.com/download?id=file_root&export=download"
    );

    // Second file: report.docx in "Nested Project/Documents"
    assert_eq!(files[1].file_name, "report.docx");
    assert_eq!(
        files[1].rel_dir,
        Path::new("Nested Project").join("Documents")
    );
    assert!(
        files[1].download_url.contains("confirm=t"),
        "unexpected url: {}",
        files[1].download_url
    );
}

#[tokio::test]
async fn files_keep_the_folder_order_when_resolved_concurrently() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/folders/folder_123"))
        .respond_with(ResponseTemplate::new(200).set_body_string(read_fixture("flat.html")))
        .mount(&server)
        .await;
    // The first file answers last.
    for (id, delay) in [("file_id_1", 300), ("file_id_2", 100), ("file_id_3", 0)] {
        Mock::given(method("GET"))
            .and(path("/uc"))
            .and(query_param("id", id))
            .respond_with(
                ResponseTemplate::new(303)
                    .insert_header("Location", format!("https://example.com/{id}").as_str())
                    .set_delay(std::time::Duration::from_millis(delay)),
            )
            .mount(&server)
            .await;
    }
    let files = GdriveClient::new()
        .with_base_url(server.uri())
        .expand_folder("folder_123")
        .await
        .expect("expand_folder failed");
    let names: Vec<&str> = files.iter().map(|f| f.file_name.as_str()).collect();
    assert_eq!(names, ["document.pdf", "photo.jpg", "archive.tar.gz"]);
}

#[tokio::test]
async fn missing_folder_reports_the_http_status_not_a_private_folder() {
    let server = wiremock::MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/drive/folders/gone"))
        .respond_with(ResponseTemplate::new(404).set_body_string("<html>Not Found</html>"))
        .mount(&server)
        .await;
    let err = GdriveClient::new()
        .with_base_url(server.uri())
        .expand_folder("gone")
        .await
        .expect_err("a 404 is an error");
    assert!(
        matches!(&err, GdriveError::Network(m) if m.contains("404")),
        "{err:?}"
    );
}

#[tokio::test]
async fn recursive_cycle_guard_terminates() {
    let server = wiremock::MockServer::start().await;

    // Root folder contains subfolder "sub_1"
    let root_body = r#"<!DOCTYPE html><html><head><title>Cycle Root - Google Drive</title></head><body>
    <script>window['_DRIVE_ivd'] = '[[["sub_1",["root"],"Sub1","application/vnd.google-apps.folder"]]]';</script>
    </body></html>"#;

    // sub_1 links back to "root"
    let sub_body = r#"<!DOCTYPE html><html><head><title>Cycle Sub - Google Drive</title></head><body>
    <script>window['_DRIVE_ivd'] = '[[["root",["sub_1"],"Root","application/vnd.google-apps.folder"],["leaf",["sub_1"],"leaf.txt","text/plain"]]]';</script>
    </body></html>"#;

    Mock::given(method("GET"))
        .and(path("/drive/folders/root"))
        .respond_with(ResponseTemplate::new(200).set_body_string(root_body))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/drive/folders/sub_1"))
        .respond_with(ResponseTemplate::new(200).set_body_string(sub_body))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/uc"))
        .and(query_param("id", "leaf"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Disposition", "attachment; filename=\"leaf.txt\"")
                .insert_header("Content-Type", "text/plain"),
        )
        .mount(&server)
        .await;

    let client = GdriveClient::new().with_base_url(server.uri());
    let files = client
        .expand_folder("root")
        .await
        .expect("expand_folder failed");

    // Must terminate cleanly without stack overflow, finding only "leaf.txt"
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name, "leaf.txt");
    assert_eq!(files[0].rel_dir, Path::new("Cycle Root").join("Sub1"));
}

#[tokio::test]
#[ignore]
async fn live_public_folder_expansion() {
    if std::env::var("DMAN_TEST_LIVE_GDRIVE").is_err() {
        eprintln!("skipping live test; set DMAN_TEST_LIVE_GDRIVE=1 to run");
        return;
    }
    let folder_url = "https://drive.google.com/drive/folders/0Bx8RePQCi0zsdTJHekFzMVB5Zzg?resourcekey=0-6JmqCQiqGowKBuLOMg1reQ";
    let folder_id = parse_folder_id(folder_url).expect("failed to parse folder id");
    assert_eq!(folder_id, "0Bx8RePQCi0zsdTJHekFzMVB5Zzg");

    let client = GdriveClient::new();
    let files = client
        .expand_folder(&folder_id)
        .await
        .expect("expand_folder failed");

    println!("Found {} files:", files.len());
    for f in &files {
        println!(" - {} in {:?}", f.file_name, f.rel_dir);
    }
    assert!(!files.is_empty());
}
