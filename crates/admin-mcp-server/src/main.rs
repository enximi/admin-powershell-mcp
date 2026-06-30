use admin_common::{send_request, AdminOperation, AdminRequest};
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router, ServerHandler, ServiceExt,
};
use serde::Deserialize;

#[derive(Clone)]
struct AdminTools {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ReasonOnly {
    reason: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RunCommandArgs {
    command: String,
    cwd: Option<String>,
    timeout_seconds: Option<u64>,
    max_output_bytes: Option<usize>,
    reason: String,
}

#[tool_router]
impl AdminTools {
    fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(description = "Check whether the admin broker is reachable.")]
    fn ping(&self, Parameters(args): Parameters<ReasonOnly>) -> String {
        call(AdminOperation::Ping, args.reason)
    }

    #[tool(description = "Get admin broker status and PowerShell version.")]
    fn get_status(&self, Parameters(args): Parameters<ReasonOnly>) -> String {
        call(AdminOperation::GetStatus, args.reason)
    }

    #[tool(
        description = "Run a Windows PowerShell command with administrator privileges through the local admin broker. Use only when normal shell permissions are insufficient, such as Windows services, firewall/network maintenance, system diagnostics, package maintenance, or machine-level configuration. The command must be allowed by broker policy. Always include a concrete reason. Optional cwd, timeout_seconds, and max_output_bytes control working directory, timeout, and output truncation."
    )]
    fn run_command(&self, Parameters(args): Parameters<RunCommandArgs>) -> String {
        call(
            AdminOperation::RunCommand {
                command: args.command,
                cwd: args.cwd,
                timeout_seconds: args.timeout_seconds,
                max_output_bytes: args.max_output_bytes,
            },
            args.reason,
        )
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for AdminTools {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Windows administrator PowerShell access through a local named-pipe broker. Prefer the normal shell for project-local commands. Use this server only for tasks that need elevated Windows privileges. Commands are policy-gated, timed, logged, and output-limited. Every call must include a concrete reason.")
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    AdminTools::new()
        .serve(rmcp::transport::stdio())
        .await?
        .waiting()
        .await?;
    Ok(())
}

fn call(operation: AdminOperation, reason: String) -> String {
    let req = AdminRequest { operation, reason };
    match send_request(&req) {
        Ok(resp) => format!(
            "ok: {}\nexit_code: {:?}\nstdout:\n{}\nstderr:\n{}",
            resp.ok, resp.exit_code, resp.stdout, resp.stderr
        ),
        Err(err) => format!("broker_error: {err:#}"),
    }
}
