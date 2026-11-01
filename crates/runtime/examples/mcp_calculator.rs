//! A one-tool MCP server on stdio (`add`), for trying MCP support:
//! `cargo run -q -p hermes_runtime --example mcp_calculator`.

use std::sync::Arc;

use rmcp::ServiceExt as _;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};

#[derive(Clone)]
struct Calculator;

impl ServerHandler for Calculator {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        let Value::Object(schema) = json!({"type": "object", "properties": {
            "a": {"type": "number"}, "b": {"type": "number"}}, "required": ["a", "b"]})
        else {
            unreachable!()
        };
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "add",
            "Add two numbers exactly.",
            Arc::new(schema),
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let args = request.arguments.unwrap_or_default();
        let sum = args["a"].as_f64().unwrap_or(0.0) + args["b"].as_f64().unwrap_or(0.0);
        Ok(CallToolResult::success(vec![ContentBlock::text(sum.to_string())]).into())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let running = Calculator.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}
