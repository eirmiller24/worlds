//! The MCP side: an rmcp [`ServerHandler`] that publishes the catalog of
//! `docs/mcp.md` and forwards every call to the [`Engine`], which runs on a
//! thread of its own (its backends are blocking).

use std::sync::{Arc, mpsc};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, InitializeRequestParams,
    InitializeResult, ListResourceTemplatesResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult,
    Resource, ResourceContents, ResourceTemplate, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
use serde_json::Value;

use crate::catalog::{INSTRUCTIONS, TOOLS};
use crate::engine::{Config, Engine};
use crate::resources::TEMPLATES;

type Job = Box<dyn FnOnce(&mut Engine) + Send>;

/// The published tool list, built from the catalog's JSON constants.
pub fn tools() -> Vec<Tool> {
    TOOLS
        .iter()
        .map(|t| {
            let schema: serde_json::Map<String, Value> =
                serde_json::from_str(t.schema).expect("catalog schemas are JSON objects");
            Tool::new(t.name, t.description, Arc::new(schema))
        })
        .collect()
}

/// The MCP server: a handle on the engine thread.
#[derive(Clone)]
pub struct WorldsMcp {
    jobs: mpsc::Sender<Job>,
}

impl WorldsMcp {
    /// Start the engine thread.
    pub fn start(config: Config) -> Self {
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("worlds-mcp-engine".into())
            .spawn(move || {
                let mut engine = Engine::new(config);
                while let Ok(job) = rx.recv() {
                    job(&mut engine);
                }
            })
            .expect("spawn the engine thread");
        WorldsMcp { jobs: tx }
    }

    /// Run `f` on the engine and wait for it.
    pub async fn with_engine<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Engine) -> R + Send + 'static,
    ) -> Result<R, McpError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.jobs
            .send(Box::new(move |e| {
                let _ = tx.send(f(e));
            }))
            .map_err(|_| McpError::internal_error("the worlds-mcp engine stopped", None))?;
        rx.await
            .map_err(|_| McpError::internal_error("the worlds-mcp engine stopped", None))
    }
}

/// The client's name as a request's `_meta` gives it. Clients on the
/// stateless lifecycle (MCP 2026-07-28, which Claude Code uses after
/// `server/discover`) never send `initialize`: `clientInfo` comes with every
/// request instead.
fn meta_client(context: &RequestContext<RoleServer>) -> Option<String> {
    context.meta.client_info().map(|i| i.name)
}

/// Name the engine's client after `name` (from `_meta`), if given.
fn name_client(e: &mut Engine, name: Option<String>) {
    if let Some(n) = name {
        let before = e.client_name().to_string();
        e.set_client_name(&n);
        if e.client_name() != before {
            tracing::info!(client = e.client_name(), "client named by request _meta");
        }
    }
}

impl ServerHandler for WorldsMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_instructions(INSTRUCTIONS)
        .with_server_info(Implementation::new("worlds-mcp", env!("CARGO_PKG_VERSION")))
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, McpError> {
        let name = request.client_info.name.clone();
        self.with_engine(move |e| {
            e.set_client_name(&name);
            tracing::info!(client = e.client_name(), "initialize");
        })
        .await?;
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let name = request.name.to_string();
        let args = request.arguments.map(Value::Object).unwrap_or(Value::Null);
        let client = meta_client(&context);
        let r = self
            .with_engine(move |e| {
                name_client(e, client);
                e.call_tool(&name, args)
            })
            .await?;
        Ok(r.into())
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let client = meta_client(&context);
        let listed = self
            .with_engine(move |e| {
                name_client(e, client);
                e.list_resources()
            })
            .await?
            .map_err(|m| McpError::internal_error(m, None))?;
        Ok(ListResourcesResult::with_all_items(
            listed
                .into_iter()
                .map(|l| Resource::new(l.uri, l.name).with_mime_type("application/json"))
                .collect(),
        ))
    }

    async fn list_resource_templates(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(ListResourceTemplatesResult::with_all_items(
            TEMPLATES
                .iter()
                .map(|(uri, name, desc)| {
                    let mut t = ResourceTemplate::new(*uri, *name);
                    t.description = Some((*desc).to_string());
                    t.mime_type = Some("application/json".into());
                    t
                })
                .collect(),
        ))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = request.uri.clone();
        let client = meta_client(&context);
        let text = self
            .with_engine(move |e| {
                name_client(e, client);
                e.read_resource(&uri)
            })
            .await?
            .map_err(|m| McpError::resource_not_found(m, None))?;
        Ok(
            ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
                uri: request.uri,
                mime_type: Some("application/json".into()),
                text,
                meta: None,
            }])
            .into(),
        )
    }
}
