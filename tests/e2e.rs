use std::{net::SocketAddr, time::Duration};

use rmcp::{
    model::{CallToolRequestParams, ClientInfo, GetPromptRequestParams},
    transport::TokioChildProcess,
    ServiceExt,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::process::Command;

#[derive(Clone, Copy)]
struct MockResponse {
    path: &'static str,
    status: u16,
    content_type: &'static str,
    body: &'static str,
}

async fn run_mock_http(listener: TcpListener, responses: Vec<MockResponse>) -> anyhow::Result<()> {
    loop {
        let (mut stream, _) = listener.accept().await?;
        let responses = responses.clone();
        tokio::spawn(async move {
            let mut request_buf = [0_u8; 1024];
            let read_len = stream.read(&mut request_buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&request_buf[..read_len]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .unwrap_or("/");
            let response = responses
                .iter()
                .find(|candidate| candidate.path == path)
                .copied()
                .unwrap_or(MockResponse {
                    path: "/",
                    status: 404,
                    content_type: "text/plain; charset=utf-8",
                    body: "not found",
                });
            let status_text = match response.status {
                200 => "OK",
                401 => "Unauthorized",
                403 => "Forbidden",
                404 => "Not Found",
                _ => "OK",
            };
            let headers = format!(
                "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                response.status,
                status_text,
                response.content_type,
                response.body.len()
            );
            let _ = stream.write_all(headers.as_bytes()).await;
            let _ = stream.write_all(response.body.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
    }
}

fn locate_test_binary() -> String {
    for key in ["CARGO_BIN_EXE_fetch-mcp-rs", "CARGO_BIN_EXE_fetch_mcp_rs"] {
        if let Ok(path) = std::env::var(key) {
            return path;
        }
    }

    let target_dir = std::env::var("CARGO_TARGET_DIR").unwrap_or_else(|_| "target".into());
    let profile = std::env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let mut path = std::path::PathBuf::from(target_dir);
    path.push(&profile);
    let exe = if cfg!(target_os = "windows") {
        "fetch-mcp-rs.exe"
    } else {
        "fetch-mcp-rs"
    };
    path.push(exe);
    path.to_string_lossy().to_string()
}

async fn start_service(
    args: &[&str],
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ClientInfo>> {
    let mut command = Command::new(locate_test_binary());
    command.args(args);
    let transport = TokioChildProcess::new(command)?;
    let client = ClientInfo::default();
    Ok(client.serve(transport).await?)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_fetch_tool_and_prompt() -> anyhow::Result<()> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    tokio::spawn(run_mock_http(
        listener,
        vec![
            MockResponse {
                path: "/",
                status: 200,
                content_type: "text/plain; charset=utf-8",
                body: "abcdefghijklmnopqrstuvwxyz",
            },
            MockResponse {
                path: "/robots.txt",
                status: 404,
                content_type: "text/plain; charset=utf-8",
                body: "not found",
            },
        ],
    ));

    let service = start_service(&[]).await?;

    tokio::time::sleep(Duration::from_millis(300)).await;

    let tools = service.list_all_tools().await?;
    let tool_names: Vec<String> = tools.iter().map(|tool| tool.name.to_string()).collect();
    assert!(
        tool_names.iter().any(|name| name == "fetch"),
        "fetch tool missing: {tool_names:?}"
    );

    let prompts = service.list_all_prompts().await?;
    let prompt_names: Vec<String> = prompts.iter().map(|prompt| prompt.name.clone()).collect();
    assert!(
        prompt_names.iter().any(|name| name == "fetch"),
        "fetch prompt missing: {prompt_names:?}"
    );

    let mut tool_args = serde_json::Map::new();
    tool_args.insert(
        "url".into(),
        serde_json::Value::String(format!("http://{local_addr}")),
    );
    tool_args.insert(
        "max_length".into(),
        serde_json::Value::Number(serde_json::Number::from(5u64)),
    );
    tool_args.insert(
        "start_index".into(),
        serde_json::Value::Number(serde_json::Number::from(2u64)),
    );

    let tool_result = service
        .call_tool(CallToolRequestParams::new("fetch").with_arguments(tool_args))
        .await?;
    let tool_text = format!("{tool_result:?}");
    assert!(tool_text.contains("Contents of http://"));
    assert!(tool_text.contains("cdefg"));
    assert!(tool_text.contains("start_index of 7"));

    let mut prompt_args = serde_json::Map::new();
    prompt_args.insert(
        "url".into(),
        serde_json::Value::String(format!("http://{local_addr}")),
    );
    let prompt_result = service
        .get_prompt(GetPromptRequestParams::new("fetch").with_arguments(prompt_args))
        .await?;
    let prompt_text = format!("{prompt_result:?}");
    assert!(prompt_text.contains("abcdefghijklmnopqrstuvwxyz"));

    let _ = service.cancel().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_obeys_robots_txt_unless_ignored() -> anyhow::Result<()> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    tokio::spawn(run_mock_http(
        listener,
        vec![
            MockResponse {
                path: "/",
                status: 200,
                content_type: "text/plain; charset=utf-8",
                body: "visible body",
            },
            MockResponse {
                path: "/robots.txt",
                status: 200,
                content_type: "text/plain; charset=utf-8",
                body: "User-agent: *\nDisallow: /",
            },
        ],
    ));

    let service = start_service(&[]).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut tool_args = serde_json::Map::new();
    tool_args.insert(
        "url".into(),
        serde_json::Value::String(format!("http://{local_addr}")),
    );

    let tool_error = service
        .call_tool(CallToolRequestParams::new("fetch").with_arguments(tool_args.clone()))
        .await
        .expect_err("tool call should be blocked by robots.txt");
    let tool_error_text = tool_error.to_string();
    assert!(tool_error_text.contains("robots.txt"));

    let prompt_result = service
        .get_prompt(GetPromptRequestParams::new("fetch").with_arguments(tool_args.clone()))
        .await?;
    let prompt_text = format!("{prompt_result:?}");
    assert!(prompt_text.contains("visible body"));
    let _ = service.cancel().await;

    let ignore_service = start_service(&["--ignore-robots-txt"]).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tool_result = ignore_service
        .call_tool(CallToolRequestParams::new("fetch").with_arguments(tool_args))
        .await?;
    let tool_text = format!("{tool_result:?}");
    assert!(tool_text.contains("visible body"));
    let _ = ignore_service.cancel().await;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e2e_prefers_main_content_when_simplifying_html() -> anyhow::Result<()> {
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let listener = TcpListener::bind(addr).await?;
    let local_addr = listener.local_addr()?;
    tokio::spawn(run_mock_http(
        listener,
        vec![
            MockResponse {
                path: "/",
                status: 200,
                content_type: "text/html; charset=utf-8",
                body: r#"<!doctype html>
<html>
  <head>
    <title>Example</title>
    <script>window.banner = "noise";</script>
  </head>
  <body>
    <header>
      <nav>
        <a href="/models">Models</a>
        <a href="/pricing">Pricing</a>
      </nav>
    </header>
    <main>
      <article>
        <h1>Useful Title</h1>
        <p>Primary body text.</p>
        <ul>
          <li>First point</li>
          <li>Second point</li>
        </ul>
      </article>
      <nav><a href="/toc">Table of contents</a></nav>
      <aside>Sidebar noise</aside>
    </main>
    <footer>Footer noise</footer>
  </body>
</html>"#,
            },
            MockResponse {
                path: "/robots.txt",
                status: 404,
                content_type: "text/plain; charset=utf-8",
                body: "not found",
            },
        ],
    ));

    let service = start_service(&[]).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut tool_args = serde_json::Map::new();
    tool_args.insert(
        "url".into(),
        serde_json::Value::String(format!("http://{local_addr}")),
    );

    let tool_result = service
        .call_tool(CallToolRequestParams::new("fetch").with_arguments(tool_args))
        .await?;
    let tool_text = format!("{tool_result:?}");
    assert!(tool_text.contains("Useful Title"));
    assert!(tool_text.contains("Primary body text."));
    assert!(tool_text.contains("First point"));
    assert!(!tool_text.contains("window.banner"));
    assert!(!tool_text.contains("Pricing"));
    assert!(!tool_text.contains("Sidebar noise"));
    assert!(!tool_text.contains("Footer noise"));

    let _ = service.cancel().await;
    Ok(())
}
