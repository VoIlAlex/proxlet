//! `proxlet mcp`: the MCP server AI tools launch (hidden from help).

use proxlet_core::Locations;

use crate::mcp::{self, tools::ProxletTools};
use crate::output::{CmdResult, Failure};

/// Serves MCP on stdin and stdout until stdin closes.
pub fn run(locations: &Locations) -> CmdResult {
    let tools = ProxletTools::new(locations.clone());
    mcp::serve(std::io::stdin().lock(), std::io::stdout().lock(), &tools)
        .map_err(|e| Failure::new(format!("the MCP connection failed: {e}")))
}
