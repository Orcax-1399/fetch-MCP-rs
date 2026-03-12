use reqwest::Url;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{
        CallToolResult, Content, GetPromptRequestParams, GetPromptResult, ListPromptsResult,
        PaginatedRequestParams, Prompt, PromptArgument, PromptMessage, PromptMessageRole,
        ServerCapabilities, ServerInfo,
    },
    tool, tool_handler, tool_router,
    transport::stdio,
    ErrorData, ServiceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::env;
use std::io;
use tracing_subscriber::{fmt, EnvFilter};

const DEFAULT_MAX_LENGTH: usize = 5_000;
const MAX_ALLOWED_LENGTH: usize = 1_000_000;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const MAX_DOWNLOAD_BYTES: usize = 5_000_000;
const DEFAULT_USER_AGENT_AUTONOMOUS: &str =
    "ModelContextProtocol/1.0 (Autonomous; +https://github.com/modelcontextprotocol/servers)";
const DEFAULT_USER_AGENT_MANUAL: &str =
    "ModelContextProtocol/1.0 (User-Specified; +https://github.com/modelcontextprotocol/servers)";

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FetchRequest {
    #[schemars(description = "URL to fetch")]
    pub url: String,
    #[schemars(description = "Maximum number of characters to return (default: 5000)")]
    pub max_length: Option<usize>,
    #[schemars(description = "Start content from this character index (default: 0)")]
    pub start_index: Option<usize>,
    #[schemars(description = "Get raw content without markdown conversion (default: false)")]
    pub raw: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PromptFetchRequest {
    url: String,
}

#[derive(Debug)]
struct FetchOptions {
    url: String,
    max_length: usize,
    start_index: usize,
    raw: bool,
}

#[derive(Debug)]
struct FetchedPage {
    content: String,
    prefix: String,
}

#[derive(Debug, Clone, Default)]
struct ServerConfig {
    custom_user_agent: Option<String>,
    ignore_robots_txt: bool,
    proxy_url: Option<String>,
}

impl ServerConfig {
    fn autonomous_user_agent(&self) -> &str {
        self.custom_user_agent
            .as_deref()
            .unwrap_or(DEFAULT_USER_AGENT_AUTONOMOUS)
    }

    fn manual_user_agent(&self) -> &str {
        self.custom_user_agent
            .as_deref()
            .unwrap_or(DEFAULT_USER_AGENT_MANUAL)
    }
}

#[derive(Clone)]
pub struct FetchServer {
    tool_router: ToolRouter<Self>,
    config: ServerConfig,
}

#[tool_router]
impl FetchServer {
    fn new(config: ServerConfig) -> Self {
        Self {
            tool_router: Self::tool_router(),
            config,
        }
    }

    #[tool(
        name = "fetch",
        description = "Fetches a URL from the internet and optionally extracts its contents as markdown."
    )]
    async fn fetch(&self, params: Parameters<FetchRequest>) -> Result<CallToolResult, ErrorData> {
        let options = parse_fetch_request(params.0)?;
        if !self.config.ignore_robots_txt {
            check_may_autonomously_fetch_url(
                &options.url,
                self.config.autonomous_user_agent(),
                self.config.proxy_url.as_deref(),
            )
            .await?;
        }
        let page = fetch_url(
            &options.url,
            self.config.autonomous_user_agent(),
            options.raw,
            self.config.proxy_url.as_deref(),
        )
        .await?;
        let content = paginate_content(
            &options.url,
            &page.content,
            &page.prefix,
            options.start_index,
            options.max_length,
        );
        Ok(CallToolResult::success(vec![Content::text(content)]))
    }
}

#[tool_handler]
impl rmcp::ServerHandler for FetchServer {
    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<GetPromptResult, ErrorData> {
        if request.name != "fetch" {
            return Err(ErrorData::invalid_params(
                format!("Unknown prompt: {}", request.name),
                None,
            ));
        }

        let arguments = request
            .arguments
            .ok_or_else(|| ErrorData::invalid_params("URL is required", None))?;
        let prompt_args: PromptFetchRequest =
            serde_json::from_value(serde_json::Value::Object(arguments))
                .map_err(|err| ErrorData::invalid_params(err.to_string(), None))?;
        let url = validate_url(&prompt_args.url)?;
        let page = fetch_url(
            &url,
            self.config.manual_user_agent(),
            false,
            self.config.proxy_url.as_deref(),
        )
        .await?;

        Ok(GetPromptResult::new(vec![PromptMessage::new_text(
            PromptMessageRole::User,
            format!("{}{}", page.prefix, page.content),
        )])
        .with_description(format!("Contents of {url}")))
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(vec![
            fetch_prompt_definition(),
        ]))
    }

    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.instructions = Some(
            "Fetch MCP server: fetch URLs, return markdown-ish text, and expose a fetch prompt."
                .into(),
        );
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_prompts()
            .build();
        info
    }
}

fn parse_server_config() -> Result<ServerConfig, String> {
    let mut config = ServerConfig::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        if arg == "--ignore-robots-txt" {
            config.ignore_robots_txt = true;
        } else if arg == "--user-agent" {
            let value = args
                .next()
                .ok_or_else(|| "--user-agent requires a value".to_string())?;
            config.custom_user_agent = Some(value);
        } else if arg == "--proxy-url" {
            let value = args
                .next()
                .ok_or_else(|| "--proxy-url requires a value".to_string())?;
            config.proxy_url = Some(value);
        } else if let Some(value) = arg.strip_prefix("--user-agent=") {
            config.custom_user_agent = Some(value.to_string());
        } else if let Some(value) = arg.strip_prefix("--proxy-url=") {
            config.proxy_url = Some(value.to_string());
        } else if arg == "--help" || arg == "-h" {
            return Err(
                "Usage: fetch-mcp-rs [--ignore-robots-txt] [--user-agent VALUE] [--proxy-url VALUE]"
                    .to_string(),
            );
        } else {
            return Err(format!("Unknown argument: {arg}"));
        }
    }

    Ok(config)
}

fn fetch_prompt_definition() -> Prompt {
    Prompt::new(
        "fetch",
        Some("Fetch a URL and extract its contents as markdown"),
        Some(vec![PromptArgument::new("url")
            .with_description("URL to fetch")
            .with_required(true)]),
    )
}

fn parse_fetch_request(request: FetchRequest) -> Result<FetchOptions, ErrorData> {
    let url = validate_url(&request.url)?;
    let max_length = request.max_length.unwrap_or(DEFAULT_MAX_LENGTH);
    if max_length == 0 || max_length >= MAX_ALLOWED_LENGTH {
        return Err(ErrorData::invalid_params(
            format!(
                "max_length must be between 1 and {}",
                MAX_ALLOWED_LENGTH - 1
            ),
            None,
        ));
    }

    Ok(FetchOptions {
        url,
        max_length,
        start_index: request.start_index.unwrap_or(0),
        raw: request.raw.unwrap_or(false),
    })
}

fn validate_url(url: &str) -> Result<String, ErrorData> {
    let parsed = Url::parse(url)
        .map_err(|err| ErrorData::invalid_params(format!("Invalid URL: {err}"), None))?;
    Ok(parsed.to_string())
}

async fn fetch_url(
    url: &str,
    user_agent: &str,
    force_raw: bool,
    proxy_url: Option<&str>,
) -> Result<FetchedPage, ErrorData> {
    let client = build_http_client(user_agent, proxy_url)?;

    let mut response =
        client.get(url).send().await.map_err(|err| {
            ErrorData::internal_error(format!("Failed to fetch {url}: {err}"), None)
        })?;

    if response.status().as_u16() >= 400 {
        return Err(ErrorData::internal_error(
            format!(
                "Failed to fetch {url} - status code {}",
                response.status().as_u16()
            ),
            None,
        ));
    }

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let (page_raw, was_truncated) = read_response_text(&mut response).await?;

    let mut prefix = String::new();
    if was_truncated {
        prefix.push_str(
            "Response exceeded the internal safety download limit and was truncated before processing.\n",
        );
    }

    let is_page_html = page_raw[..page_raw.len().min(100)].contains("<html")
        || content_type.contains("text/html")
        || content_type.is_empty();

    if is_page_html && !force_raw {
        let markdown = simplify_html_to_markdown(&page_raw);
        let content = if markdown.trim().is_empty() {
            "<error>Page failed to be simplified from HTML</error>".to_string()
        } else {
            markdown
        };
        return Ok(FetchedPage { content, prefix });
    }

    let type_label = if content_type.is_empty() {
        "unknown"
    } else {
        &content_type
    };
    prefix.push_str(&format!(
        "Content type {type_label} cannot be simplified to markdown, but here is the raw content:\n"
    ));

    Ok(FetchedPage {
        content: page_raw,
        prefix,
    })
}

fn build_http_client(
    user_agent: &str,
    proxy_url: Option<&str>,
) -> Result<reqwest::Client, ErrorData> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent(user_agent)
        .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS));

    if let Some(proxy_url) = proxy_url {
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|err| ErrorData::invalid_params(format!("Invalid proxy URL: {err}"), None))?;
        builder = builder.proxy(proxy);
    }

    builder
        .build()
        .map_err(|err| ErrorData::internal_error(err.to_string(), None))
}

async fn check_may_autonomously_fetch_url(
    url: &str,
    user_agent: &str,
    proxy_url: Option<&str>,
) -> Result<(), ErrorData> {
    let robots_url = texting_robots::get_robots_url(url).map_err(|err| {
        ErrorData::internal_error(format!("Failed to derive robots.txt URL: {err}"), None)
    })?;
    let client = build_http_client(user_agent, proxy_url)?;

    let mut response = client.get(&robots_url).send().await.map_err(|err| {
        ErrorData::internal_error(
            format!("Failed to fetch robots.txt {robots_url} due to a connection issue: {err}"),
            None,
        )
    })?;

    let status = response.status().as_u16();
    if matches!(status, 401 | 403) {
        return Err(ErrorData::internal_error(
            format!(
                "When fetching robots.txt ({robots_url}), received status {status} so assuming autonomous fetching is not allowed."
            ),
            None,
        ));
    }
    if (400..500).contains(&status) {
        return Ok(());
    }

    let (robots_body, _) = read_response_text(&mut response).await?;
    let robot = texting_robots::Robot::new(user_agent, robots_body.as_bytes()).map_err(|err| {
        ErrorData::internal_error(
            format!("Failed to parse robots.txt {robots_url}: {err}"),
            None,
        )
    })?;

    if !robot.allowed(url) {
        return Err(ErrorData::internal_error(
            format!(
                "The site's robots.txt ({robots_url}) specifies that autonomous fetching is not allowed for {user_agent}: {url}"
            ),
            None,
        ));
    }

    Ok(())
}

async fn read_response_text(response: &mut reqwest::Response) -> Result<(String, bool), ErrorData> {
    let mut bytes = Vec::new();
    let mut was_truncated = false;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|err| ErrorData::internal_error(err.to_string(), None))?
    {
        let remaining = MAX_DOWNLOAD_BYTES.saturating_sub(bytes.len());
        if remaining == 0 {
            was_truncated = true;
            break;
        }

        if chunk.len() > remaining {
            bytes.extend_from_slice(&chunk[..remaining]);
            was_truncated = true;
            break;
        }

        bytes.extend_from_slice(&chunk);
    }

    Ok((String::from_utf8_lossy(&bytes).to_string(), was_truncated))
}

fn simplify_html_to_markdown(html: &str) -> String {
    let focused_html = extract_primary_html_fragment(html).unwrap_or(html);
    let cleaned_html = strip_unwanted_html_blocks(focused_html);
    html2md::parse_html(&cleaned_html)
}

fn extract_primary_html_fragment(html: &str) -> Option<&str> {
    ["main", "article", "body"]
        .iter()
        .find_map(|tag| extract_html_tag_block(html, tag))
}

fn extract_html_tag_block<'a>(html: &'a str, tag: &str) -> Option<&'a str> {
    let lowercase = html.to_ascii_lowercase();
    let open_pattern = format!("<{tag}");
    let close_pattern = format!("</{tag}");
    let start = lowercase.find(&open_pattern)?;
    let mut cursor = start;
    let mut depth = 0usize;

    while cursor < lowercase.len() {
        let next_open = lowercase[cursor..]
            .find(&open_pattern)
            .map(|offset| cursor + offset);
        let next_close = lowercase[cursor..]
            .find(&close_pattern)
            .map(|offset| cursor + offset);

        let next_index = match (next_open, next_close) {
            (Some(open), Some(close)) if open < close => open,
            (Some(_), Some(close)) => close,
            (Some(open), None) => open,
            (None, Some(close)) => close,
            (None, None) => return None,
        };
        let tag_end = lowercase[next_index..].find('>')? + next_index + 1;

        if next_open == Some(next_index) {
            depth += 1;
        } else {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return Some(&html[start..tag_end]);
            }
        }

        cursor = tag_end;
    }

    None
}

fn strip_unwanted_html_blocks(html: &str) -> String {
    let mut cleaned = strip_html_comments(html);
    for tag in [
        "script", "style", "noscript", "svg", "nav", "footer", "aside",
    ] {
        cleaned = strip_html_tag_blocks(&cleaned, tag);
    }
    cleaned
}

fn strip_html_comments(html: &str) -> String {
    let mut cleaned = String::with_capacity(html.len());
    let mut remainder = html;

    while let Some(start) = remainder.find("<!--") {
        cleaned.push_str(&remainder[..start]);
        let Some(end) = remainder[start + 4..].find("-->") else {
            return cleaned;
        };
        remainder = &remainder[start + 4 + end + 3..];
    }

    cleaned.push_str(remainder);
    cleaned
}

fn strip_html_tag_blocks(html: &str, tag: &str) -> String {
    let lowercase = html.to_ascii_lowercase();
    let open_pattern = format!("<{tag}");
    let close_pattern = format!("</{tag}");
    let mut cleaned = String::with_capacity(html.len());
    let mut cursor = 0usize;

    while let Some(relative_start) = lowercase[cursor..].find(&open_pattern) {
        let start = cursor + relative_start;
        cleaned.push_str(&html[cursor..start]);

        let Some(open_end) = lowercase[start..].find('>') else {
            cursor = start;
            break;
        };
        let open_end = start + open_end + 1;
        let open_tag = &lowercase[start..open_end];
        if open_tag.ends_with("/>") {
            cursor = open_end;
            continue;
        }

        let mut scan = open_end;
        let mut depth = 1usize;
        while scan < lowercase.len() {
            let next_open = lowercase[scan..]
                .find(&open_pattern)
                .map(|offset| scan + offset);
            let next_close = lowercase[scan..]
                .find(&close_pattern)
                .map(|offset| scan + offset);

            let next_index = match (next_open, next_close) {
                (Some(open), Some(close)) if open < close => open,
                (Some(_), Some(close)) => close,
                (Some(open), None) => open,
                (None, Some(close)) => close,
                (None, None) => lowercase.len(),
            };

            if next_index == lowercase.len() {
                scan = lowercase.len();
                break;
            }

            let Some(tag_end_rel) = lowercase[next_index..].find('>') else {
                scan = lowercase.len();
                break;
            };
            let tag_end = next_index + tag_end_rel + 1;
            if next_open == Some(next_index) {
                if !lowercase[next_index..tag_end].ends_with("/>") {
                    depth += 1;
                }
            } else {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    scan = tag_end;
                    break;
                }
            }
            scan = tag_end;
        }

        cursor = scan;
    }

    cleaned.push_str(&html[cursor..]);
    cleaned
}

fn paginate_content(
    url: &str,
    content: &str,
    prefix: &str,
    start_index: usize,
    max_length: usize,
) -> String {
    let original_length = content.chars().count();
    let body = if start_index >= original_length {
        "<error>No more content available.</error>".to_string()
    } else {
        let truncated = slice_chars(content, start_index, max_length);
        if truncated.is_empty() {
            "<error>No more content available.</error>".to_string()
        } else {
            let actual_length = truncated.chars().count();
            let mut rendered = truncated;
            let remaining = original_length.saturating_sub(start_index + actual_length);
            if actual_length == max_length && remaining > 0 {
                rendered.push_str(&format!(
                    "\n\n<error>Content truncated. Call the fetch tool with a start_index of {} to get more content.</error>",
                    start_index + actual_length
                ));
            }
            rendered
        }
    };

    format!("{prefix}Contents of {url}:\n{body}")
}

fn slice_chars(input: &str, start: usize, max_length: usize) -> String {
    input.chars().skip(start).take(max_length).collect()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = match parse_server_config() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            return Ok(());
        }
    };

    let _ = fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .with_writer(io::stderr)
        .try_init();

    let service = FetchServer::new(config)
        .serve(stdio())
        .await
        .inspect_err(|e| eprintln!("Error starting server: {e}"))?;

    service.waiting().await?;
    Ok(())
}
