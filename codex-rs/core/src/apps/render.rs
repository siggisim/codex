use crate::mcp::CODEX_APPS_MCP_SERVER_NAME;

pub(crate) fn render_apps_section() -> String {
    format!(
        "<apps_guidance>\nApps are mentioned in the prompt in the format `[$app-name](app://{{connector_id}})`.\nAn app is equivalent to a set of MCP tools within the `{CODEX_APPS_MCP_SERVER_NAME}` MCP.\nWhen you see an app mention, the app's MCP tools are either already provided in `{CODEX_APPS_MCP_SERVER_NAME}`, or do not exist because the user did not install it.\nDo not additionally call list_mcp_resources for apps that are already mentioned.\n</apps_guidance>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn render_apps_section_uses_xmlish_wrapper() {
        let apps_section = render_apps_section();
        assert!(apps_section.starts_with("<apps_guidance>\n"));
        assert!(apps_section.ends_with("\n</apps_guidance>"));
        assert!(apps_section.contains("Apps are mentioned in the prompt in the format"));
        assert_eq!(apps_section.matches("<apps_guidance>").count(), 1);
        assert_eq!(apps_section.matches("</apps_guidance>").count(), 1);
    }
}
